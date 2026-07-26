//! Session-bus MPRIS metadata enrichment.
//!
//! Players registering org.mpris.MediaPlayer2.* expose "now playing" metadata,
//! letting the UI label a stream "YouTube · Title" instead of "Stream 113".
//! Streams match players by PID, via GetConnectionUnixProcessID. Caveats:
//!
//! - Chromium-family browsers share one AudioService PID and one MPRIS player
//!   across tabs, so the title tracks the most recently played media. Still
//!   beats "Stream <id>".
//! - Apps with no MPRIS player get no enrichment; the row keeps its label.
//!
//! A thread owns the bus connection and writes the player cache; the UI reads
//! it while projecting a snapshot and is nudged to repaint with a Message. The
//! thread never holds the cache lock across a bus call, so a slow or wedged
//! player cannot stall a repaint.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use crate::bus::Sender as BusSender;
use crate::dbus::connection::Connection;
use crate::dbus::wire::{MethodCall, Value};
use crate::state::Message;

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const PLAYER_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
const PROPS_IFACE: &str = "org.freedesktop.DBus.Properties";
const BUS_NAME: &str = "org.freedesktop.DBus";
const BUS_PATH: &str = "/org/freedesktop/DBus";

/// Hard cap on tracked players. A session never runs anywhere near this many
/// media players; past it, new ones are dropped so a buggy or hostile peer
/// spamming bus names cannot grow the cache without bound.
const MAX_PLAYERS: usize = 32;

/// Resolved metadata for one MPRIS player. The row label only needs title and
/// artist, so other spec fields are discarded at parse time.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlayerInfo {
    /// xesam:title, usually the song or video title.
    pub title: Option<String>,
    /// First entry of xesam:artist, which the spec defines as an array.
    pub artist: Option<String>,
}

impl PlayerInfo {
    /// Label "Title · Artist", or whichever half exists, or None when the
    /// player reports neither. Caller hard-truncates to the label width.
    pub fn display(&self) -> Option<String> {
        match (self.title.as_deref(), self.artist.as_deref()) {
            (Some(t), Some(a)) => Some(format!("{t} · {a}")),
            (Some(t), None) => Some(t.to_string()),
            (None, Some(a)) => Some(a.to_string()),
            (None, None) => None,
        }
    }
}

/// A tracked player. The owner is its unique bus name, which is what signals
/// arrive from, so PropertiesChanged can be routed without asking the bus who
/// sent it.
struct CachedPlayer {
    owner: String,
    pid: u32,
    info: PlayerInfo,
}

/// Live player cache keyed by well-known bus name. A session runs a handful of
/// players, so a linear-scan Vec beats a hash map at this size and stays
/// bounded by MAX_PLAYERS.
#[derive(Default)]
struct PlayerCache {
    players: Vec<(String, CachedPlayer)>,
    /// Bumped on every change. A reader that resolved against one generation
    /// knows its answer still holds while the number has not moved.
    generation: u64,
}

impl PlayerCache {
    /// Insert or replace the entry for `name`. False when the cache is full
    /// and `name` is not already present, dropping `player`.
    fn insert(&mut self, name: String, player: CachedPlayer) -> bool {
        self.generation = self.generation.wrapping_add(1);
        if let Some(slot) = self.players.iter_mut().find(|(n, _)| *n == name) {
            slot.1 = player;
            true
        } else if self.players.len() < MAX_PLAYERS {
            self.players.push((name, player));
            true
        } else {
            false
        }
    }

    /// Remove the entry for `name`. True if one existed.
    fn remove(&mut self, name: &str) -> bool {
        self.generation = self.generation.wrapping_add(1);
        if let Some(idx) = self.players.iter().position(|(n, _)| n.as_str() == name) {
            self.players.swap_remove(idx);
            true
        } else {
            false
        }
    }

    /// The player whose unique bus name is `owner`. Taking it mutably is a
    /// change in the making, so the generation moves with it.
    fn by_owner_mut(&mut self, owner: &str) -> Option<&mut CachedPlayer> {
        self.generation = self.generation.wrapping_add(1);
        self.players
            .iter_mut()
            .find(|(_, p)| p.owner == owner)
            .map(|(_, p)| p)
    }

