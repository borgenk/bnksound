pub mod app;
pub mod bus;
pub mod command_palette;
pub mod domain;
pub mod geometry;
pub mod meter;
pub mod mpris;
pub mod pipewire_worker;
pub mod profile;
pub mod screenshot;
pub mod settings;
pub mod state;
pub mod store;
pub mod ui;
pub mod xdg;

use std::path::{Path, PathBuf};

/// Resolve a file under the XDG config dir (`$XDG_CONFIG_HOME` or
/// `$HOME/.config`, then `bnksound/<filename>`). `None` when neither env var
/// is set; callers degrade to in-memory only.
pub fn config_path(filename: &str) -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return Some(PathBuf::from(xdg).join("bnksound").join(filename));
    }
    let home = std::env::var("HOME").ok().filter(|s| !s.is_empty())?;
    Some(
        PathBuf::from(home)
            .join(".config")
            .join("bnksound")
            .join(filename),
    )
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
