//! Session-bus MPRIS metadata enrichment.
//!
//! Players registering `org.mpris.MediaPlayer2.*` expose "now playing"
//! metadata (title, artist), letting the UI label a stream "YouTube · Title"
//! instead of "Stream 113". Streams match players by PID, via
//! `GetConnectionUnixProcessID`. Caveats:
//!
//! - Chromium-family browsers share one AudioService PID and one MPRIS player
//!   across tabs, so the title tracks the most recently played media. Still
//!   beats "Stream <id>".
//! - Apps with no MPRIS player get no enrichment; the row keeps its label.
//!
//! Runs on the GTK main thread via GLib's DBus integration; callbacks update
//! the cache in place and nudge the UI to repaint with a `state::Message`.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib;
use gtk4 as gtk;

use crate::bus::Sender as BusSender;
use crate::state::Message;

/// Cache entry, keyed in `players` by bus name (what NameOwnerChanged uses).
struct CachedPlayer {
    pid: u32,
    info: PlayerInfo,
    /// PropertiesChanged subscription; dropping it deregisters the listener.
    _props_sub: gio::SignalSubscription,
}

/// Hard cap on tracked players. A session never runs anywhere near this
/// many media players; past it, new ones are dropped so a buggy or hostile
/// peer spamming bus names can't grow the cache without bound.
const MAX_PLAYERS: usize = 32;

/// Live MPRIS player cache, keyed by bus name. A session runs a handful of
/// players, so a linear-scan Vec beats a hash map at this size and stays
/// bounded by MAX_PLAYERS.
#[derive(Default)]
struct PlayerCache(Vec<(String, CachedPlayer)>);

impl PlayerCache {
    /// Insert or replace the entry for `name`. Returns false, dropping
    /// `player`, when the cache is full and `name` is not already present.
    fn insert(&mut self, name: String, player: CachedPlayer) -> bool {
        if let Some(slot) = self.0.iter_mut().find(|(n, _)| *n == name) {
            slot.1 = player;
            true
        } else if self.0.len() < MAX_PLAYERS {
            self.0.push((name, player));
            true
        } else {
            false
        }
    }

    /// Remove the entry for `name`, dropping it (and its subscription).
    /// True if one existed.
    fn remove(&mut self, name: &str) -> bool {
        if let Some(idx) = self.0.iter().position(|(n, _)| n.as_str() == name) {
            self.0.swap_remove(idx);
            true
        } else {
            false
        }
    }

    /// Mutable access to the entry for `name`, if present.
    fn get_mut(&mut self, name: &str) -> Option<&mut CachedPlayer> {
        self.0
            .iter_mut()
            .find(|(n, _)| n.as_str() == name)
            .map(|(_, p)| p)
    }

    /// Metadata for the player at `pid`, or `None`. Scans by PID since the
    /// cache is keyed by bus name.
    fn by_pid(&self, pid: u32) -> Option<&PlayerInfo> {
        self.0
            .iter()
            .find(|(_, p)| p.pid == pid)
            .map(|(_, p)| &p.info)
    }
}

/// Owns the live player cache and the bus subscription. The UI holds one for
/// the window's life and queries it with [`Mpris::resolve_title`]; dropping it
/// tears the listeners down.
pub struct Mpris {
    players: Rc<RefCell<PlayerCache>>,
    /// NameOwnerChanged subscription, `None` when the session bus was
    /// unavailable. Held so the listener lives as long as `Mpris`.
    _owner_sub: Option<gio::SignalSubscription>,
}

impl Mpris {
    /// Title for the player owning `audio_pid` or one of its `/proc`
    /// ancestors, or `None` if none matches or it has no title/artist.
    pub fn resolve_title(&self, audio_pid: u32) -> Option<String> {
        let players = self.players.borrow();
        let info = ancestor_pids(audio_pid).find_map(|pid| players.by_pid(pid))?;
        info.display()
    }
}

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const PLAYER_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";

