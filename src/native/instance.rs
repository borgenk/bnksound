//! One window per session.
//!
//! The first launch binds a socket under XDG_RUNTIME_DIR and holds it for as
//! long as the window lives. A later launch finds it bound, hands over the
//! activation token its launcher gave it, and exits; the instance holding the
//! socket brings its window forward instead. The lock and the handover live
//! here; raising the window is the app's job.

use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Most of a handover we read. Activation tokens run to a few dozen bytes, so
/// past this the peer is not one of ours.
const MAX_TOKEN: usize = 1024;

/// How long the answering instance waits for a handover's bytes. Only a raise
/// rides on them, so giving up costs the raise and nothing else.
const HANDOVER_TIMEOUT: Duration = Duration::from_millis(100);

/// Environment variables a launcher passes its activation token in. The first
/// is the Wayland name for it, the second the older X11 one that some
/// launchers still set alongside.
const TOKEN_VARS: [&str; 2] = ["XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID"];

/// What claiming the lock settled.
pub enum Launch {
    /// This process is the app. The listener answers later launches; None when
    /// the lock could not be taken at all, which leaves later launches to open
    /// windows of their own. The token is this launch's own, to raise the
    /// window it is about to open.
    Run {
        listener: Option<Listener>,
        token: String,
    },
    /// A window was already up and has been asked to present itself, so this
    /// process has nothing left to do.
    HandedOver,
}

/// The bound socket, held for as long as the window lives.
pub struct Listener {
    sock: UnixListener,
    path: PathBuf,
}

impl Listener {
    /// The socket fd, to poll for a waiting launch.
    pub fn fd(&self) -> RawFd {
        self.sock.as_raw_fd()
    }

    /// Take the next launch waiting on the socket and return the activation
    /// token it handed over, which is empty when its launcher gave it none.
    /// None when nothing was waiting.
    pub fn accept(&self) -> Option<String> {
        let (peer, _) = self.sock.accept().ok()?;
        let mut bytes = Vec::new();
        if peer.set_read_timeout(Some(HANDOVER_TIMEOUT)).is_ok() {
            let _ = peer.take(MAX_TOKEN as u64).read_to_end(&mut bytes);
        }
        Some(token_from(&bytes))
    }
}

impl Drop for Listener {
    /// Unbind on a clean exit so the next launch finds nothing to clear. The
    /// file goes before the socket closes, so a launch that races the exit and
    /// binds its own cannot have it unlinked from under it.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Claim the one-window lock for this Wayland session.
pub fn claim() -> Launch {
    let token = activation_token();
    let Some(path) = socket_path() else {
        return Launch::Run {
            listener: None,
            token,
        };
    };
    claim_at(&path, &token)
}

/// Claim the lock at `path`, handing `token` over to whoever already holds it.
fn claim_at(path: &Path, token: &str) -> Launch {
    // Two turns at most. The first can find a socket left behind by an
    // instance that died without unbinding, which is cleared and the bind
    // retried; the second either binds or gives up.
    for _ in 0..2 {
        let err = match UnixListener::bind(path) {
            Ok(sock) => {
                // The loop polls this fd, so accept must never park the UI on a
                // peer that connected and left again.
                if sock.set_nonblocking(true).is_err() {
                    return Launch::Run {
                        listener: None,
                        token: token.to_string(),
                    };
                }
                return Launch::Run {
                    listener: Some(Listener {
                        sock,
                        path: path.to_path_buf(),
                    }),
                    token: token.to_string(),
                };
            }
            Err(e) => e,
        };
        if err.kind() != ErrorKind::AddrInUse {
            return Launch::Run {
                listener: None,
                token: token.to_string(),
            };
        }
        match UnixStream::connect(path) {
            Ok(peer) => {
                hand_over(peer, token);
                return Launch::HandedOver;
            }
            // Bound with nobody listening: the socket outlived its process.
            Err(_) if std::fs::remove_file(path).is_ok() => {}
            Err(_) => {
                return Launch::Run {
                    listener: None,
                    token: token.to_string(),
                };
            }
        }
    }
    Launch::Run {
        listener: None,
        token: token.to_string(),
    }
}

/// Hand this launch over to the running instance. Best effort: it either takes
/// the bytes or the window stays where it is.
fn hand_over(mut peer: UnixStream, token: &str) {
    let _ = peer.write_all(token.as_bytes());
}

/// The activation token our launcher gave us, empty when it gave none.
///
/// The token is what tells the compositor a raise was asked for rather than
/// stolen, and it belongs to the process the user just launched, which is why
/// it travels over the socket to the one that can use it.
fn activation_token() -> String {
    TOKEN_VARS
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|value| !value.is_empty())
        .unwrap_or_default()
}

