//! The `default` Metadata global: the system default sink/source and
//! per-stream `target.object` routing. Binding it drives `is_default`
//! and `target_sink_name` on the matching Streams; the apply functions
//! write the metadata back when the user picks a default or pins a
//! stream.

use std::rc::Rc;

use libspa::utils::dict::DictRef;
use pipewire as pw;
use pw::metadata::{Metadata, MetadataListener};

use crate::bus::Sender as BusSender;
use crate::domain::StreamKind;
use crate::pipewire_worker::{BoundMap, DefaultMetaSlot, DefaultNodeName, Error, Event, Result};

/// The bound `default` metadata proxy + listener (at most one globally).
/// `proxy` is reused for writes; the listener gives the readback.
pub(crate) struct DefaultMeta {
    pub(crate) id: u32,
    proxy: Metadata,
    _listener: MetadataListener,
}

/// Bind the `default` Metadata global, tracking `default.audio.sink` /
/// `default.audio.source` changes onto `is_default`. Other metadata
/// globals are ignored.
pub(crate) fn bind_default_meta(
    registry: &pw::registry::RegistryRc,
    obj: &pw::registry::GlobalObject<&DictRef>,
    nodes: BoundMap,
    default_name: DefaultNodeName,
    default_source_name: DefaultNodeName,
    slot: DefaultMetaSlot,
    evt_tx: BusSender<Event>,
) {
    let is_default = obj
        .props
        .as_ref()
        .and_then(|d| d.get("metadata.name"))
        .is_some_and(|n| n == "default");
    if !is_default {
        return;
    }
    let proxy: Metadata = match registry.bind(obj) {
        Ok(m) => m,
        Err(e) => {
            let _ = evt_tx.send(Event::Error(format!(
                "bind default metadata {}: {e}",
                obj.id
            )));
            return;
        }
    };

    let nodes_for_prop = Rc::clone(&nodes);
    let default_name_for_prop = Rc::clone(&default_name);
    let default_source_name_for_prop = Rc::clone(&default_source_name);
    let evt_for_prop = evt_tx.clone();
    let listener = proxy
        .add_listener_local()
        .property(move |subject, key, _type_, value| {
            if key == Some("default.audio.sink") {
                let parsed = value.and_then(parse_default_node_value);
                *default_name_for_prop.borrow_mut() = parsed.clone();
                refresh_default_marks(&nodes_for_prop, StreamKind::Sink, &parsed, &evt_for_prop);
            }
            if key == Some("default.audio.source") {
                let parsed = value.and_then(parse_default_node_value);
                *default_source_name_for_prop.borrow_mut() = parsed.clone();
                refresh_default_marks(&nodes_for_prop, StreamKind::Source, &parsed, &evt_for_prop);
            }
            // Per-stream routing: subject 0 is global keys, non-zero is a
            // node id. `value == None` (clear) resets the row to "follow
            // default".
            if subject != 0 && key == Some("target.object") {
                let resolved = value.and_then(parse_stream_target_value);
                apply_stream_target_to_row(&nodes_for_prop, subject, resolved, &evt_for_prop);
            }
            0
        })
        .register();

    *slot.borrow_mut() = Some(DefaultMeta {
        id: obj.id,
        proxy,
        _listener: listener,
    });
}

/// Flip `is_default` on every bound Node of `kind` to match
/// `default_name` (by `node.name`), emitting `StreamUpdated` only for
/// rows that moved. Sink and Source track independent default names.
pub(crate) fn refresh_default_marks(
    nodes: &BoundMap,
    kind: StreamKind,
    default_name: &Option<String>,
    evt_tx: &BusSender<Event>,
) {
    let mut node_map = nodes.borrow_mut();
    for bound in node_map.values_mut() {
        if bound.stream.kind != kind {
            continue;
        }
        let now_default = match (default_name.as_deref(), bound.stream.node_name.as_deref()) {
            (Some(want), Some(have)) => want == have,
            _ => false,
        };
        if bound.stream.is_default != now_default {
            bound.stream.is_default = now_default;
            let _ = evt_tx.send(Event::StreamUpdated(bound.stream.clone()));
        }
    }
}

