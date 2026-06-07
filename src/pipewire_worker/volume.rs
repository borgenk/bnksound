//! channelVolumes / mute pod writes against a bound Node or its backing
//! Device's Route param. The reads they consult are populated by
//! [`crate::pipewire_worker::node`] and
//! [`crate::pipewire_worker::device`].

use std::io::Cursor;

use libspa::param::ParamType;
use libspa::pod::serialize::PodSerializer;
use libspa::pod::{Object, Pod, Property, PropertyFlags, Value, ValueArray};
use libspa::utils::SpaTypes;
use pipewire as pw;
use pw::device::Device;
use pw::node::Node;

use crate::pipewire_worker::{BoundMap, DeviceMap, Error, Result};

/// Set channelVolumes: hardware sinks via the Device Route param,
/// software-only sinks and app streams via Node Props.
pub(crate) fn apply_volume(
    bound: &BoundMap,
    devices: &DeviceMap,
    node_id: u32,
    volume: f32,
) -> Result<()> {
    let map = bound.borrow();
    let entry = map.get(&node_id).ok_or(Error::UnknownNode(node_id))?;

    // Preserve channel count; default to stereo before Props arrive.
    let channel_count = entry.stream.channel_volumes.len().max(2);
    let linear = volume.max(0.0);

    // Prefer the Device Route param for hardware sinks: it's the path the
    // hardware mixer respects. Node Props layer on top of the route gain,
    // so a 20% slider can come out inaudible.
    if let (Some(dev_id), Some(route_dev)) = (entry.device_id.get(), entry.route_device.get()) {
        let devs = devices.borrow();
        if let Some(dev) = devs.get(&dev_id) {
            let route_index = dev.routes.borrow().get(&route_dev).map(|e| e.index);
            if let Some(idx) = route_index {
                return set_volume_via_route(&dev.proxy, idx, route_dev, linear, channel_count);
            }
        }
    }

    // Software-only sinks and app streams write Props on the Node.
    set_volume_on_node(&entry.proxy, linear, channel_count)
}

fn build_props_object(linear: f32, channels: usize) -> Object {
    Object {
        type_: SpaTypes::ObjectParamProps.as_raw(),
        id: ParamType::Props.as_raw(),
        properties: vec![Property {
            key: libspa_sys::SPA_PROP_channelVolumes,
            flags: PropertyFlags::empty(),
            value: Value::ValueArray(ValueArray::Float(vec![linear; channels])),
        }],
    }
}

fn serialize_object_pod(value: Value) -> Result<Vec<u8>> {
    let (cursor, _) =
        PodSerializer::serialize(Cursor::new(Vec::new()), &value).map_err(|source| {
            Error::Serialize {
                what: "pod",
                source,
            }
        })?;
    Ok(cursor.into_inner())
}

fn set_volume_on_node(node: &Node, linear: f32, channels: usize) -> Result<()> {
    let bytes = serialize_object_pod(Value::Object(build_props_object(linear, channels)))?;
    let pod = Pod::from_bytes(&bytes).ok_or(Error::BuildPod("Props pod"))?;
    node.set_param(ParamType::Props, 0, pod);
    Ok(())
}

/// SetVolume via a Device `Route` param: embeds a channelVolumes Props
/// object in a Route targeting `(index, device)`, propagating to the
/// hardware mixer instead of being a software-only gain.
fn set_volume_via_route(
    device: &Device,
    route_index: u32,
    route_device: i32,
    linear: f32,
    channels: usize,
) -> Result<()> {
    let inner_props = build_props_object(linear, channels);
    let route = Object {
        type_: libspa_sys::SPA_TYPE_OBJECT_ParamRoute,
        id: ParamType::Route.as_raw(),
        properties: vec![
            Property {
                key: libspa_sys::SPA_PARAM_ROUTE_index,
                flags: PropertyFlags::empty(),
                value: Value::Int(route_index as i32),
            },
            Property {
                key: libspa_sys::SPA_PARAM_ROUTE_device,
                flags: PropertyFlags::empty(),
                value: Value::Int(route_device),
            },
            Property {
                key: libspa_sys::SPA_PARAM_ROUTE_props,
                flags: PropertyFlags::empty(),
                value: Value::Object(inner_props),
            },
            // `save=false`: apply without flagging it to WirePlumber's
            // stream-restore as a saved preference (matches pavucontrol).
            Property {
                key: libspa_sys::SPA_PARAM_ROUTE_save,
                flags: PropertyFlags::empty(),
                value: Value::Bool(false),
            },
        ],
    };
    let bytes = serialize_object_pod(Value::Object(route))?;
    let pod = Pod::from_bytes(&bytes).ok_or(Error::BuildPod("Route pod"))?;
    device.set_param(ParamType::Route, 0, pod);
    Ok(())
}

pub(crate) fn apply_mute(bound: &BoundMap, node_id: u32, mute: bool) -> Result<()> {
    let map = bound.borrow();
    let entry = map.get(&node_id).ok_or(Error::UnknownNode(node_id))?;

    let obj = Object {
        type_: SpaTypes::ObjectParamProps.as_raw(),
        id: ParamType::Props.as_raw(),
        properties: vec![Property {
            key: libspa_sys::SPA_PROP_mute,
            flags: PropertyFlags::empty(),
            value: Value::Bool(mute),
        }],
    };
    let bytes = serialize_object_pod(Value::Object(obj))?;
    let pod = Pod::from_bytes(&bytes).ok_or(Error::BuildPod("Props pod"))?;
    entry.proxy.set_param(ParamType::Props, 0, pod);
    Ok(())
}
