//! Dedicated PipeWire thread.
//!
//! ```text
//!     main thread                 pipewire thread
//!     ───────────                 ───────────────
//!     Handle.commands ───────────► pw::channel::Receiver
//!                                    │
//!                                    ▼
//!                                  MainLoop
//!                                    │
//!     bus::Sender<Event> ◄────────── events out
//! ```
//!
//! The PipeWire loop is single-threaded and `!Send`, so it owns its own OS
//! thread. The main thread talks to it via a `pipewire::channel` (an
//! fd-backed queue the loop can poll) for commands, and a [`crate::bus`]
//! sender for events back.
//!
//! Each child module owns one registry-global type and its writes:
//! [`node`], [`device`], [`client`], [`default_meta`], [`volume`],
//! [`monitor`]. `run` owns the shared maps and dispatches each global to
//! the matching `bind_*`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::thread;

use libspa::pod::serialize::GenError;
use pipewire as pw;
use pw::node::{Node, NodeListener};
use pw::proxy::ProxyListener;
use pw::types::ObjectType;

use crate::bus::Sender as BusSender;
use crate::domain::{Stream, StreamKind};
use crate::meter::PeakPool;

mod client;
mod default_meta;
mod device;
mod monitor;
mod node;
mod volume;

/// Errors from the worker thread. Formatted via `{:#}` so the leaf cause
/// stays visible.
#[derive(Debug)]
pub enum Error {
    PwInit {
        stage: &'static str,
        source: pw::Error,
    },
    Serialize {
        what: &'static str,
        source: GenError,
    },
    BuildPod(&'static str),
    UnknownNode(u32),
    DefaultMetaNotBound,
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PwInit { stage, .. } => write!(f, "{stage}")?,
            Self::Serialize { what, .. } => write!(f, "serialize {what}")?,
            Self::BuildPod(what) => write!(f, "parse built {what}")?,
            Self::UnknownNode(id) => write!(f, "unknown node {id}")?,
            Self::DefaultMetaNotBound => f.write_str("`default` metadata not bound yet")?,
        }
        if f.alternate() {
            let mut cur = std::error::Error::source(self);
            while let Some(e) = cur {
                write!(f, ": {e}")?;
                cur = e.source();
            }
        }
        Ok(())
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PwInit { source, .. } => Some(source),
            Self::Serialize { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum Command {
    SetVolume {
        node_id: u32,
        volume: f32,
    },
    SetMute {
        node_id: u32,
        mute: bool,
    },
    SetDefaultSink {
        node_name: String,
    },
    /// Mark a capture device as the system default audio input.
    SetDefaultSource {
        node_name: String,
    },
    /// Pin an application stream to a sink (`Some`) or clear the override
    /// to follow the default (`None`), via the `default` metadata's
    /// `target.object` key under the stream node id.
    SetStreamTarget {
        node_id: u32,
        sink_node_name: Option<String>,
    },
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum Event {
    StreamAdded(Stream),
    StreamUpdated(Stream),
    StreamRemoved(u32),
    Error(String),
}

#[derive(Clone)]
pub struct Handle {
    commands: pw::channel::Sender<Command>,
}

impl Handle {
    pub fn send(&self, cmd: Command) {
        // Drop on failure: if the worker thread is gone there's nothing to do.
        let _ = self.commands.send(cmd);
    }
}

static HANDLE: OnceLock<Handle> = OnceLock::new();

// Hard caps that panic loudly if a bounded structure grows past a sane
// threshold, cheaper than chasing a runaway leak after the OOM killer.

/// Hard cap on bound Nodes / Clients. Normal session is well under 100.
pub(crate) const MAX_BOUND_NODES: usize = 1024;
pub(crate) const MAX_BOUND_CLIENTS: usize = 1024;

/// Spawn the PipeWire thread. Idempotent: subsequent calls are no-ops
/// (the additional `evt_tx` clones are dropped on the floor).
pub fn init(evt_tx: BusSender<Event>, pool: Arc<PeakPool>) {
    if HANDLE.get().is_some() {
        return;
    }

    let (cmd_tx, cmd_rx) = pw::channel::channel::<Command>();

    let evt_tx_for_thread = evt_tx.clone();
    thread::Builder::new()
        .name("pipewire".into())
        .spawn(move || {
            if let Err(e) = run(cmd_rx, evt_tx_for_thread.clone(), pool) {
                let _ = evt_tx_for_thread.send(Event::Error(format!("pipewire thread: {e:#}")));
            }
        })
        .expect("spawn pipewire thread");

    let _ = HANDLE.set(Handle { commands: cmd_tx });
}

pub fn handle() -> Option<Handle> {
    HANDLE.get().cloned()
}

// ---------------------------------------------------------------------------
// Shared worker state
// ---------------------------------------------------------------------------

/// Per-node bookkeeping kept alive while the proxy is bound.
pub(crate) struct Bound {
    pub(crate) proxy: Node,
    pub(crate) stream: Stream,
    /// Client that owns this Node. Some apps (Spotify) leave Node-level
    /// application.* props empty and only set them on the Client.
    pub(crate) client_id: Option<u32>,
    /// Backing `Device` id (`device.id`). `None` for software-only sinks
    /// (we fall back to Node Props). `Cell` so the Node-info listener can
    /// patch it after bind; the registry global often omits `device.id`.
    pub(crate) device_id: Cell<Option<u32>>,
    /// Route `device` sub-id (`card.profile.device`). Pairs with
    /// `device_id` to identify the active route (the hardware-mixer path).
    pub(crate) route_device: Cell<Option<i32>>,
    // Listeners deregister when dropped, so we hold them.
    pub(crate) _node_listener: NodeListener,
    pub(crate) _proxy_listener: ProxyListener,
    /// Capture stream for the UI level meter, torn down with `Bound`.
    /// `None` for unmetered nodes or when stream creation failed
    /// (best-effort, a missing meter doesn't fail the bind).
    pub(crate) _monitor: Option<monitor::MonitorState>,
}

pub(crate) type BoundMap = Rc<RefCell<HashMap<u32, Bound>>>;
pub(crate) type ClientMap = Rc<RefCell<HashMap<u32, client::BoundClient>>>;
pub(crate) type DeviceMap = Rc<RefCell<HashMap<u32, device::BoundDevice>>>;
pub(crate) type DefaultNodeName = Rc<RefCell<Option<String>>>;
pub(crate) type DefaultMetaSlot = Rc<RefCell<Option<default_meta::DefaultMeta>>>;

fn run(
    cmd_rx: pw::channel::Receiver<Command>,
    evt_tx: BusSender<Event>,
    pool: Arc<PeakPool>,
) -> Result<()> {
    pw::init();

    let main_loop = pw::main_loop::MainLoopRc::new(None).map_err(|source| Error::PwInit {
        stage: "create MainLoop",
        source,
    })?;
    let context =
        pw::context::ContextRc::new(&main_loop, None).map_err(|source| Error::PwInit {
            stage: "create Context",
            source,
        })?;
    let core = context.connect_rc(None).map_err(|source| Error::PwInit {
        stage: "connect Core",
        source,
    })?;
    let registry = core.get_registry_rc().map_err(|source| Error::PwInit {
        stage: "get Registry",
        source,
    })?;

    let bound: BoundMap = Rc::new(RefCell::new(HashMap::new()));
    let clients: ClientMap = Rc::new(RefCell::new(HashMap::new()));
    let devices: DeviceMap = Rc::new(RefCell::new(HashMap::new()));
    let default_name: DefaultNodeName = Rc::new(RefCell::new(None));
    // node.name of the current `default.audio.source`; drives `is_default`
    // on the matching source.
    let default_source_name: DefaultNodeName = Rc::new(RefCell::new(None));
    let default_meta: DefaultMetaSlot = Rc::new(RefCell::new(None));

    let registry_weak = registry.downgrade();
    let core_for_global = core.clone();
    let pool_for_global = Arc::clone(&pool);
    let bound_for_global = Rc::clone(&bound);
    let clients_for_global = Rc::clone(&clients);
    let devices_for_global = Rc::clone(&devices);
    let default_name_for_global = Rc::clone(&default_name);
    let default_source_name_for_global = Rc::clone(&default_source_name);
    let default_meta_for_global = Rc::clone(&default_meta);
    let evt_for_global = evt_tx.clone();
    let _registry_listener = registry
        .add_listener_local()
        .global(move |obj| {
            let Some(registry) = registry_weak.upgrade() else {
                return;
            };

            // One registry global, one binder; each `bind_*` decides what
            // to ignore.
            match obj.type_ {
                ObjectType::Client => client::bind_client(
                    &registry,
                    obj,
                    Rc::clone(&clients_for_global),
                    Rc::clone(&bound_for_global),
                    evt_for_global.clone(),
                ),
                ObjectType::Metadata => default_meta::bind_default_meta(
                    &registry,
                    obj,
                    Rc::clone(&bound_for_global),
                    Rc::clone(&default_name_for_global),
                    Rc::clone(&default_source_name_for_global),
                    Rc::clone(&default_meta_for_global),
                    evt_for_global.clone(),
                ),
                ObjectType::Device => device::bind_device(
                    &registry,
                    obj,
                    Rc::clone(&devices_for_global),
                    Rc::clone(&bound_for_global),
                    &core_for_global,
                    evt_for_global.clone(),
                ),
                ObjectType::Node => node::bind_node(
                    &registry,
                    obj,
                    Rc::clone(&bound_for_global),
                    Rc::clone(&clients_for_global),
                    Rc::clone(&devices_for_global),
                    Rc::clone(&default_name_for_global),
                    Rc::clone(&default_source_name_for_global),
                    &core_for_global,
                    &pool_for_global,
                    evt_for_global.clone(),
                ),
                _ => {}
            }
        })
        .global_remove({
            let bound = Rc::clone(&bound);
            let clients = Rc::clone(&clients);
            let devices = Rc::clone(&devices);
            let default_name = Rc::clone(&default_name);
            let default_source_name = Rc::clone(&default_source_name);
            let default_meta = Rc::clone(&default_meta);
            let evt_tx = evt_tx.clone();
            move |id| {
                if bound.borrow_mut().remove(&id).is_some() {
                    let _ = evt_tx.send(Event::StreamRemoved(id));
                }
                clients.borrow_mut().remove(&id);
                devices.borrow_mut().remove(&id);
                // `default` metadata going away: drop the proxy and clear
                // cached names so devices fall back to non-default.
                if default_meta.borrow().as_ref().is_some_and(|m| m.id == id) {
                    *default_meta.borrow_mut() = None;
                    *default_name.borrow_mut() = None;
                    *default_source_name.borrow_mut() = None;
                    default_meta::refresh_default_marks(&bound, StreamKind::Sink, &None, &evt_tx);
                    default_meta::refresh_default_marks(&bound, StreamKind::Source, &None, &evt_tx);
                }
            }
        })
        .register();

    let main_loop_weak = main_loop.downgrade();
    let bound_for_cmd = Rc::clone(&bound);
    let devices_for_cmd = Rc::clone(&devices);
    let default_meta_for_cmd = Rc::clone(&default_meta);
    let default_name_for_cmd = Rc::clone(&default_name);
    let default_source_name_for_cmd = Rc::clone(&default_source_name);
    let evt_for_cmd = evt_tx.clone();
    let _cmd_recv = cmd_rx.attach(main_loop.loop_(), move |cmd| match cmd {
        Command::SetVolume { node_id, volume } => {
            if let Err(e) = volume::apply_volume(&bound_for_cmd, &devices_for_cmd, node_id, volume)
            {
                let _ = evt_for_cmd.send(Event::Error(format!("set volume: {e:#}")));
            }
        }
        Command::SetMute { node_id, mute } => {
            if let Err(e) = volume::apply_mute(&bound_for_cmd, node_id, mute) {
                let _ = evt_for_cmd.send(Event::Error(format!("set mute: {e:#}")));
            }
        }
        Command::SetDefaultSink { node_name } => {
            if let Err(e) = default_meta::apply_default(
                &default_meta_for_cmd,
                &default_name_for_cmd,
                &bound_for_cmd,
                StreamKind::Sink,
                &node_name,
                &evt_for_cmd,
            ) {
                let _ = evt_for_cmd.send(Event::Error(format!("set default sink: {e:#}")));
            }
        }
        Command::SetDefaultSource { node_name } => {
            if let Err(e) = default_meta::apply_default(
                &default_meta_for_cmd,
                &default_source_name_for_cmd,
                &bound_for_cmd,
                StreamKind::Source,
                &node_name,
                &evt_for_cmd,
            ) {
                let _ = evt_for_cmd.send(Event::Error(format!("set default source: {e:#}")));
            }
        }
        Command::SetStreamTarget {
            node_id,
            sink_node_name,
        } => {
            if let Err(e) = default_meta::apply_stream_target(
                &default_meta_for_cmd,
                node_id,
                sink_node_name.as_deref(),
            ) {
                let _ = evt_for_cmd.send(Event::Error(format!("set stream target: {e:#}")));
            }
        }
        Command::Shutdown => {
            if let Some(ml) = main_loop_weak.upgrade() {
                ml.quit();
            }
        }
    });

    main_loop.run();

    Ok(())
}
