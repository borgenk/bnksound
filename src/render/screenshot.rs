//! Save a PNG of the painted frame, bound to Ctrl+Shift+S.
//!
//! Both shells paint the same buffer, so the capture is that buffer rather than
//! anything the window system holds: what lands on disk is exactly what was
//! drawn. Writes to BNKSOUND_SCREENSHOT when set, otherwise a timestamped file
//! in the user's Pictures dir, falling back to the current directory.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::render::png;

/// Write `pixels` to the destination and report it on stderr. Failures are
/// logged, not propagated: a missed screenshot never disturbs the session.
pub fn capture(pixels: &[u32], width: u32, height: u32) {
    let path = destination();
    match write(&path, pixels, width, height) {
        Ok(()) => eprintln!("screenshot: wrote {}", path.display()),
        Err(e) => eprintln!("screenshot: {}: {e}", path.display()),
    }
}

fn write(path: &Path, pixels: &[u32], width: u32, height: u32) -> io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, png::encode_rgb(pixels, width, height))
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

    #[test]
    fn a_capture_lands_on_disk_as_a_png() {
        let path = std::env::temp_dir().join("bnksound_capture_test.png");
        let _ = std::fs::remove_file(&path);
        write(&path, &[0xff00_ff00; 4], 2, 2).expect("write");
        let bytes = std::fs::read(&path).expect("read back");
        assert_eq!(&bytes[1..4], b"PNG");
        std::fs::remove_file(&path).expect("cleanup");
    }
}