/// Write a default-node `name` to BOTH the `configured` and live keys.
/// Must write both: WirePlumber's rescan (fires on any Route param
/// change) reads `configured.*`, so a live-only write gets reverted.
fn write_default_meta(
    meta_slot: &DefaultMetaSlot,
    configured_key: &str,
    live_key: &str,
    name: &str,
) -> Result<()> {
    let slot = meta_slot.borrow();
    let meta = slot.as_ref().ok_or(Error::DefaultMetaNotBound)?;
    let json = format!(r#"{{"name":"{}"}}"#, escape_json(name));
    for key in [configured_key, live_key] {
        meta.proxy
            .set_property(0, key, Some("Spa:String:JSON"), Some(&json));
    }
    Ok(())
}

/// Mark `name` as the system default for its direction, then update the
/// local `cache` and emit `StreamUpdated` so the UI doesn't wait for
/// PipeWire's echo. `Application` is a no-op.
pub(crate) fn apply_default(
    meta_slot: &DefaultMetaSlot,
    cache: &DefaultNodeName,
    nodes: &BoundMap,
    kind: StreamKind,
    name: &str,
    evt_tx: &BusSender<Event>,
) -> Result<()> {
    let (configured_key, live_key) = match kind {
        StreamKind::Sink => ("default.configured.audio.sink", "default.audio.sink"),
        StreamKind::Source => ("default.configured.audio.source", "default.audio.source"),
        StreamKind::Application => return Ok(()),
    };
    write_default_meta(meta_slot, configured_key, live_key, name)?;

    let new_name = Some(name.to_string());
    *cache.borrow_mut() = new_name.clone();
    refresh_default_marks(nodes, kind, &new_name, evt_tx);
    Ok(())
}

/// Write a per-stream `target.object` (`Some` pins to that sink, `None`
/// clears). No local cache update: the metadata listener echoes the
/// write back, so we and other clients converge on the same path.
pub(crate) fn apply_stream_target(
    meta_slot: &DefaultMetaSlot,
    node_id: u32,
    sink_node_name: Option<&str>,
) -> Result<()> {
    let slot = meta_slot.borrow();
    let meta = slot.as_ref().ok_or(Error::DefaultMetaNotBound)?;
    match sink_node_name {
        Some(name) => {
            // `Spa:String` (the bare form pw-metadata writes), not JSON.
            meta.proxy
                .set_property(node_id, "target.object", Some("Spa:String"), Some(name));
        }
        None => {
            // Clear the override; the stream re-routes to the default sink.
            meta.proxy
                .set_property(node_id, "target.object", None, None);
        }
    }
    Ok(())
}

/// Escape `"` and `\` for embedding in JSON. node.name values are
/// already safe (`[A-Za-z0-9_.-]`); belt-and-braces for exotic callers.
fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(ch),
        }
    }
    out
}

/// Mirror a `target.object` change onto the Stream's `target_sink_name`,
/// emitting `StreamUpdated` only when it moved (so our own echo doesn't
/// fan out a redundant update).
fn apply_stream_target_to_row(
    nodes: &BoundMap,
    node_id: u32,
    sink_name: Option<String>,
    evt_tx: &BusSender<Event>,
) {
    let mut map = nodes.borrow_mut();
    let Some(bound) = map.get_mut(&node_id) else {
        return;
    };
    if !matches!(bound.stream.kind, StreamKind::Application) {
        return;
    }
    if bound.stream.target_sink_name == sink_name {
        return;
    }
    bound.stream.target_sink_name = sink_name;
    let _ = evt_tx.send(Event::StreamUpdated(bound.stream.clone()));
}

/// Decode a `target.object` value (bare node name, numeric
/// object.serial, or `{"name":"<n>"}` JSON) to the node name, or `None`
/// for a bare serial (we keep no serial → name index).
fn parse_stream_target_value(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if let Some(name) = parse_default_node_value(trimmed) {
        return Some(name);
    }
    // Bare numeric is an object.serial; we have no name for it.
    if trimmed.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    // node.name is `[A-Za-z0-9_.-]`; reject anything else as malformed.
    if !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    {
        return Some(trimmed.to_string());
    }
    None
}

/// Extract `name` from a `{"name":"<node>"}` value.
fn parse_default_node_value(raw: &str) -> Option<String> {
    let key = "\"name\"";
    let key_start = raw.find(key)?;
    let after = &raw[key_start + key.len()..];
    let colon = after.find(':')?;
    let after = &after[colon + 1..];
    let q1 = after.find('"')?;
    let after = &after[q1 + 1..];
    let q2 = after.find('"')?;
    Some(after[..q2].to_string())
}

#[cfg(test)]
mod tests {
    use crate::pipewire_worker::default_meta::parse_stream_target_value;

    #[test]
    fn parse_stream_target_value_accepts_json_name() {
        // WirePlumber stream-restore JSON form.
        assert_eq!(
            parse_stream_target_value(r#"{"name":"alsa_output.usb"}"#).as_deref(),
            Some("alsa_output.usb"),
        );
    }

    #[test]
    fn parse_stream_target_value_accepts_bare_name() {
        // Bare form `pw-metadata` writes.
        assert_eq!(
            parse_stream_target_value("bluez_output.headset").as_deref(),
            Some("bluez_output.headset"),
        );
    }

    #[test]
    fn parse_stream_target_value_drops_bare_numeric_serial() {
        // No serial → name index, so a bare serial resolves to None.
        assert_eq!(parse_stream_target_value("12345"), None);
    }

    #[test]
    fn parse_stream_target_value_rejects_malformed_string() {
        // Chars outside `[A-Za-z0-9_.-]` aren't valid node.names.
        assert_eq!(parse_stream_target_value("hey there"), None);
        assert_eq!(parse_stream_target_value("garbage{value"), None);
        assert_eq!(parse_stream_target_value(""), None);
    }
}
