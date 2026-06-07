//! Backing `Device` binding: the Route catalog (the `(index, device)`
//! pair the hardware mixer respects) and form-factor resolution for the
//! row icon/sort. The writes that consume the catalog live in
//! [`crate::pipewire_worker::volume`].

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use libspa::param::ParamType;
use libspa::pod::deserialize::PodDeserializer;
use libspa::pod::{Pod, Value, ValueArray};
use libspa::utils::dict::DictRef;
use pipewire as pw;
use pw::core::CoreRc;
use pw::device::{Device, DeviceListener};

use crate::bus::Sender as BusSender;
use crate::domain::{DeviceForm, SinkForm, SourceForm, StreamKind};
use crate::pipewire_worker::{BoundMap, DeviceMap, Event};

/// Per-route info on a `BoundDevice`. `index` goes in the Route-write
/// pod; `port_type` (e.g. `"speaker"`/`"hdmi"`) drives form detection for
/// cards lacking `device.form-factor`.
#[derive(Debug, Clone, Default)]
pub(crate) struct RouteEntry {
    pub(crate) index: u32,
    port_type: Option<String>,
}

/// Bound `Device` proxy + Route listener. `routes` is keyed by the Route
/// `device` sub-id (matches a sink Node's `card.profile.device`); `props`
/// mirrors the latest Device-info dict.
pub(crate) struct BoundDevice {
    pub(crate) proxy: Device,
    _listener: DeviceListener,
    pub(crate) routes: Rc<RefCell<HashMap<i32, RouteEntry>>>,
    props: Rc<RefCell<BTreeMap<String, String>>>,
}

/// Bind a Device global, subscribe + enumerate its routes, capture its
/// info dict, and store it. Routes and props populate incrementally from
/// the listeners.
pub(crate) fn bind_device(
    registry: &pw::registry::RegistryRc,
    obj: &pw::registry::GlobalObject<&DictRef>,
    devices: DeviceMap,
    nodes: BoundMap,
    core: &CoreRc,
    evt_tx: BusSender<Event>,
) {
    let device: Device = match registry.bind(obj) {
        Ok(d) => d,
        Err(e) => {
            let _ = evt_tx.send(Event::Error(format!("bind device {}: {e}", obj.id)));
            return;
        }
    };

    let device_id = obj.id;
    let routes: Rc<RefCell<HashMap<i32, RouteEntry>>> = Rc::new(RefCell::new(HashMap::new()));
    let props: Rc<RefCell<BTreeMap<String, String>>> = Rc::new(RefCell::new(BTreeMap::new()));

    let routes_for_param = Rc::clone(&routes);
    let nodes_for_param = Rc::clone(&nodes);
    let devices_for_param = Rc::clone(&devices);
    let evt_for_param = evt_tx.clone();
    let props_for_info = Rc::clone(&props);
    let nodes_for_info = Rc::clone(&nodes);
    let devices_for_info = Rc::clone(&devices);
    let evt_for_info = evt_tx.clone();

    let listener = device
        .add_listener_local()
        .info(move |info| {
            let Some(dict) = info.props() else { return };
            let snapshot: BTreeMap<String, String> = dict
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            *props_for_info.borrow_mut() = snapshot;
            recompute_device_forms_for_device(
                &nodes_for_info,
                &devices_for_info,
                device_id,
                &evt_for_info,
            );
        })
        .param(move |_seq, param_id, _index, _next, pod| {
            if param_id != ParamType::EnumRoute && param_id != ParamType::Route {
                return;
            }
            let Some(pod) = pod else { return };
            {
                let mut map = routes_for_param.borrow_mut();
                for (entry, route_dev) in parse_route_entries(pod) {
                    // Merge: an active-Route event may lack port.type;
                    // don't clobber one learned from EnumRoute.
                    let slot = map.entry(route_dev).or_default();
                    slot.index = entry.index;
                    if entry.port_type.is_some() {
                        slot.port_type = entry.port_type.clone();
                    }
                }
            }
            recompute_device_forms_for_device(
                &nodes_for_param,
                &devices_for_param,
                device_id,
                &evt_for_param,
            );
        })
        .register();

    // Subscribe + explicitly enumerate: `subscribe_params` alone doesn't
    // always push existing routes. `core.sync` flushes the responses.
    device.subscribe_params(&[ParamType::EnumRoute, ParamType::Route]);
    device.enum_params(1, Some(ParamType::EnumRoute), 0, u32::MAX);
    device.enum_params(2, Some(ParamType::Route), 0, u32::MAX);
    if let Err(e) = core.sync(0) {
        let _ = evt_tx.send(Event::Error(format!("device {device_id} core.sync: {e}")));
    }

    devices.borrow_mut().insert(
        device_id,
        BoundDevice {
            proxy: device,
            _listener: listener,
            routes,
            props,
        },
    );
}