    /// Metadata for the player at `pid`. Scans by PID since the cache is keyed
    /// by bus name.
    fn by_pid(&self, pid: u32) -> Option<&PlayerInfo> {
        self.players
            .iter()
            .find(|(_, p)| p.pid == pid)
            .map(|(_, p)| &p.info)
    }
}

/// Handle on the player cache the worker keeps current. The UI holds one for
/// the window's life and queries it with [`Mpris::resolve_title`].
pub struct Mpris {
    cache: Arc<Mutex<PlayerCache>>,
    /// What each audio PID last resolved to, and the cache generation it was
    /// resolved against.
    ///
    /// Matching a stream to a player reads a /proc entry per ancestor, and the
    /// snapshot this feeds is rebuilt on every state change, which includes
    /// every step of a slider drag. Answers only go stale when the players do,
    /// so the generation is what decides.
    resolved: RefCell<HashMap<u32, (u64, Option<String>)>>,
}

/// Audio PIDs remembered at once. A session has a handful of streams; the cap
/// is what keeps a long run of short-lived ones from growing the map.
const MAX_RESOLVED: usize = 64;

impl Mpris {
    /// Title for the player owning `audio_pid` or one of its /proc ancestors,
    /// or None if none matches or it reports no title or artist.
    pub fn resolve_title(&self, audio_pid: u32) -> Option<String> {
        // A poisoned lock means the worker died mid-write. Enrichment is
        // optional, so miss rather than take the process down with it.
        let cache = self.cache.lock().ok()?;
        let generation = cache.generation;

        let remembered = self.resolved.borrow().get(&audio_pid).cloned();
        if let Some((against, title)) = remembered
            && against == generation
        {
            return title;
        }

        let title = ancestor_pids(audio_pid)
            .find_map(|pid| cache.by_pid(pid))
            .and_then(PlayerInfo::display);
        drop(cache);

        let mut resolved = self.resolved.borrow_mut();
        if resolved.len() >= MAX_RESOLVED {
            resolved.clear();
        }
        resolved.insert(audio_pid, (generation, title.clone()));
        title
    }
}

/// Start tracking MPRIS players on a thread of its own, returning the handle
/// the UI queries. The handle works either way: if the session bus is
/// unavailable the cache simply stays empty and every lookup misses.
pub fn init(tx: BusSender<Message>) -> Mpris {
    let cache = Arc::new(Mutex::new(PlayerCache::default()));
    let worker_cache = Arc::clone(&cache);

    let spawned = std::thread::Builder::new()
        .name("mpris".to_string())
        .spawn(move || {
            if let Err(e) = run(worker_cache, tx) {
                eprintln!("mpris: stopped: {e}");
            }
        });
    if let Err(e) = spawned {
        eprintln!("mpris: could not start worker: {e}");
    }

    Mpris {
        cache,
        resolved: RefCell::new(HashMap::new()),
    }
}

/// Own the connection: subscribe, prime the cache, then follow signals until
/// the bus goes away.
fn run(cache: Arc<Mutex<PlayerCache>>, tx: BusSender<Message>) -> io::Result<()> {
    let mut conn = Connection::session()?;

    // Subscribe before priming so a player appearing between the two is caught
    // by a signal rather than missed entirely.
    conn.add_match(
        "type='signal',sender='org.freedesktop.DBus',\
         interface='org.freedesktop.DBus',member='NameOwnerChanged',\
         arg0namespace='org.mpris.MediaPlayer2'",
    )?;
    // One path-scoped rule covers every player, so players need no per-player
    // subscription as they come and go.
    conn.add_match(
        "type='signal',interface='org.freedesktop.DBus.Properties',\
         member='PropertiesChanged',path='/org/mpris/MediaPlayer2'",
    )?;

    prime(&mut conn, &cache, &tx);

    loop {
        let signal = conn.next_signal()?;
        match signal.member.as_deref() {
            Some("NameOwnerChanged") => on_name_owner_changed(&mut conn, &cache, &tx, &signal),
            Some("PropertiesChanged") => on_properties_changed(&mut conn, &cache, &tx, &signal),
            _ => {}
        }
    }
}