/// Attach to the session bus and start tracking MPRIS players, returning a
/// handle the UI queries for titles. If the session bus is unavailable the
/// handle is inert (every lookup misses) and the app runs without enrichment.
pub fn init(tx: BusSender<Message>) -> Mpris {
    let players: Rc<RefCell<PlayerCache>> = Rc::new(RefCell::new(PlayerCache::default()));

    let connection = match gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("mpris: skip session bus unavailable: {e}");
            return Mpris {
                players,
                _owner_sub: None,
            };
        }
    };

    // Subscribe before priming so a player appearing between ListNames and
    // the subscription isn't missed. arg0-prefix filtering isn't available,
    // so we filter in the callback.
    let conn_for_sub = connection.clone();
    let players_for_sub = Rc::clone(&players);
    let tx_for_sub = tx.clone();
    // Held in the returned Mpris so it lives as long as the handle; dropping
    // Mpris (on window close) tears the listener down.
    let owner_sub = connection.subscribe_to_signal(
        Some("org.freedesktop.DBus"),
        Some("org.freedesktop.DBus"),
        Some("NameOwnerChanged"),
        Some("/org/freedesktop/DBus"),
        None,
        gio::DBusSignalFlags::NONE,
        move |signal| {
            let Some((name, old_owner, new_owner)) =
                signal.parameters.get::<(String, String, String)>()
            else {
                return;
            };
            if !name.starts_with(MPRIS_PREFIX) {
                return;
            }
            if !new_owner.is_empty() {
                // Player appeared or owner changed: detach then reattach.
                detach_player(&players_for_sub, &name);
                attach_player(
                    &conn_for_sub,
                    Rc::clone(&players_for_sub),
                    tx_for_sub.clone(),
                    name,
                );
            } else if !old_owner.is_empty() {
                // Player went away.
                if detach_player(&players_for_sub, &name) {
                    notify(&tx_for_sub);
                }
            }
        },
    );

    // Initial players: list every bus name, filter to MPRIS, attach each.
    match connection.call_sync(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "ListNames",
        None,
        Some(glib::VariantTy::new("(as)").expect("static signature")),
        gio::DBusCallFlags::NONE,
        -1,
        gio::Cancellable::NONE,
    ) {
        Ok(reply) => {
            if let Some((names,)) = reply.get::<(Vec<String>,)>() {
                for name in names {
                    if name.starts_with(MPRIS_PREFIX) {
                        attach_player(&connection, Rc::clone(&players), tx.clone(), name);
                    }
                }
                notify(&tx);
            }
        }
        Err(e) => {
            eprintln!("mpris: ListNames failed: {e}");
        }
    }

    Mpris {
        players,
        _owner_sub: Some(owner_sub),
    }
}

