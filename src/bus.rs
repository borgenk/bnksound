//! Cross-thread message channel feeding the GTK main loop.
//!
//! A thin wrapper over async_channel that pins down one send policy in one
//! place: send never blocks. Producers run on any thread (the PipeWire thread
//! and its audio callbacks, MPRIS, GLib timeouts), and none can afford to wait
//! on a full queue, so a full channel drops the message and logs while a closed
//! channel returns Err(Closed). The consumer drains the Receiver from a
//! spawn_future_local task on the main thread in crate::app.

/// The consumer ([`Receiver`]) was dropped, so the message can't be delivered.
/// Returned by [`Sender::send`] so a producer loop can stop.
#[derive(Debug)]
pub struct Closed;

/// Producer handle. Cheap to clone; hand a clone to every producer thread.
pub struct Sender<T> {
    inner: async_channel::Sender<T>,
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Sender<T> {
    /// Queue a message without blocking. `Err(Closed)` means the consumer is
    /// gone (normal at shutdown). A *full* channel (capacity sized above every
    /// valid burst) signals a stall or runaway producer: we drop the message,
    /// log loudly, and return `Ok` rather than grow toward OOM, so callers must
    /// not treat a drop as shutdown.
    pub fn send(&self, msg: T) -> Result<(), Closed> {
        match self.inner.try_send(msg) {
            Ok(()) => Ok(()),
            Err(async_channel::TrySendError::Closed(_)) => Err(Closed),
            Err(async_channel::TrySendError::Full(_)) => {
                eprintln!(
                    "bus: dropping {} — channel full at capacity {:?}; \
                     runaway producer or stalled consumer suspected",
                    std::any::type_name::<T>(),
                    self.inner.capacity(),
                );
                Ok(())
            }
        }
    }
}

/// Consumer handle. Drained on the GLib main loop by a `spawn_future_local`
/// task in `crate::app`.
pub struct Receiver<T> {
    inner: async_channel::Receiver<T>,
}

impl<T> Receiver<T> {
    /// Await the next message, or `None` once every [`Sender`] is dropped and
    /// the queue is drained.
    pub(crate) async fn recv(&self) -> Option<T> {
        self.inner.recv().await.ok()
    }
}

/// Create a bounded channel with `capacity` slots (allocated once, reused;
/// `send` never reallocates). Pick `capacity` above any valid burst; see
/// [`Sender::send`] for what happens past it.
pub fn channel<T>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = async_channel::bounded(capacity);
    (Sender { inner: tx }, Receiver { inner: rx })
}
