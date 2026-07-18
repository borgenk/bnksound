//! Cross-thread message bus with a pollable wakeup fd.
//!
//! A bounded queue plus an eventfd. Producers on any thread (the PipeWire
//! thread and its audio callbacks, MPRIS, loop timers) enqueue without blocking
//! and signal the fd; the consumer registers that fd in its event loop and
//! drains the queue when it wakes. One send policy lives here: send never
//! blocks. A full queue, sized above any valid burst, drops the message and
//! logs rather than grow toward OOM; a closed queue returns Closed.

use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::sync::Arc;
use std::sync::mpsc::{self, SyncSender, TrySendError};

use crate::platform::sys;

/// The consumer ([`Receiver`]) was dropped, so the message can't be delivered.
/// Returned by [`Sender::send`] so a producer loop can stop.
#[derive(Debug)]
pub struct Closed;

/// Producer handle. Cheap to clone; hand a clone to every producer thread.
pub struct Sender<T> {
    tx: SyncSender<T>,
    wake: Arc<OwnedFd>,
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            wake: Arc::clone(&self.wake),
        }
    }
}

impl<T> Sender<T> {
    /// Queue a message without blocking. `Err(Closed)` means the consumer is
    /// gone (normal at shutdown). A full queue (capacity sized above every valid
    /// burst) signals a stall or runaway producer: the message is dropped, a
    /// line is logged, and `Ok` returned rather than grow toward OOM, so callers
    /// must not treat a drop as shutdown.
    pub fn send(&self, msg: T) -> Result<(), Closed> {
        match self.tx.try_send(msg) {
            Ok(()) => {
                // Enqueue, then wake. The consumer clears the fd before it
                // drains, so a message queued here is always observed by the
                // drain that clearing precedes, or leaves the fd armed for the
                // next poll. Either way the wake is never lost.
                sys::eventfd_signal(self.wake.as_raw_fd());
                Ok(())
            }
            Err(TrySendError::Disconnected(_)) => Err(Closed),
            Err(TrySendError::Full(_)) => {
                eprintln!(
                    "bus: dropping {} — queue full; runaway producer or stalled \
                     consumer suspected",
                    std::any::type_name::<T>(),
                );
                Ok(())
            }
        }
    }
}

/// Consumer handle. Drained on the main thread by the shell's event loop when
/// the wakeup fd polls readable.
pub struct Receiver<T> {
    rx: mpsc::Receiver<T>,
    wake: Arc<OwnedFd>,
}

impl<T> Receiver<T> {
    /// The wakeup fd to register in the event loop. Readable while messages are
    /// waiting. Poll it; do not read it directly, [`drain`](Self::drain) clears
    /// it.
    pub fn wake_fd(&self) -> RawFd {
        self.wake.as_raw_fd()
    }

    /// Clear the wakeup fd, then hand every queued message to `f`. Clearing
    /// before draining is what makes a burst safe: a message enqueued mid-drain
    /// re-arms the fd, so the next poll wakes again and no message is stranded.
    pub fn drain(&self, mut f: impl FnMut(T)) {
        sys::eventfd_clear(self.wake.as_raw_fd());
        while let Ok(msg) = self.rx.try_recv() {
            f(msg);
        }
    }
}

/// Create a bounded bus with `capacity` slots and a fresh wakeup eventfd. Pick
/// `capacity` above any valid burst; see [`Sender::send`] for what happens past
/// it. Fails only if the eventfd cannot be created (out of descriptors).
pub fn channel<T>(capacity: usize) -> std::io::Result<(Sender<T>, Receiver<T>)> {
    let (tx, rx) = mpsc::sync_channel(capacity);
    let wake = Arc::new(sys::make_eventfd()?);
    Ok((
        Sender {
            tx,
            wake: Arc::clone(&wake),
        },
        Receiver { rx, wake },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::time::Duration;

    fn drained(rx: &Receiver<i32>) -> Vec<i32> {
        let mut out = Vec::new();
        rx.drain(|m| out.push(m));
        out
    }

    #[test]
    fn send_then_drain_delivers_in_order() {
        let (tx, rx) = channel::<i32>(16).unwrap();
        tx.send(1).unwrap();
        tx.send(2).unwrap();
        tx.send(3).unwrap();
        assert_eq!(drained(&rx), vec![1, 2, 3]);
        // Drained queue yields nothing further.
        assert_eq!(drained(&rx), Vec::<i32>::new());
    }

    #[test]
    fn send_wakes_the_poll_fd_and_drain_clears_it() {
        let (tx, rx) = channel::<i32>(16).unwrap();
        let mut fds = [sys::PollFd::readable(rx.wake_fd())];
        assert_eq!(sys::poll(&mut fds, Some(Duration::ZERO)).unwrap(), 0);

        tx.send(7).unwrap();
        let mut fds = [sys::PollFd::readable(rx.wake_fd())];
        assert_eq!(sys::poll(&mut fds, Some(Duration::ZERO)).unwrap(), 1);

        assert_eq!(drained(&rx), vec![7]);
        let mut fds = [sys::PollFd::readable(rx.wake_fd())];
        assert_eq!(sys::poll(&mut fds, Some(Duration::ZERO)).unwrap(), 0);
    }

    #[test]
    fn one_drain_after_one_wake_collects_a_whole_burst() {
        let (tx, rx) = channel::<i32>(1024).unwrap();
        for i in 0..500 {
            tx.send(i).unwrap();
        }
        // A single wake (one poll readiness) drains every queued message.
        let got = drained(&rx);
        assert_eq!(got.len(), 500);
        assert_eq!(got, (0..500).collect::<Vec<_>>());
    }

    #[test]
    fn full_queue_drops_the_overflow_without_error() {
        let (tx, rx) = channel::<i32>(2).unwrap();
        tx.send(1).unwrap();
        tx.send(2).unwrap();
        // Third send is dropped (queue full) but reports Ok, not shutdown.
        tx.send(3).unwrap();
        assert_eq!(drained(&rx), vec![1, 2]);
    }

    #[test]
    fn send_after_receiver_dropped_reports_closed() {
        let (tx, rx) = channel::<i32>(4).unwrap();
        drop(rx);
        assert!(matches!(tx.send(1), Err(Closed)));
    }

    #[test]
    fn many_producers_lose_no_messages() {
        const PRODUCERS: usize = 8;
        const PER: i32 = 1000;
        let (tx, rx) = channel::<i32>(PRODUCERS * PER as usize).unwrap();
        let barrier = Arc::new(Barrier::new(PRODUCERS));
        let mut handles = Vec::new();
        for _ in 0..PRODUCERS {
            let tx = tx.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for i in 0..PER {
                    tx.send(i).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(drained(&rx).len(), PRODUCERS * PER as usize);
    }
}