/// Bind one player: resolve PID, fetch Metadata, install a PropertiesChanged
/// listener. Best-effort; any DBus failure leaves the player out of the cache.
fn attach_player(
    connection: &gio::DBusConnection,
    players: Rc<RefCell<PlayerCache>>,
    tx: BusSender<Message>,
    name: String,
) {
    let pid = match get_connection_pid(connection, &name) {
        Some(p) => p,
        None => return,
    };
    let info = fetch_metadata(connection, &name).unwrap_or_default();

    let players_for_props = Rc::clone(&players);
    let tx_for_props = tx.clone();
    let name_for_props = name.clone();
    let sub = connection.subscribe_to_signal(
        Some(&name),
        Some("org.freedesktop.DBus.Properties"),
        Some("PropertiesChanged"),
        Some(PLAYER_PATH),
        Some(PLAYER_IFACE),
        gio::DBusSignalFlags::NONE,
        move |signal| {
            // Reread full Metadata via Get rather than diffing the signal
            // payload: players sometimes list Metadata in
            // invalidated_properties instead of changed_properties.
            let info = fetch_metadata(signal.connection, &name_for_props).unwrap_or_default();
            let changed = {
                let mut map = players_for_props.borrow_mut();
                if let Some(entry) = map.get_mut(&name_for_props) {
                    if entry.info != info {
                        entry.info = info;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            };
            if changed {
                notify(&tx_for_props);
            }
        },
    );

    let inserted = players.borrow_mut().insert(
        name,
        CachedPlayer {
            pid,
            info,
            _props_sub: sub,
        },
    );
    if inserted {
        notify(&tx);
    } else {
        eprintln!("mpris: player cache full ({MAX_PLAYERS}), ignoring new player");
    }
}

/// Drop the cache entry (and its subscription). Returns true if one existed.
fn detach_player(players: &Rc<RefCell<PlayerCache>>, name: &str) -> bool {
    players.borrow_mut().remove(name)
}

/// Nudge the UI to re-read the cache on its next refresh. The cache lives in
/// the UI's `Mpris` handle, so the message carries no data.
fn notify(tx: &BusSender<Message>) {
    let _ = tx.send(Message::MprisChanged);
}

/// `GetConnectionUnixProcessID` on the bus daemon. `None` on any error (e.g.
/// the connection vanished, a race on rapid app churn).
fn get_connection_pid(connection: &gio::DBusConnection, bus_name: &str) -> Option<u32> {
    let reply = connection
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "GetConnectionUnixProcessID",
            Some(&(bus_name,).to_variant()),
            Some(glib::VariantTy::new("(u)").expect("static signature")),
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
        )
        .ok()?;
    reply.get::<(u32,)>().map(|(p,)| p)
}

/// Read the player's Metadata via `Properties.Get`. `None` if the call fails
/// or the player lacks the Player interface.
fn fetch_metadata(connection: &gio::DBusConnection, bus_name: &str) -> Option<PlayerInfo> {
    let reply = connection
        .call_sync(
            Some(bus_name),
            PLAYER_PATH,
            "org.freedesktop.DBus.Properties",
            "Get",
            Some(&(PLAYER_IFACE, "Metadata").to_variant()),
            Some(glib::VariantTy::new("(v)").expect("static signature")),
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
        )
        .ok()?;
    let (boxed,) = reply.get::<(glib::Variant,)>()?;
    Some(parse_metadata_dict(&boxed))
}

/// Unbox a `v`-typed Variant one level, otherwise return it as-is. gtk-rs
/// sometimes already hands back the inner value; calling `as_variant` then
/// would emit a C-level `g_variant_get_variant` CRITICAL to stderr, which
/// spams on frequent PropertiesChanged.
fn unbox_variant(v: glib::Variant) -> glib::Variant {
    if v.type_().as_str() == "v" {
        v.as_variant().unwrap_or(v)
    } else {
        v
    }
}

/// Parse the `a{sv}` Metadata dict into `PlayerInfo`, ignoring unused keys.
/// `xesam:title`/`xesam:artist` are optional; omitting them yields a default.
fn parse_metadata_dict(boxed: &glib::Variant) -> PlayerInfo {
    let mut info = PlayerInfo::default();
    // Properties.Get returns a `v`; unbox once to reach the `a{sv}` dict.
    let dict = unbox_variant(boxed.clone());
    let n = dict.n_children();
    for i in 0..n {
        let entry = dict.child_value(i);
        // Each entry is {sv}: child 0 = key, child 1 = value. Skip anything
        // malformed rather than panic.
        if entry.n_children() < 2 {
            continue;
        }
        let key = entry.child_value(0).get::<String>().unwrap_or_default();
        let unboxed = unbox_variant(entry.child_value(1));
        match key.as_str() {
            "xesam:title" => {
                if let Some(s) = unboxed.get::<String>()
                    && !s.is_empty()
                {
                    info.title = Some(s);
                }
            }
            "xesam:artist" => {
                if let Some(arr) = unboxed.get::<Vec<String>>()
                    && let Some(first) = arr.into_iter().find(|s| !s.is_empty())
                {
                    info.artist = Some(first);
                }
            }
            _ => {}
        }
    }
    info
}

/// Resolved metadata for one MPRIS player. The row label only needs
/// title + artist, so other spec fields are discarded at parse time.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlayerInfo {
    /// `xesam:title`, usually the song / video title.
    pub title: Option<String>,
    /// First entry of `xesam:artist` (the spec allows an array).
    pub artist: Option<String>,
}