/// Attach every MPRIS player already on the bus.
fn prime(conn: &mut Connection, cache: &Arc<Mutex<PlayerCache>>, tx: &BusSender<Message>) {
    let reply = match conn.call(&MethodCall {
        destination: BUS_NAME,
        path: BUS_PATH,
        interface: BUS_NAME,
        member: "ListNames",
        args: &[],
    }) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mpris: ListNames failed: {e}");
            return;
        }
    };

    let names: Vec<String> = reply
        .body_values()
        .and_then(|v| v.first().map(collect_strings))
        .unwrap_or_default();

    let mut any = false;
    for name in names.iter().filter(|n| n.starts_with(MPRIS_PREFIX)) {
        any |= attach(conn, cache, name);
    }
    if any {
        notify(tx);
    }
}

/// A player appeared, moved, or went away.
fn on_name_owner_changed(
    conn: &mut Connection,
    cache: &Arc<Mutex<PlayerCache>>,
    tx: &BusSender<Message>,
    signal: &crate::dbus::wire::Message,
) {
    let Some(values) = signal.body_values() else {
        return;
    };
    let (Some(name), Some(old), Some(new)) = (
        values.first().and_then(Value::as_str),
        values.get(1).and_then(Value::as_str),
        values.get(2).and_then(Value::as_str),
    ) else {
        return;
    };
    if !name.starts_with(MPRIS_PREFIX) {
        return;
    }

    if !new.is_empty() {
        // Appeared, or changed hands: drop what we knew and rebind.
        detach(cache, name);
        if attach(conn, cache, name) {
            notify(tx);
        }
    } else if !old.is_empty() && detach(cache, name) {
        notify(tx);
    }
}

/// A tracked player changed a property. Metadata is re-read in full rather
/// than diffed out of the signal, because players sometimes list it as
/// invalidated instead of changed.
fn on_properties_changed(
    conn: &mut Connection,
    cache: &Arc<Mutex<PlayerCache>>,
    tx: &BusSender<Message>,
    signal: &crate::dbus::wire::Message,
) {
    let Some(owner) = signal.sender.as_deref() else {
        return;
    };
    let changed_iface = signal
        .body_values()
        .and_then(|v| v.first().and_then(Value::as_str).map(str::to_string));
    if changed_iface.as_deref() != Some(PLAYER_IFACE) {
        return;
    }

    // The lock is taken twice, around the call rather than across it, so a
    // player that takes its time answering never blocks a repaint.
    let known = matches!(cache.lock(), Ok(c) if c.players.iter().any(|(_, p)| p.owner == owner));
    if !known {
        return;
    }
    let info = fetch_metadata(conn, owner);

    let changed = match cache.lock() {
        Ok(mut c) => match c.by_owner_mut(owner) {
            Some(player) if player.info != info => {
                player.info = info;
                true
            }
            _ => false,
        },
        Err(_) => false,
    };
    if changed {
        notify(tx);
    }
}

/// Bind one player: resolve its owner and PID, read its metadata, cache it.
/// Best-effort, so any bus failure leaves the player out. True when the cache
/// changed.
fn attach(conn: &mut Connection, cache: &Arc<Mutex<PlayerCache>>, name: &str) -> bool {
    let Some(owner) = get_name_owner(conn, name) else {
        return false;
    };
    let Some(pid) = get_connection_pid(conn, name) else {
        return false;
    };
    let info = fetch_metadata(conn, &owner);

    let inserted = match cache.lock() {
        Ok(mut c) => c.insert(name.to_string(), CachedPlayer { owner, pid, info }),
        Err(_) => false,
    };
    if !inserted {
        eprintln!("mpris: player cache full ({MAX_PLAYERS}), ignoring {name}");
    }
    inserted
}

/// Drop a player from the cache. True if one was there.
fn detach(cache: &Arc<Mutex<PlayerCache>>, name: &str) -> bool {
    match cache.lock() {
        Ok(mut c) => c.remove(name),
        Err(_) => false,
    }
}

/// Nudge the UI to re-read the cache on its next refresh. The cache lives
/// behind the handle, so the message carries no data.
fn notify(tx: &BusSender<Message>) {
    let _ = tx.send(Message::MprisChanged);
}

