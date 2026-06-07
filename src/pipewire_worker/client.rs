//! Per-Client prop enrichment. Some apps (Spotify, for one) leave the
//! identifying `application.*` props off their Node and only set them on
//! the owning Client, so we bind every Client and fold its props onto the
//! matching application Streams.

use std::collections::BTreeMap;
use std::rc::Rc;

use libspa::utils::dict::DictRef;
use pipewire as pw;
use pw::client::{Client, ClientListener};

use crate::bus::Sender as BusSender;
use crate::domain::{Stream, StreamKind};
use crate::pipewire_worker::{BoundMap, ClientMap, Event, MAX_BOUND_CLIENTS};

/// Per-client bookkeeping for the enrichment cache.
pub(crate) struct BoundClient {
    _proxy: Client,
    _listener: ClientListener,
    /// Full prop dict from the latest info event; empty until that arrives.
    pub(crate) props: BTreeMap<String, String>,
}

/// Bind a Client global and store it; when info arrives, enrich any bound
/// nodes whose `client.id` matches.
pub(crate) fn bind_client(
    registry: &pw::registry::RegistryRc,
    obj: &pw::registry::GlobalObject<&DictRef>,
    clients: ClientMap,
    nodes: BoundMap,
    evt_tx: BusSender<Event>,
) {
    let client: Client = match registry.bind(obj) {
        Ok(c) => c,
        Err(e) => {
            let _ = evt_tx.send(Event::Error(format!("bind client {}: {e}", obj.id)));
            return;
        }
    };

    let client_id = obj.id;
    let clients_for_info = Rc::clone(&clients);
    let listener = client
        .add_listener_local()
        .info(move |info| {
            let Some(dict) = info.props() else { return };
            let snapshot: BTreeMap<String, String> = dict
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();

            if let Some(entry) = clients_for_info.borrow_mut().get_mut(&client_id) {
                entry.props = snapshot.clone();
            }

            let mut node_map = nodes.borrow_mut();
            for bound in node_map.values_mut() {
                // App streams only: WirePlumber owns sinks, so its
                // application.name would relabel every sink "WirePlumber".
                if bound.client_id == Some(client_id)
                    && matches!(bound.stream.kind, StreamKind::Application)
                    && apply_client_props_map(&mut bound.stream, &snapshot)
                {
                    let _ = evt_tx.send(Event::StreamUpdated(bound.stream.clone()));
                }
            }
        })
        .register();

    {
        let mut cm = clients.borrow_mut();
        cm.insert(
            client_id,
            BoundClient {
                _proxy: client,
                _listener: listener,
                props: BTreeMap::new(),
            },
        );
        assert!(
            cm.len() <= MAX_BOUND_CLIENTS,
            "bound clients exceeded cap ({}), runaway leak suspected",
            cm.len()
        );
    }
}

/// Merge Client props into a Stream and re-run XDG lookup. Returns `true` if
/// anything actually changed.
pub(crate) fn apply_client_props_map(
    stream: &mut Stream,
    props: &BTreeMap<String, String>,
) -> bool {
    let mut changed = false;

    let merge = |slot: &mut Option<String>, key: &str, changed: &mut bool| {
        if let Some(v) = props.get(key)
            && slot.as_deref() != Some(v.as_str())
        {
            *slot = Some(v.clone());
            *changed = true;
        }
    };

    merge(&mut stream.app_id, "application.id", &mut changed);
    merge(
        &mut stream.binary,
        "application.process.binary",
        &mut changed,
    );
    merge(&mut stream.pid, "application.process.id", &mut changed);

    // Upgrade "audio-src" to the Client's application.name (e.g. "spotify").
    if let Some(app_name) = props.get("application.name")
        && stream.name.as_str() != app_name.as_str()
    {
        stream.name = app_name.clone();
        changed = true;
    }

    if matches!(stream.kind, StreamKind::Application) {
        let new_xdg = crate::xdg::lookup(&crate::xdg::Hints {
            app_id: stream.app_id.as_deref(),
            portal_app_id: props
                .get("pipewire.access.portal.app_id")
                .map(String::as_str),
            binary: stream.binary.as_deref(),
            wm_class: props.get("window.x11.wm_class").map(String::as_str),
            app_name: props.get("application.name").map(String::as_str),
        });
        if stream.xdg != new_xdg {
            stream.xdg = new_xdg;
            changed = true;
        }
    }

    changed
}
