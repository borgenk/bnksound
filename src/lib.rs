pub mod bus;
pub mod command_palette;
pub mod dbus;
/// Development tooling: the perf gate, the table generator, the frame renderer.
/// Behind a feature so the shipping binary carries none of it.
#[cfg(feature = "dev")]
pub mod dev;
pub mod domain;
pub mod geometry;
pub mod meter;
pub mod mpris;
pub mod native;
pub mod pipewire_worker;
pub mod platform;
pub mod profile;
pub mod render;
pub mod runtime;
pub mod settings;
pub mod shell;
pub mod state;
pub mod store;
pub mod ui;
pub mod view;
pub mod xdg;

#[cfg(feature = "gtk")]
pub mod gtk_shell;

use std::path::{Path, PathBuf};

/// The application id, which is what the desktop matches a window against its
/// entry and icon by. Both shells set it on their toplevel, and it names the
/// desktop entry and icon files that ship alongside the binary.
pub const APP_ID: &str = "io.github.borgenk.BnkSound";

/// Resolve a file under the XDG config dir (`$XDG_CONFIG_HOME` or
/// `$HOME/.config`, then `bnksound/<filename>`). `None` when neither env var
/// is set; callers degrade to in-memory only.
pub fn config_path(filename: &str) -> Option<PathBuf> {
    config_path_in("bnksound", filename)
}

/// The same resolution for a config directory belonging to someone else, e.g.
/// `gtk-4.0/settings.ini`, which is read to follow the desktop's own settings.
pub fn config_path_in(dir: &str, filename: &str) -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return Some(PathBuf::from(xdg).join(dir).join(filename));
    }
    let home = std::env::var("HOME").ok().filter(|s| !s.is_empty())?;
    Some(PathBuf::from(home).join(".config").join(dir).join(filename))
}

/// Failure from [`atomic_write`], tagging which step failed.
pub struct AtomicWriteError {
    pub op: &'static str,
    pub path: PathBuf,
    pub source: std::io::Error,
}

/// Durable write: stream `bytes` into `tmp`, fsync, then rename over `dest`,
/// so a crash mid-write leaves the previous `dest` intact. `tmp` must sit on
/// the same filesystem as `dest` for the rename to stay atomic.
pub fn atomic_write(dest: &Path, tmp: &Path, bytes: &[u8]) -> Result<(), AtomicWriteError> {
    use std::io::Write;
    {
        let mut f = std::fs::File::create(tmp).map_err(|source| AtomicWriteError {
            op: "create temp file",
            path: tmp.to_path_buf(),
            source,
        })?;
        f.write_all(bytes).map_err(|source| AtomicWriteError {
            op: "write temp file",
            path: tmp.to_path_buf(),
            source,
        })?;
        f.sync_all().map_err(|source| AtomicWriteError {
            op: "fsync temp file",
            path: tmp.to_path_buf(),
            source,
        })?;
    }
    std::fs::rename(tmp, dest).map_err(|source| AtomicWriteError {
        op: "rename temp file",
        path: tmp.to_path_buf(),
        source,
    })?;
    Ok(())
}