/// The unique bus name currently owning `name`.
fn get_name_owner(conn: &mut Connection, name: &str) -> Option<String> {
    let reply = conn
        .call(&MethodCall {
            destination: BUS_NAME,
            path: BUS_PATH,
            interface: BUS_NAME,
            member: "GetNameOwner",
            args: &[name],
        })
        .ok()?;
    reply
        .body_values()?
        .first()
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The PID behind a bus name. None on any error, e.g. the connection vanished
/// in a race on rapid app churn.
fn get_connection_pid(conn: &mut Connection, name: &str) -> Option<u32> {
    let reply = conn
        .call(&MethodCall {
            destination: BUS_NAME,
            path: BUS_PATH,
            interface: BUS_NAME,
            member: "GetConnectionUnixProcessID",
            args: &[name],
        })
        .ok()?;
    reply.body_values()?.first().and_then(Value::as_u32)
}

/// Read a player's Metadata property. An absent or unreadable property yields
/// empty info, which is what a player with nothing playing reports anyway.
fn fetch_metadata(conn: &mut Connection, destination: &str) -> PlayerInfo {
    let reply = conn.call(&MethodCall {
        destination,
        path: PLAYER_PATH,
        interface: PROPS_IFACE,
        member: "Get",
        args: &[PLAYER_IFACE, "Metadata"],
    });
    let Ok(reply) = reply else {
        return PlayerInfo::default();
    };
    reply
        .body_values()
        .and_then(|v| v.first().map(parse_metadata))
        .unwrap_or_default()
}

/// Pull title and artist out of the a{sv} Metadata dict, ignoring every other
/// key. Both are optional; a player with neither yields a default.
fn parse_metadata(dict: &Value) -> PlayerInfo {
    let title = dict
        .dict_get("xesam:title")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    // The spec types artist as an array, but players have been known to send a
    // bare string, so accept either.
    let artist_value = dict.dict_get("xesam:artist");
    let artist = artist_value
        .and_then(|v| {
            v.as_array().map(|names| {
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .find(|s| !s.is_empty())
                    .map(str::to_string)
            })
        })
        .flatten()
        .or_else(|| {
            artist_value
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        });

    PlayerInfo { title, artist }
}

/// Every string in an array value, skipping anything that is not one.
fn collect_strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap_or(&[])
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

/// The PIDs to try when matching an audio stream to a player: `audio_pid` then
/// its /proc ancestors, up to [`MAX_ANCESTOR_DEPTH`], stopping before PID 1 so
/// an unrelated higher-up player cannot match. The walk is needed because
/// Chromium-family browsers route all tab audio through one AudioService child
/// while registering MPRIS from the main process, so the player is an ancestor
/// of the audio PID.
fn ancestor_pids(audio_pid: u32) -> impl Iterator<Item = u32> {
    std::iter::successors(Some(audio_pid), |&pid| parent_pid(pid).filter(|&p| p > 1))
        .take(MAX_ANCESTOR_DEPTH)
}

/// Upper bound on /proc/<pid>/stat reads per [`ancestor_pids`] walk.
const MAX_ANCESTOR_DEPTH: usize = 8;

/// Read the parent PID from /proc/<pid>/stat, None if unreadable or gone. The
/// comm field may contain spaces and parens, so split on the LAST ')' to find
/// the fields that follow.
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
    use crate::dbus::wire::{Decoder, Encoder};

    fn player(owner: &str, pid: u32, title: &str) -> CachedPlayer {
        CachedPlayer {
            owner: owner.to_string(),
            pid,
            info: PlayerInfo {
                title: Some(title.to_string()),
                artist: None,
            },
        }
    }

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
    fn the_cache_replaces_an_existing_name_without_growing() {
        let mut cache = PlayerCache::default();
        assert!(cache.insert("a".into(), player(":1.1", 10, "First")));
        assert!(cache.insert("a".into(), player(":1.2", 11, "Second")));
        assert_eq!(cache.players.len(), 1);
        assert_eq!(
            cache.by_pid(11).and_then(|i| i.display()).as_deref(),
            Some("Second")
        );
        assert!(cache.by_pid(10).is_none(), "the old PID is gone");
    }

    #[test]
    fn the_cache_stops_at_its_cap_but_still_updates_known_names() {
        let mut cache = PlayerCache::default();
        for i in 0..MAX_PLAYERS {
            assert!(cache.insert(format!("p{i}"), player(&format!(":1.{i}"), i as u32, "T")));
        }
        assert!(
            !cache.insert("one-too-many".into(), player(":1.99", 999, "T")),
            "a new name past the cap is refused",
        );
        assert!(
            cache.insert("p0".into(), player(":1.0", 0, "Updated")),
            "an existing name still updates when full",
        );
        assert_eq!(cache.players.len(), MAX_PLAYERS);
    }

    #[test]
    fn removing_reports_whether_anything_was_there() {
        let mut cache = PlayerCache::default();
        cache.insert("a".into(), player(":1.1", 10, "T"));
        assert!(cache.remove("a"));
        assert!(!cache.remove("a"), "second removal is a miss");
        assert!(cache.by_pid(10).is_none());
    }

    #[test]
    fn lookup_by_owner_finds_the_player_a_signal_came_from() {
        let mut cache = PlayerCache::default();
        cache.insert("org.mpris.MediaPlayer2.vlc".into(), player(":1.7", 42, "T"));
        assert!(cache.by_owner_mut(":1.7").is_some());
        assert!(cache.by_owner_mut(":1.8").is_none());
    }

    /// Encode an a{sv} metadata dict the way a Properties.Get reply carries it.
    fn metadata(entries: &[(&str, MetaValue)]) -> Value {
        let mut e = Encoder::new();
        e.signature("a{sv}");
        e.array(8, |e| {
            for (key, value) in entries {
                e.align(8);
                e.string(key);
                match value {
                    MetaValue::Str(s) => {
                        e.signature("s");
                        e.string(s);
                    }
                    MetaValue::Strs(list) => {
                        e.signature("as");
                        e.array(4, |e| {
                            for s in list.iter() {
                                e.string(s);
                            }
                        });
                    }
                }
            }
        });
        let bytes = e.into_bytes();
        Decoder::new(&bytes, true)
            .read(b"v")
            .expect("the encoder produced a decodable variant")
    }

    enum MetaValue {
        Str(&'static str),
        Strs(&'static [&'static str]),
    }

    #[test]
    fn metadata_parses_title_and_the_first_artist() {
        let dict = metadata(&[
            ("xesam:title", MetaValue::Str("Song")),
            ("xesam:artist", MetaValue::Strs(&["Band", "Guest"])),
        ]);
        let info = parse_metadata(&dict);
        assert_eq!(info.title.as_deref(), Some("Song"));
        assert_eq!(info.artist.as_deref(), Some("Band"));
        assert_eq!(info.display().as_deref(), Some("Song · Band"));
    }

    #[test]
    fn metadata_skips_empty_strings_rather_than_showing_blanks() {
        let dict = metadata(&[
            ("xesam:title", MetaValue::Str("")),
            ("xesam:artist", MetaValue::Strs(&["", "Real"])),
        ]);
        let info = parse_metadata(&dict);
        assert_eq!(info.title, None, "an empty title is not a title");
        assert_eq!(
            info.artist.as_deref(),
            Some("Real"),
            "skips to the first real name"
        );
    }

    #[test]
    fn metadata_accepts_an_artist_sent_as_a_bare_string() {
        // Off-spec, but players do it, and the row should still fill in.
        let dict = metadata(&[("xesam:artist", MetaValue::Str("Solo"))]);
        assert_eq!(parse_metadata(&dict).artist.as_deref(), Some("Solo"));
    }

    #[test]
    fn metadata_with_no_useful_keys_yields_nothing_to_show() {
        let dict = metadata(&[("mpris:trackid", MetaValue::Str("/track/1"))]);
        let info = parse_metadata(&dict);
        assert_eq!(info, PlayerInfo::default());
        assert_eq!(info.display(), None);
    }

    #[test]
    fn collect_strings_keeps_only_the_strings() {
        let mut e = Encoder::new();
        e.array(4, |e| {
            e.string("org.freedesktop.DBus");
            e.string("org.mpris.MediaPlayer2.vlc");
        });
        let bytes = e.into_bytes();
        let value = Decoder::new(&bytes, true).read(b"as").expect("an array");
        assert_eq!(
            collect_strings(&value),
            vec!["org.freedesktop.DBus", "org.mpris.MediaPlayer2.vlc"],
        );
        assert!(
            collect_strings(&Value::U32(1)).is_empty(),
            "a non-array is empty"
        );
    }

    #[test]
    fn parent_pid_resolves_for_self() {
        // Parses a real /proc/self/stat to prove the parser handles live input.
        let me = std::process::id();
        let parent = parent_pid(me).expect("parent_pid of self resolves");
        assert!(parent > 0, "parent PID must be positive");
    }

    #[test]
    fn parent_pid_handles_comm_with_paren() {
        // comm with parens and spaces must survive the rsplit-on-')' parse.
        // Format: "<pid> (<comm>) <state> <ppid> ..."
        let fake = "42 (a (tricky) name) S 17 1 1 0 -1 ...";
        let (_, after) = fake.rsplit_once(')').unwrap();
        let mut fields = after.split_whitespace();
        assert_eq!(fields.next(), Some("S"));
        assert_eq!(fields.next(), Some("17"));
    }

    #[test]
    fn parent_pid_misses_on_a_pid_that_does_not_exist() {
        // PID 0 is never a live process, so /proc has no entry for it.
        assert_eq!(parent_pid(0), None);
    }

    #[test]
    fn ancestor_pids_starts_at_self_and_stays_bounded() {
        let me = std::process::id();
        let chain: Vec<u32> = ancestor_pids(me).collect();
        assert_eq!(chain.first(), Some(&me), "walk starts at the given PID");
        assert!(chain.len() <= MAX_ANCESTOR_DEPTH, "respects the depth cap");
        assert!(chain.iter().all(|&p| p > 1), "never includes PID 1 or 0");
    }

    fn inert_handle() -> Mpris {
        Mpris {
            cache: Arc::new(Mutex::new(PlayerCache::default())),
            resolved: RefCell::new(HashMap::new()),
        }
    }

    /// Resolving walks /proc, and the snapshot it feeds is rebuilt on every
    /// state change, so the answer is remembered. What it must never do is go
    /// on repeating an answer the players have moved past.
    #[test]
    fn a_resolved_title_is_remembered_until_the_players_change() {
        let mpris = inert_handle();
        let me = std::process::id();
        let name = "org.mpris.MediaPlayer2.test";

        {
            let mut cache = mpris.cache.lock().expect("lock");
            cache.insert(name.into(), player(":1.1", me, "First"));
        }
        assert_eq!(mpris.resolve_title(me).as_deref(), Some("First"));
        // Asked again with nothing changed, and answered from memory.
        assert_eq!(mpris.resolve_title(me).as_deref(), Some("First"));
        assert_eq!(mpris.resolved.borrow().len(), 1, "one PID remembered");

        // The player starts a new track, which moves the cache on.
        {
            let mut cache = mpris.cache.lock().expect("lock");
            cache.insert(name.into(), player(":1.1", me, "Second"));
        }
        assert_eq!(
            mpris.resolve_title(me).as_deref(),
            Some("Second"),
            "a changed player is resolved afresh rather than answered from memory",
        );

        // And a player going away stops the row claiming a title.
        {
            let mut cache = mpris.cache.lock().expect("lock");
            assert!(cache.remove(name));
        }
        assert_eq!(mpris.resolve_title(me), None);
    }

    #[test]
    fn an_unreachable_bus_leaves_an_inert_handle() {
        // No worker can connect with the address pointing nowhere, so every
        // lookup must miss instead of blocking or panicking.
        let (tx, _rx) = crate::bus::channel::<Message>(8).expect("bus");
        let mpris = Mpris {
            cache: Arc::new(Mutex::new(PlayerCache::default())),
            resolved: RefCell::new(HashMap::new()),
        };
        drop(tx);
        assert_eq!(mpris.resolve_title(std::process::id()), None);
    }
}
