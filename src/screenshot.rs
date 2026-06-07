//! Save a PNG of a window straight from GTK, no external screenshot tool.
//! Bound to Ctrl+Shift+S. Writes to the path in BNKSOUND_SCREENSHOT when that
//! is set, otherwise a timestamped file in the user's Pictures dir, falling
//! back to the current directory.

use std::cell::Cell;
use std::fmt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use gtk4 as gtk;
use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;

#[derive(Debug)]
enum Error {
    NotRealized,
    EmptyScene,
    Save {
        path: PathBuf,
        source: glib::BoolError,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRealized => f.write_str("window not realized yet, nothing to capture"),
            Self::EmptyScene => f.write_str("window produced an empty render tree"),
            Self::Save { path, source } => write!(f, "save {}: {source}", path.display()),
        }
    }
}

/// Capture the window to disk and report the outcome on stderr. Failures are
/// logged, not propagated: a missed screenshot never disturbs the session.
///
/// A widget caches its render node only between a paint and the next
/// queue_draw, and the live meters invalidate it every frame, so grabbing it
/// synchronously races them. Force a frame and capture in the after-paint
/// phase, when the node is freshly rebuilt and nothing has cleared it yet.
pub fn capture_window(window: &gtk::ApplicationWindow) {
    let Some(clock) = window.frame_clock() else {
        eprintln!("screenshot: {}", Error::NotRealized);
        return;
    };
    let path = destination();
    window.queue_draw();

    let window = window.clone();
    let handler = Rc::new(Cell::new(None::<glib::SignalHandlerId>));
    let handler_inner = Rc::clone(&handler);
    let id = clock.connect_after_paint(move |clock| {
        // One shot: unhook before saving so a save-time redraw can't re-enter.
        if let Some(id) = handler_inner.take() {
            clock.disconnect(id);
        }
        match save(&window, &path) {
            Ok(()) => eprintln!("screenshot: wrote {}", path.display()),
            Err(e) => eprintln!("screenshot: {e}"),
        }
    });
    handler.set(Some(id));
}

fn save(window: &gtk::ApplicationWindow, path: &Path) -> Result<(), Error> {
    let texture = render(window)?;
    if let Some(parent) = path.parent() {
        // Best-effort: a genuine problem resurfaces from save_to_png below.
        let _ = std::fs::create_dir_all(parent);
    }
    texture.save_to_png(path).map_err(|source| Error::Save {
        path: path.to_path_buf(),
        source,
    })
}

// Render the live widget tree into an offscreen texture via the window's own
// GSK renderer. Captures the client area as drawn, so a CSD titlebar is
// included and a server-side one is not.
fn render(window: &gtk::ApplicationWindow) -> Result<gdk::Texture, Error> {
    let width = window.width();
    let height = window.height();
    if width <= 0 || height <= 0 {
        return Err(Error::NotRealized);
    }
    let renderer = window.renderer().ok_or(Error::NotRealized)?;

    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
    let node = snapshot.to_node().ok_or(Error::EmptyScene)?;

    Ok(renderer.render_texture(&node, None))
}

fn destination() -> PathBuf {
    let explicit = std::env::var_os("BNKSOUND_SCREENSHOT").map(PathBuf::from);
    screenshot_path(explicit, pictures_dir(), unix_timestamp())
}

fn screenshot_path(explicit: Option<PathBuf>, pictures: Option<PathBuf>, stamp: u64) -> PathBuf {
    if let Some(path) = explicit {
        return path;
    }
    let dir = pictures.unwrap_or_else(|| PathBuf::from("."));
    dir.join(format!("bnksound-{stamp}.png"))
}

fn pictures_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_PICTURES_DIR") {
        let dir = PathBuf::from(dir);
        if dir.is_dir() {
            return Some(dir);
        }
    }
    let pictures = PathBuf::from(std::env::var_os("HOME")?).join("Pictures");
    pictures.is_dir().then_some(pictures)
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_path_wins_over_pictures() {
        let got = screenshot_path(
            Some(PathBuf::from("assets/screenshot.png")),
            Some(PathBuf::from("/home/x/Pictures")),
            123,
        );
        assert_eq!(got, PathBuf::from("assets/screenshot.png"));
    }

    #[test]
    fn pictures_dir_gets_a_timestamped_name() {
        let got = screenshot_path(None, Some(PathBuf::from("/home/x/Pictures")), 1_700_000_000);
        assert_eq!(
            got,
            PathBuf::from("/home/x/Pictures/bnksound-1700000000.png")
        );
    }

    #[test]
    fn no_pictures_dir_falls_back_to_current_dir() {
        let got = screenshot_path(None, None, 42);
        assert_eq!(got, PathBuf::from("./bnksound-42.png"));
    }
}