/// Read a token out of the bytes a launch sent. They come from another process,
/// so nothing about them is assumed: the string stops at the first NUL, since
/// one inside a Wayland string would break the message carrying it.
fn token_from(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

/// Where the lock's socket lives: one per compositor, so a nested one is its
/// own session and gets its own window. None when XDG_RUNTIME_DIR is unset,
/// leaving nowhere session-private to put it.
fn socket_path() -> Option<PathBuf> {
    let dir = env_var("XDG_RUNTIME_DIR")?;
    let display = env_var("WAYLAND_DISPLAY").unwrap_or_else(|| "wayland-0".to_string());
    Some(PathBuf::from(dir).join(format!("bnksound-{}.sock", one_component(&display))))
}

/// A set, non-empty environment variable.
fn env_var(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

/// Fold a display name into a single path component. WAYLAND_DISPLAY may be an
/// absolute path, and the whole socket path has to fit in sun_path, so the name
/// is both flattened and cut short.
fn one_component(display: &str) -> String {
    display
        .chars()
        .take(32)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A socket path of this test's own, since tests share the process.
    fn path(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "bnksound_instance_{}_{name}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    /// The listener a claim took, or a panic naming what it did instead.
    fn listener(launch: Launch) -> Listener {
        match launch {
            Launch::Run {
                listener: Some(listener),
                ..
            } => listener,
            Launch::Run { listener: None, .. } => panic!("the lock was not taken"),
            Launch::HandedOver => panic!("the launch was handed over"),
        }
    }

    #[test]
    fn the_launch_that_takes_the_lock_keeps_its_own_token() {
        let path = path("keeps-token");
        match claim_at(&path, "tok-first") {
            Launch::Run { listener, token } => {
                assert!(listener.is_some(), "the lock was there to take");
                assert_eq!(
                    token, "tok-first",
                    "the window this launch opens has a token to raise itself with"
                );
            }
            Launch::HandedOver => panic!("nothing was up to hand over to"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_launch_that_cannot_bind_still_carries_its_token() {
        let path = Path::new("/bnksound-no-such-directory/instance.sock");
        match claim_at(path, "tok-unbound") {
            Launch::Run { listener, token } => {
                assert!(listener.is_none(), "nothing could be bound");
                assert_eq!(token, "tok-unbound");
            }
            Launch::HandedOver => panic!("there was nobody to hand over to"),
        }
    }

    #[test]
    fn the_first_launch_takes_the_lock_and_the_next_hands_its_token_over() {
        let path = path("handover");
        let held = listener(claim_at(&path, ""));
        assert!(
            path.exists(),
            "the socket is there for later launches to find"
        );
        assert_eq!(held.accept(), None, "nothing has been handed over yet");

        match claim_at(&path, "tok-123") {
            Launch::HandedOver => {}
            _ => panic!("a second launch must hand over, not take the lock"),
        }
        assert_eq!(held.accept().as_deref(), Some("tok-123"));
        assert_eq!(held.accept(), None, "one launch, one handover");
    }

    #[test]
    fn a_launch_with_no_token_still_asks_for_the_window() {
        let path = path("no_token");
        let held = listener(claim_at(&path, ""));
        assert!(matches!(claim_at(&path, ""), Launch::HandedOver));
        // Empty is the app's cue to ask the compositor for a token of its own.
        assert_eq!(held.accept().as_deref(), Some(""));
    }

    #[test]
    fn a_socket_left_behind_by_a_dead_instance_is_cleared() {
        let path = path("stale");
        // Dropping a listener closes the socket without unlinking it, which is
        // what a killed instance leaves behind.
        std::mem::drop(UnixListener::bind(&path).expect("bind a stale socket"));
        assert!(path.exists(), "the stale socket is in the way");

        let held = listener(claim_at(&path, ""));
        assert!(matches!(claim_at(&path, "after-stale"), Launch::HandedOver));
        assert_eq!(
            held.accept().as_deref(),
            Some("after-stale"),
            "the new instance owns the name the dead one left"
        );
    }

    #[test]
    fn a_clean_exit_unbinds_so_the_next_launch_takes_the_lock() {
        let path = path("unbind");
        std::mem::drop(listener(claim_at(&path, "")));
        assert!(!path.exists(), "the socket goes with the instance");
        // And the next launch is the first one again, not a handover.
        let _held = listener(claim_at(&path, ""));
    }

    #[test]
    fn a_lock_that_cannot_be_bound_leaves_the_window_to_open_anyway() {
        // A directory that is not there stands in for any reason the socket
        // cannot be bound. The window still has to come up.
        let path = Path::new("/bnksound-no-such-directory/instance.sock");
        match claim_at(path, "") {
            Launch::Run { listener: None, .. } => {}
            _ => panic!("a failed lock must still let the app run"),
        }
    }

    #[test]
    fn a_handover_is_read_as_one_line_of_token() {
        // Trailing whitespace from a shell, and anything past a NUL, are cut:
        // a NUL inside a Wayland string would break the request carrying it.
        assert_eq!(token_from(b"tok-1\n"), "tok-1");
        assert_eq!(token_from(b"  tok-2  "), "tok-2");
        assert_eq!(token_from(b"tok-3\0junk"), "tok-3");
        assert_eq!(token_from(b""), "");
        assert_eq!(token_from(b"\0"), "");
        // Invalid UTF-8 is replaced rather than dropping the whole token.
        assert_eq!(token_from(&[0xff, b'a']), "\u{fffd}a");
    }

    #[test]
    fn an_oversized_handover_is_capped_rather_than_read_to_the_end() {
        let path = path("oversized");
        let held = listener(claim_at(&path, ""));
        let peer = UnixStream::connect(&path).expect("connect");
        hand_over(peer, &"x".repeat(MAX_TOKEN * 4));
        let token = held.accept().expect("a handover arrived");
        assert_eq!(token.len(), MAX_TOKEN);
    }

    #[test]
    fn accepting_never_parks_the_loop_on_a_peer_that_says_nothing() {
        let path = path("silent");
        let held = listener(claim_at(&path, ""));

        // A peer that connected and left again ends the read at once.
        std::mem::drop(UnixStream::connect(&path).expect("connect"));
        let started = std::time::Instant::now();
        assert_eq!(held.accept().as_deref(), Some(""));

        // One that holds the socket open and silent is given up on instead.
        let peer = UnixStream::connect(&path).expect("connect");
        assert_eq!(held.accept().as_deref(), Some(""));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "both reads are bounded"
        );
        std::mem::drop(peer);
    }

    #[test]
    fn the_socket_name_holds_the_display_in_one_component() {
        assert_eq!(one_component("wayland-1"), "wayland-1");
        assert_eq!(
            one_component("/run/user/1000/wayland-0"),
            "_run_user_1000_wayland-0"
        );
        assert_eq!(one_component(&"w".repeat(64)).len(), 32);
    }
}
