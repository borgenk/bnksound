//! Audio Node binding: application playback streams, output sinks, and
//! input sources. `bind_node` owns the Node proxy, its Props/Info
//! listeners, and the per-node level meter; the writes that target a
//! bound Node live in [`crate::pipewire_worker::volume`].

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use libspa::param::ParamType;
use libspa::pod::deserialize::PodDeserializer;
use libspa::pod::{Pod, Value, ValueArray};
use libspa::utils::dict::DictRef;
use pipewire as pw;
use pw::core::CoreRc;
use pw::node::Node;
use pw::proxy::ProxyT;

use crate::bus::Sender as BusSender;
use crate::domain::{Stream, StreamKind};
use crate::meter::PeakPool;
use crate::pipewire_worker::client::apply_client_props_map;
use crate::pipewire_worker::device::recompute_device_forms_for_device;
use crate::pipewire_worker::monitor::start_monitor_stream;
use crate::pipewire_worker::{
    Bound, BoundMap, ClientMap, DefaultNodeName, DeviceMap, Event, MAX_BOUND_NODES,
};

const MEDIA_CLASS_STREAM_OUTPUT_AUDIO: &str = "Stream/Output/Audio";
const MEDIA_CLASS_AUDIO_SINK: &str = "Audio/Sink";
const MEDIA_CLASS_AUDIO_SOURCE: &str = "Audio/Source";

