//! Shell-agnostic application core.
//!
//! Runtime owns the persistent state, the peak pool, and the message sender,
//! and reduces messages through state::update. It knows nothing about GTK,
//! Wayland, or any main loop: each shell boots one, drains the two receivers on
//! its own loop, and calls dispatch. What draws afterward is the shell's
//! business.

use std::io;
use std::sync::Arc;

use crate::bus::{self, Receiver, Sender};
use crate::meter::PeakPool;
use crate::pipewire_worker::{self, Command, Event, Handle};
use crate::state::{self, App, Message};

/// How many messages each bus can hold. Sized above any valid burst; see
/// [`bus::Sender::send`] for what happens past it.
const BUS_CAPACITY: usize = 4096;

/// The application core: persistent state plus the handles a shell needs to
/// feed and read it.
pub struct Runtime {
    state: App,
    peaks: Arc<PeakPool>,
    msg_tx: Sender<Message>,
    worker: Handle,
    /// Where a reduce leaves the commands it wants sent. Kept between calls so
    /// a slider drag reuses one allocation.
    commands: Vec<Command>,
}

impl Runtime {
    /// Boot the services a running app needs: the two buses, the shared peak
    /// pool, the PipeWire worker thread, and the persisted state. Returns the
    /// runtime plus the message and worker-event receivers the shell's loop
    /// drains. Fails only if a bus wakeup fd cannot be created.
    pub fn boot() -> io::Result<(Runtime, Receiver<Message>, Receiver<Event>)> {
        let (msg_tx, msg_rx) = bus::channel::<Message>(BUS_CAPACITY)?;
        let (evt_tx, evt_rx) = bus::channel::<Event>(BUS_CAPACITY)?;

        let peaks = Arc::new(PeakPool::new());
        let worker = pipewire_worker::init(evt_tx, Arc::clone(&peaks));

        let runtime = Runtime {
            state: state::boot(),
            peaks,
            msg_tx,
            worker,
            commands: Vec::new(),
        };
        Ok((runtime, msg_rx, evt_rx))
    }

    /// A message sender clone for the shell to hand to producers.
    pub fn sender(&self) -> Sender<Message> {
        self.msg_tx.clone()
    }

    /// The current state, for the shell to project and draw.
    pub fn state(&self) -> &App {
        &self.state
    }

    /// The shared peak pool, read by the shell's meter tick.
    pub fn peaks(&self) -> &Arc<PeakPool> {
        &self.peaks
    }

    /// Reduce one message and report whether the view needs a refresh.
    ///
    /// Two messages are quieter than the rest. A save tick changes what is
    /// drawn only when it fails, and what it changes is the status line, so
    /// that is what decides. A resize never changes what is drawn inside the
    /// window at all.
    pub fn dispatch(&mut self, message: Message) -> bool {
        let status_before = match &message {
            Message::AutoSaveTick => Some(self.state.status.clone()),
            _ => None,
        };
        let resized = matches!(message, Message::GeometryChanged { .. });

        self.commands.clear();
        state::update(&mut self.state, message, &mut self.commands);
        // The reduce only names what it wants done; sending it is this side of
        // the boundary, which is what keeps the reducer a pure function of its
        // state and its message.
        for command in self.commands.drain(..) {
            self.worker.send(command);
        }

        match status_before {
            Some(before) => self.state.status != before,
            None => !resized,
        }
    }

    /// Wrap a worker event as a Message and dispatch it.
    pub fn dispatch_worker(&mut self, event: Event) -> bool {
        self.dispatch(Message::Worker(Box::new(event)))
    }

    /// Final save attempt on shutdown: an AutoSaveTick flushes any dirty state
    /// and geometry.
    pub fn shutdown(&mut self) {
        let _ = self.dispatch(Message::AutoSaveTick);
    }
}