/// Recompute the form of every sink/source Node backed by `device_id`
/// and emit `StreamUpdated` when it moved. App streams are skipped.
pub(crate) fn recompute_device_forms_for_device(
    nodes: &BoundMap,
    devices: &DeviceMap,
    device_id: u32,
    evt_tx: &BusSender<Event>,
) {
    let mut node_map = nodes.borrow_mut();
    for bound in node_map.values_mut() {
        let kind = bound.stream.kind;
        if !matches!(kind, StreamKind::Sink | StreamKind::Source) {
            continue;
        }
        if bound.device_id.get() != Some(device_id) {
            continue;
        }
        // Form is sticky-on: once resolved, never unset, so a transient
        // empty Device-info/Route event can't blank the icon mid-session.
        let resolved = match kind {
            StreamKind::Sink => {
                resolve_sink_form(bound.device_id.get(), bound.route_device.get(), devices)
                    .map(DeviceForm::Output)
            }
            StreamKind::Source => {
                resolve_source_form(bound.device_id.get(), bound.route_device.get(), devices)
                    .map(DeviceForm::Input)
            }
            StreamKind::Application => None,
        };
        let new_form = resolved.or(bound.stream.form);
        if bound.stream.form != new_form {
            bound.stream.form = new_form;
            let _ = evt_tx.send(Event::StreamUpdated(bound.stream.clone()));
        }
    }
}

/// Resolution ladder: explicit `device.form-factor` → active route's
/// `port.type` → bluez5 inference → Generic.
fn resolve_sink_form(
    device_id: Option<u32>,
    route_device: Option<i32>,
    devices: &DeviceMap,
) -> Option<SinkForm> {
    let dev_id = device_id?;
    let devs = devices.borrow();
    let dev = devs.get(&dev_id)?;

    if let Some(form) = dev
        .props
        .borrow()
        .get("device.form-factor")
        .and_then(|ff| SinkForm::from_form_factor(ff))
    {
        return Some(form);
    }

    if let Some(rdev) = route_device
        && let Some(form) = dev
            .routes
            .borrow()
            .get(&rdev)
            .and_then(|entry| entry.port_type.as_deref())
            .and_then(SinkForm::from_port_type)
    {
        return Some(form);
    }

    if dev.props.borrow().get("device.api").map(String::as_str) == Some("bluez5") {
        return Some(SinkForm::Headset);
    }

    Some(SinkForm::Generic)
}

/// Capture-side mirror of [`resolve_sink_form`]. Resolution ladder:
/// explicit `device.form-factor` → active capture route's `port.type` →
/// bluez5 inference (a bluetooth capture device is a mic) → Generic.
fn resolve_source_form(
    device_id: Option<u32>,
    route_device: Option<i32>,
    devices: &DeviceMap,
) -> Option<SourceForm> {
    let dev_id = device_id?;
    let devs = devices.borrow();
    let dev = devs.get(&dev_id)?;

    if let Some(form) = dev
        .props
        .borrow()
        .get("device.form-factor")
        .and_then(|ff| SourceForm::from_form_factor(ff))
    {
        return Some(form);
    }

    if let Some(rdev) = route_device
        && let Some(form) = dev
            .routes
            .borrow()
            .get(&rdev)
            .and_then(|entry| entry.port_type.as_deref())
            .and_then(SourceForm::from_port_type)
    {
        return Some(form);
    }

    if dev.props.borrow().get("device.api").map(String::as_str) == Some("bluez5") {
        return Some(SourceForm::Microphone);
    }

    Some(SourceForm::Generic)
}

/// Extract a route's `(RouteEntry, device sub-id)` pairs from a Route /
/// EnumRoute pod. EnumRoute carries the full `devices` array; the active
/// Route usually carries a singular `device`. Both shapes are handled.
fn parse_route_entries(pod: &Pod) -> Vec<(RouteEntry, i32)> {
    let bytes = pod.as_bytes();
    let Ok((_, value)) = PodDeserializer::deserialize_from::<Value>(bytes) else {
        return Vec::new();
    };
    let Value::Object(obj) = value else {
        return Vec::new();
    };
    if obj.type_ != libspa_sys::SPA_TYPE_OBJECT_ParamRoute {
        return Vec::new();
    }
    let mut index = None;
    let mut devices = Vec::new();
    let mut port_type: Option<String> = None;
    for prop in &obj.properties {
        match prop.key {
            libspa_sys::SPA_PARAM_ROUTE_index => {
                if let Value::Int(v) = &prop.value
                    && *v >= 0
                {
                    index = Some(*v as u32);
                }
            }
            libspa_sys::SPA_PARAM_ROUTE_device => {
                if let Value::Int(v) = &prop.value {
                    devices.push(*v);
                }
            }
            libspa_sys::SPA_PARAM_ROUTE_devices => {
                if let Value::ValueArray(ValueArray::Int(values)) = &prop.value {
                    devices.extend(values.iter().copied());
                }
            }
            libspa_sys::SPA_PARAM_ROUTE_info => {
                if let Value::Struct(items) = &prop.value {
                    port_type = port_type_from_info_struct(items);
                }
            }
            _ => {}
        }
    }
    let Some(index) = index else {
        return Vec::new();
    };
    let entry = RouteEntry { index, port_type };
    devices
        .into_iter()
        .map(|dev| (entry.clone(), dev))
        .collect()
}

/// SPA encodes the route `info` field as a Struct: `Int(n_items)`
/// followed by `n` `(key, value)` string pairs.
fn port_type_from_info_struct(items: &[Value]) -> Option<String> {
    let mut iter = items.iter();
    iter.next(); // leading n_items count
    while let Some(k) = iter.next() {
        let v = iter.next()?;
        if let (Value::String(key), Value::String(val)) = (k, v)
            && key == "port.type"
        {
            return Some(val.clone());
        }
    }
    None
}