/// Device display name from a Node prop dict: `node.description`, then
/// `node.nick`, then `node.name`. `None` when all are empty (bluetooth
/// lands with a placeholder, the real name arrives via the info event).
fn resolve_device_name(props: &DictRef) -> Option<String> {
    props
        .get("node.description")
        .or_else(|| props.get("node.nick"))
        .or_else(|| props.get("node.name"))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// A sink's monitor port surfaces as an `Audio/Source` whose `node.name`
/// ends in `.monitor`. Those are loopbacks, not real capture hardware, so
/// exclude them from the input list.
fn is_monitor_source(props: &DictRef) -> bool {
    props
        .get("node.name")
        .is_some_and(|n| n.ends_with(".monitor"))
}

fn parse_props(pod: &Pod) -> Option<(Option<Vec<f32>>, Option<bool>)> {
    let (_, value) = PodDeserializer::deserialize_from::<Value>(pod.as_bytes()).ok()?;
    let Value::Object(obj) = value else {
        return None;
    };

    let mut volumes: Option<Vec<f32>> = None;
    let mut mute: Option<bool> = None;

    for prop in obj.properties {
        if prop.key == libspa_sys::SPA_PROP_channelVolumes
            && let Value::ValueArray(ValueArray::Float(v)) = prop.value
        {
            volumes = Some(v);
        } else if prop.key == libspa_sys::SPA_PROP_mute
            && let Value::Bool(b) = prop.value
        {
            mute = Some(b);
        }
    }

    Some((volumes, mute))
}

/// Bind an audio Node global: classify by `media.class`, build its
/// [`Stream`], register the Props/Info listeners, spawn the meter, and
/// insert into `bound`. Non-audio Nodes and sink monitors are ignored.
#[allow(clippy::too_many_arguments)]
pub(crate) fn bind_node(
    registry: &pw::registry::RegistryRc,
    obj: &pw::registry::GlobalObject<&DictRef>,
    bound: BoundMap,
    clients: ClientMap,
    devices: DeviceMap,
    default_name: DefaultNodeName,
    default_source_name: DefaultNodeName,
    core: &CoreRc,
    pool: &Arc<PeakPool>,
    evt_tx: BusSender<Event>,
) {
    let props = match obj.props {
        Some(p) => p,
        None => return,
    };
    let kind = match props.get("media.class") {
        Some(MEDIA_CLASS_STREAM_OUTPUT_AUDIO) => StreamKind::Application,
        Some(MEDIA_CLASS_AUDIO_SINK) => StreamKind::Sink,
        Some(MEDIA_CLASS_AUDIO_SOURCE) => StreamKind::Source,
        _ => return,
    };
    // Skip sink monitors masquerading as Audio/Source.
    if matches!(kind, StreamKind::Source) && is_monitor_source(props) {
        return;
    }

    // Apps publish application.name; sinks set it to "WirePlumber"
    // (useless as a label), so use the device description instead.
    let name = match kind {
        StreamKind::Application => props
            .get("application.name")
            .or_else(|| props.get("node.description"))
            .or_else(|| props.get("node.name"))
            .unwrap_or("(unnamed)")
            .to_string(),
        StreamKind::Sink | StreamKind::Source => {
            resolve_device_name(props).unwrap_or_else(|| "(unnamed device)".to_string())
        }
    };

    let node: Node = match registry.bind(obj) {
        Ok(n) => n,
        Err(e) => {
            let _ = evt_tx.send(Event::Error(format!("bind node {}: {e}", obj.id)));
            return;
        }
    };

    let app_id = props.get("application.id").map(str::to_string);
    let binary = props.get("application.process.binary").map(str::to_string);
    // Unique per object; the only differentiator when app streams share a
    // node.name (every Chromium browser tab does).
    let object_serial = props.get("object.serial").map(str::to_string);

    // XDG lookup only applies to app streams; sinks are devices.
    let xdg = match kind {
        StreamKind::Application => crate::xdg::lookup(&crate::xdg::Hints {
            app_id: app_id.as_deref(),
            portal_app_id: props.get("pipewire.access.portal.app_id"),
            binary: binary.as_deref(),
            wm_class: props.get("window.x11.wm_class"),
            app_name: props.get("application.name"),
        }),
        StreamKind::Sink | StreamKind::Source => None,
    };

    let initial = Stream {
        id: obj.id,
        kind,
        name,
        app_id,
        binary,
        pid: props.get("application.process.id").map(str::to_string),
        node_name: props.get("node.name").map(str::to_string),
        media_name: props.get("media.name").map(str::to_string),
        media_role: props.get("media.role").map(str::to_string),
        channel_volumes: Vec::new(),
        muted: false,
        xdg,
        // Resolved later by `recompute_device_forms_for_device` once the
        // Device info + routes arrive.
        form: None,
        is_default: false,
        // Metadata-driven; the default-metadata listener back-fills it.
        target_sink_name: None,
    };

    // Props listener mirrors live volume/mute; Info listener backfills
    // `device.id` / `card.profile.device` (needed by the Route-volume
    // path) and the finalized `node.description`, then kicks a form
    // recompute once device_id lands.
    let bound_for_info = Rc::clone(&bound);
    let devices_for_info = Rc::clone(&devices);
    let evt_for_info = evt_tx.clone();
    let bound_for_param = Rc::clone(&bound);
    let evt_for_param = evt_tx.clone();
    let node_id = obj.id;
    let node_listener = node
        .add_listener_local()
        .info(move |info| {
            let Some(props) = info.props() else { return };
            let dev_id = props.get("device.id").and_then(|s| s.parse::<u32>().ok());
            let rdev = props
                .get("card.profile.device")
                .and_then(|s| s.parse::<i32>().ok());
            let new_device_name = resolve_device_name(props);

            let (dev_id_for_recompute, updated_stream) = {
                let mut map = bound_for_info.borrow_mut();
                let Some(b) = map.get_mut(&node_id) else {
                    return;
                };
                if let Some(d) = dev_id {
                    b.device_id.set(Some(d));
                }
                if let Some(r) = rdev {
                    b.route_device.set(Some(r));
                }
                let updated = if matches!(b.stream.kind, StreamKind::Sink | StreamKind::Source)
                    && let Some(name) = new_device_name
                    && b.stream.name != name
                {
                    b.stream.name = name;
                    Some(b.stream.clone())
                } else {
                    None
                };
                (b.device_id.get(), updated)
            };

            if let Some(stream) = updated_stream {
                let _ = evt_for_info.send(Event::StreamUpdated(stream));
            }

            if let Some(dev_id) = dev_id_for_recompute {
                recompute_device_forms_for_device(
                    &bound_for_info,
                    &devices_for_info,
                    dev_id,
                    &evt_for_info,
                );
            }
        })
        .param(move |_seq, id, _index, _next, param| {
            if id != ParamType::Props {
                return;
            }
            let Some(pod) = param else { return };
            let Some((volumes, mute)) = parse_props(pod) else {
                return;
            };

            let mut map = bound_for_param.borrow_mut();
            let Some(entry) = map.get_mut(&node_id) else {
                return;
            };
            let mut changed = false;
            if let Some(v) = volumes
                && entry.stream.channel_volumes != v
            {
                entry.stream.channel_volumes = v;
                changed = true;
            }
            if let Some(m) = mute
                && entry.stream.muted != m
            {
                entry.stream.muted = m;
                changed = true;
            }
            if changed {
                let _ = evt_for_param.send(Event::StreamUpdated(entry.stream.clone()));
            }
        })
        .register();

    // Ask the server to push the current Props and any future changes.
    node.subscribe_params(&[ParamType::Props]);

    // Capture-safe: the map removal MUST be the last statement here.
    // Removing the `Bound` drops `_proxy_listener`, which drops this
    // closure's captures, so any access after it is use-after-free.
    let bound_for_remove = Rc::clone(&bound);
    let evt_for_remove = evt_tx.clone();
    let proxy_listener = node
        .upcast_ref()
        .add_listener_local()
        .removed(move || {
            let _ = evt_for_remove.send(Event::StreamRemoved(node_id));
            // Last statement: drops the Bound (and this closure). No
            // capture access past this point.
            bound_for_remove.borrow_mut().remove(&node_id);
        })
        .register();

    let client_id = props.get("client.id").and_then(|s| s.parse::<u32>().ok());

    // Apply already-arrived Client info so the first StreamAdded has the
    // resolved name/icon. App-only guard: WirePlumber's application.name
    // would clobber a sink's real name (same as the bind_client listener).
    let mut stream_for_insert = initial.clone();
    if matches!(initial.kind, StreamKind::Application)
        && let Some(cid) = client_id
        && let Some(client_props) = clients.borrow().get(&cid).map(|c| c.props.clone())
        && !client_props.is_empty()
    {
        apply_client_props_map(&mut stream_for_insert, &client_props);
    }

    // Best-effort prefill; often `None` for sinks until the info listener
    // patches them.
    let initial_device_id = props.get("device.id").and_then(|s| s.parse::<u32>().ok());
    let initial_route_device = props
        .get("card.profile.device")
        .and_then(|s| s.parse::<i32>().ok());

    // Mark default at insert if the metadata already landed; otherwise
    // `refresh_default_marks` handles it later.
    let default_cache = match kind {
        StreamKind::Sink => Some(&default_name),
        StreamKind::Source => Some(&default_source_name),
        StreamKind::Application => None,
    };
    if let Some(cache) = default_cache
        && let Some(dn) = cache.borrow().as_deref()
        && stream_for_insert.node_name.as_deref() == Some(dn)
    {
        stream_for_insert.is_default = true;
    }

    // Capture stream for the level meter (props adjusted per kind).
    let monitor = start_monitor_stream(
        core,
        node_id,
        kind,
        object_serial.as_deref(),
        stream_for_insert.node_name.as_deref(),
        evt_tx.clone(),
        pool,
    );

    {
        let mut bm = bound.borrow_mut();
        bm.insert(
            node_id,
            Bound {
                proxy: node,
                stream: stream_for_insert.clone(),
                client_id,
                device_id: Cell::new(initial_device_id),
                route_device: Cell::new(initial_route_device),
                _node_listener: node_listener,
                _proxy_listener: proxy_listener,
                _monitor: monitor,
            },
        );
        assert!(
            bm.len() <= MAX_BOUND_NODES,
            "bound nodes exceeded cap ({}), runaway leak suspected",
            bm.len()
        );
    }

    let _ = evt_tx.send(Event::StreamAdded(stream_for_insert));
}
