//! Run the window against whatever compositor WAYLAND_DISPLAY names, then
//! report what the exchange produced.
//!
//! The window is the real one: the same lock, the same App, the same loop the
//! shipped path runs. Only the ending differs, in that the run stops on a
//! deadline instead of on a close, which is what makes it a command a test can
//! wait for. A launch that finds the lock already taken hands itself over and
//! says so, so the same flag serves both halves of the one-window test.
//!
//! ```sh
//! bnksound --probe [ms]
//! ```

use std::time::{Duration, Instant};

use crate::dev::Result;
use crate::native::app::App;
use crate::native::instance::{self, Launch};

/// How long the window stays up when the command line names no duration. Long
/// enough for a configure, a frame, and the activation round trip.
const DEFAULT_MS: u64 = 1500;

/// Open the window, run it to the deadline, and print the facts.
pub fn run(args: &[String]) -> Result<()> {
    let ms = args
        .iter()
        .skip_while(|a| *a != "--probe")
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MS);

    let (instance, token) = match instance::claim() {
        Launch::Run { listener, token } => (listener, token),
        Launch::HandedOver => {
            println!("probe end handed-over");
            return Ok(());
        }
    };

    let mut app = App::new(instance, token)?;
    let deadline = Instant::now() + Duration::from_millis(ms);
    while !app.closed && Instant::now() < deadline {
        app.tick()?;
    }

    print!("{}", app.facts());
    println!("probe end ok");
    app.shutdown();
    Ok(())
}