impl PlayerInfo {
    /// Label "Title · Artist" / "Title" / "Artist", or `None` if both
    /// fields are empty. Caller hard-truncates to the label width.
    pub fn display(&self) -> Option<String> {
        match (self.title.as_deref(), self.artist.as_deref()) {
            (Some(t), Some(a)) => Some(format!("{t} · {a}")),
            (Some(t), None) => Some(t.to_string()),
            (None, Some(a)) => Some(a.to_string()),
            (None, None) => None,
        }
    }
}

/// The PIDs to try when matching an audio stream to a player: `audio_pid`
/// then its `/proc` ancestors, up to [`MAX_ANCESTOR_DEPTH`], stopping before
/// PID 1 so an unrelated higher-up player can't match. The walk is needed
/// because Chromium-family browsers route all tab audio through one
/// AudioService child while registering MPRIS from the main process, so the
/// player is an ancestor of the audio PID.
fn ancestor_pids(audio_pid: u32) -> impl Iterator<Item = u32> {
    std::iter::successors(Some(audio_pid), |&pid| parent_pid(pid).filter(|&p| p > 1))
        .take(MAX_ANCESTOR_DEPTH)
}

/// Upper bound on `/proc/<pid>/stat` reads per [`ancestor_pids`] walk.
const MAX_ANCESTOR_DEPTH: usize = 8;

/// Read the parent PID from `/proc/<pid>/stat`, `None` if unreadable or
/// gone. The `comm` field may contain spaces/parens, so split on the
/// LAST `)` to find the fields that follow.
fn parent_pid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, after_comm) = stat.rsplit_once(')')?;
    let mut fields = after_comm.split_whitespace();
    let _state = fields.next()?;
    let ppid = fields.next()?;
    ppid.parse::<u32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_combines_title_and_artist() {
        let info = PlayerInfo {
            title: Some("Song".into()),
            artist: Some("Band".into()),
        };
        assert_eq!(info.display().as_deref(), Some("Song · Band"));
    }

    #[test]
    fn display_falls_back_to_whichever_field_is_set() {
        assert_eq!(
            PlayerInfo {
                title: Some("Only Title".into()),
                artist: None,
            }
            .display()
            .as_deref(),
            Some("Only Title"),
        );
        assert_eq!(
            PlayerInfo {
                title: None,
                artist: Some("Only Artist".into()),
            }
            .display()
            .as_deref(),
            Some("Only Artist"),
        );
    }

    #[test]
    fn display_none_when_both_empty() {
        assert_eq!(PlayerInfo::default().display(), None);
    }

    #[test]
    fn parent_pid_resolves_for_self() {
        // Parses a real /proc/self/stat to prove the parser handles
        // live input.
        let me = std::process::id();
        let parent = parent_pid(me).expect("parent_pid of self resolves");
        assert!(parent > 0, "parent PID must be positive");
    }

    #[test]
    fn parent_pid_handles_comm_with_paren() {
        // comm with parens/spaces must survive the rsplit-on-')' parse.
        // Format: "<pid> (<comm>) <state> <ppid> ..."
        let fake = "42 (a (tricky) name) S 17 1 1 0 -1 ...";
        let (_, after) = fake.rsplit_once(')').unwrap();
        let mut fields = after.split_whitespace();
        assert_eq!(fields.next(), Some("S"));
        assert_eq!(fields.next(), Some("17"));
    }

    #[test]
    fn ancestor_pids_starts_at_self_and_stays_bounded() {
        let me = std::process::id();
        let chain: Vec<u32> = ancestor_pids(me).collect();
        assert_eq!(chain.first(), Some(&me), "walk starts at the given PID");
        assert!(chain.len() <= MAX_ANCESTOR_DEPTH, "respects the depth cap");
        assert!(chain.iter().all(|&p| p > 1), "never includes PID 1 or 0");
    }
}
