use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use crate::command_palette;
use crate::domain::{
    MAX_VOLUME, Section, SectionFilter, Stream as AudioStream, StreamKind, cubic_to_linear,
    linear_to_cubic,
};
use crate::geometry::Geometry;
use crate::pipewire_worker::{self as worker, Command, Event as WorkerEvent};
use crate::profile::{AppSettings, DeviceSettings, Pending, Profile, ProfileStore};
use crate::store;

/// Row-collapse key for an application stream. Streams sharing an
/// `app_identity()` collapse into one row; those without identifying
/// props fall back to a synthetic `node:<id>` key.
pub fn app_row_key(s: &AudioStream) -> String {
    s.app_identity().unwrap_or_else(|| format!("node:{}", s.id))
}

/// Node ids of every application stream in the row keyed by `key`.
pub fn members_of_app_row(state: &App, key: &str) -> Vec<u32> {
    state
        .streams
        .iter()
        .filter(|(_, s)| matches!(s.kind, StreamKind::Application))
        .filter(|(_, s)| app_row_key(s) == key)
        .map(|(id, _)| *id)
        .collect()
}

// Keep an application row visible after its backing Node disappears, so
// apps that destroy+recreate their stream (Chrome, media players) stay in
// the list and the user can pre-arm settings. The row persists until a
// fresh stream from the same app reclaims it via the StreamAdded eviction.

pub struct App {
    pub streams: BTreeMap<u32, AudioStream>,
    // Node ids whose backing Node is gone but whose row is still shown.
    // Cleared when a fresh stream from the same app reclaims the slot.
    pub tombstoned: HashSet<u32>,
    /// App-group row keys the user has expanded into per-member sub-rows.
    /// Empty by default. Stale entries are harmless: the UI only consults
    /// them for groups that still exist.
    pub expanded_groups: HashSet<String>,
    /// First-seen sequence number per app-stream node id. The UI sorts
    /// the app strip by this so rows keep their slot even when PipeWire
    /// assigns a fresh (higher) node id on reconnect. Tombstone-eviction
    /// copies the old id's seq to the new id.
    pub app_order: BTreeMap<u32, u64>,
    /// Monotonic counter for fresh app rows. Bumped only when a new row
    /// doesn't inherit a seq from an evicted tombstone; wrapping is fine.
    pub app_seq: u64,
    pub status: Option<String>,
    pub profiles: ProfileStore,
    /// Active-profile entries with no matching live stream at apply time.
    /// Consumed as matching streams arrive; cleared on profile switch or
    /// delete.
    pub pending: Pending,
    /// `Some(_)` while a profile management modal is visible. `ModalDismiss`
    /// clears it; `ModalConfirm` turns it into the real action.
    pub modal: Option<Modal>,
    /// Set by user-action messages that mutate live state. `AutoSaveTick`
    /// snapshots the active profile when it sees this, then clears it.
    /// Worker echoes don't dirty, so changes from other tools (pavucontrol)
    /// don't bleed into the profile.
    pub dirty: bool,
    /// Last-known window geometry, the header section of the state file.
    pub geometry: Geometry,
    /// Set when `GeometryChanged` moves the stored values. Parallel to
    /// `dirty` so a pure resize persists the header without re-snapshotting
    /// device state.
    pub geometry_dirty: bool,
    /// Where to persist the unified state file. `None` disables on-disk
    /// persistence (tests, or environments missing XDG_CONFIG_HOME and HOME).
    pub store_path: Option<PathBuf>,
    /// True while the Ctrl+K command palette overlay is visible.
    pub palette_open: bool,
    /// Live contents of the palette's search box. Cleared on close.
    pub palette_query: String,
    /// Highlighted-row index into the *filtered* command list, for Up/Down
    /// navigation and the row Enter executes.
    pub palette_selected: usize,
    /// Live IN/OUT/APP column visibility. Seeded from the active profile,
    /// flipped by `ToggleSection`, folded back on save so each profile
    /// reopens with its own layout. Read via [`App::shows_section`].
    pub section_filter: SectionFilter,
}

impl App {
    /// Whether `section`'s column group is currently shown, per the live
    /// section filter (which mirrors the active profile).
    pub fn shows_section(&self, section: Section) -> bool {
        self.section_filter.shows(section)
    }

    /// Whether the global-mute toggle reads as "on": at least one output
    /// sink exists and every one of them is muted. Derived from live state.
    pub fn all_outputs_muted(&self) -> bool {
        let mut any = false;
        for s in self.streams.values() {
            if matches!(s.kind, StreamKind::Sink) {
                any = true;
                if !s.muted {
                    return false;
                }
            }
        }
        any
    }
}

/// Profile-management modal state.
#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    /// Create a new profile from the current live state.
    CreateProfile { name: String, error: Option<String> },
    /// Rename `old_name` to `name`. Pre-filled with the existing name.
    RenameProfile {
        old_name: String,
        name: String,
        error: Option<String>,
    },
    /// Confirm deletion of `name`. No input field, just a Y/N gate.
    DeleteProfile { name: String },
}

#[derive(Debug, Clone)]
pub enum Message {
    // Boxed: the Stream payload dwarfs the other variants.
    Worker(Box<WorkerEvent>),
    VolumeChanged(u32, f32),
    MuteToggled(u32),
    /// Collapsed app-row slider drag. Proportional master: rescales every
    /// member by `cubic / V_old` (V_old = max of member cubics) so relative
    /// levels hold. When V_old is ~0 (all silent) just write `cubic` to all.
    GroupVolumeChanged {
        key: String,
        cubic: f32,
    },
    /// Collapsed app-row mute. If every member is muted, unmute all;
    /// otherwise mute all.
    GroupMuteToggled(String),
    /// Pin every member of a collapsed app row to `sink_id`.
    GroupSetStreamTarget {
        key: String,
        sink_id: u32,
    },
    /// Clear the per-stream pin on every member of a collapsed app row.
    GroupClearStreamTarget(String),
    /// Toggle an app row between collapsed aggregate and per-member
    /// sub-columns. No-op for single-member groups.
    GroupToggleExpanded(String),
    /// The MPRIS player cache changed; the UI re-reads it on the next
    /// refresh. Carries no data (the cache lives in the UI's Mpris handle).
    MprisChanged,
    MakeDefault(u32),
    /// Mark this input device the default audio source. Capture-side
    /// mirror of [`Self::MakeDefault`]; re-clicking the default is a no-op.
    MakeDefaultSource(u32),
    /// Toggle one column group's visibility and dirty the active profile.
    ToggleSection(Section),
    /// Mute or unmute every output sink in one gesture. If any output is
    /// unmuted it mutes all, otherwise unmutes all.
    MuteAllToggled,
    /// Pin an app stream's output to a specific sink. Re-clicking the
    /// active sink is a no-op; unpinning is `ClearStreamTarget`.
    SetStreamTarget {
        app_id: u32,
        sink_id: u32,
    },
    /// Clear the per-stream `target.object` override so the app follows
    /// the default sink again.
    ClearStreamTarget(u32),
    /// Clear every per-app pin in one click (the "Reset all" button).
    ResetAllStreamTargets,
    /// Apply the named profile: push each entry to matching live streams,
    /// set the default sink, and stash unmatched entries in `pending`.
    ApplyProfile(String),
    /// Remove a profile by name. If active, clears `pending` and the
    /// active marker.
    DeleteProfile(String),
    /// Drag-and-drop reorder: move `name` immediately before/after
    /// `target`. No-op reorders skip persistence.
    ReorderProfile {
        name: String,
        target: String,
        before: bool,
    },
    /// Open the "create profile" modal with an empty name buffer.
    OpenCreateProfileModal,
    /// Open the rename modal, pre-filled with the profile's current name.
    OpenRenameProfileModal(String),
    /// Open the delete-confirm modal for an existing profile.
    OpenDeleteProfileModal(String),
    /// Live edits in the modal's name field (create / rename).
    ModalNameChanged(String),
    /// Confirm the open modal. Empty/whitespace names keep it open with
    /// an inline error.
    ModalConfirm,
    /// Close the modal without applying its action.
    ModalDismiss,
    /// Debounce tick. Snapshots a dirty active profile and/or persists
    /// dirty geometry; otherwise a no-op.
    AutoSaveTick,
    /// Window resize / maximize notification. GTK4's `default-width`/
    /// `default-height` track the last normal-state size (they don't move
    /// while maximized/tiled), so this is the size to restore next launch.
    GeometryChanged {
        width: u32,
        height: u32,
        maximized: bool,
    },
    /// Toggle the Ctrl+K command palette, clearing query and selection.
    TogglePalette,
    /// Live edits in the palette's search field; resets selection to 0.
    PaletteQueryChanged(String),
    /// Move the highlighted palette row up, wrapping to the last row.
    PaletteSelectPrev,
    /// Move the highlighted palette row down, wrapping to 0.
    PaletteSelectNext,
}

pub fn boot() -> App {
    // Load state but don't auto-apply device settings: WirePlumber already
    // restores per-app volumes, and re-asserting them would surprise the
    // user. Pure view state (the section filter) is restored, no audio side
    // effects.
    let store_path = store::state_path();
    let (loaded, load_err) = match store_path.as_deref() {
        Some(p) => match store::load_from(p) {
            Ok(s) => (s, None),
            Err(e) => (store::State::default(), Some(format!("load state: {e:#}"))),
        },
        None => (store::State::default(), None),
    };
    let store::State {
        window: geometry,
        profiles,
    } = loaded;

    let mut app = App {
        streams: BTreeMap::new(),
        tombstoned: HashSet::new(),
        expanded_groups: HashSet::new(),
        app_order: BTreeMap::new(),
        app_seq: 0,
        status: load_err,
        profiles,
        pending: Pending::default(),
        modal: None,
        dirty: false,
        geometry,
        geometry_dirty: false,
        store_path,
        palette_open: false,
        palette_query: String::new(),
        palette_selected: 0,
        section_filter: SectionFilter::default(),
    };
    ensure_active_profile(&mut app);
    app
}

/// Enforce the "always at least one profile, and it is active" invariant,
/// then mirror the active profile's section filter into live view state.
/// Device settings are never auto-applied here.
fn ensure_active_profile(state: &mut App) {
    if state.profiles.profiles.is_empty() {
        state.profiles.profiles.push(Profile {
            name: "Default".to_string(),
            ..Profile::default()
        });
    }
    let active_valid = state
        .profiles
        .active
        .as_deref()
        .is_some_and(|a| state.profiles.find(a).is_some());
    if !active_valid {
        state.profiles.active = state.profiles.profiles.first().map(|p| p.name.clone());
    }
    if let Some(active) = state.profiles.active.as_deref()
        && let Some(profile) = state.profiles.find(active)
    {
        state.section_filter = profile.section_filter;
    }
}

pub fn update(state: &mut App, message: Message) {
    match message {
        Message::Worker(evt) => match *evt {
            WorkerEvent::StreamAdded(s) | WorkerEvent::StreamUpdated(s) => {
                // A fresh app stream matching a tombstoned row evicts the
                // ghost (one row, not two) and inherits its slider/route
                // state so in-session edits beat PipeWire's restore.
                // Ephemeral streams (libcanberra pings) MUST NOT evict:
                // they share app_identity with the parent app, so a
                // notification fired while a tab is tombstoned would wipe
                // the idle row, then die via !should_tombstone, leaving
                // the user with no row.
                let mut inherit: Option<(f32, bool, Option<String>)> = None;
                let mut inherit_seq: Option<u64> = None;
                if matches!(s.kind, StreamKind::Application)
                    && !is_ephemeral(&s)
                    && let Some(new_key) = s.app_identity()
                {
                    let stale: Vec<u32> = state
                        .tombstoned
                        .iter()
                        .copied()
                        .filter(|tid| *tid != s.id)
                        .filter(|tid| {
                            state
                                .streams
                                .get(tid)
                                .and_then(AudioStream::app_identity)
                                .is_some_and(|k| k == new_key)
                        })
                        .collect();
                    for tid in stale {
                        if inherit.is_none()
                            && let Some(ghost) = state.streams.get(&tid)
                        {
                            inherit = Some((
                                ghost.average_volume(),
                                ghost.muted,
                                ghost.target_sink_name.clone(),
                            ));
                        }
                        if inherit_seq.is_none() {
                            inherit_seq = state.app_order.get(&tid).copied();
                        }
                        state.tombstoned.remove(&tid);
                        state.streams.remove(&tid);
                        state.app_order.remove(&tid);
                    }
                }
                // A stream re-emerging under its own id sheds its tombstone.
                state.tombstoned.remove(&s.id);
                let new_id = s.id;
                let kind = s.kind;
                state.streams.insert(new_id, s);

                // First-seen seq so the strip orders by arrival, not node
                // id. Inherit from an evicted tombstone, else mint fresh on
                // first sight, else leave an existing seq alone.
                if matches!(kind, StreamKind::Application)
                    && let std::collections::btree_map::Entry::Vacant(slot) =
                        state.app_order.entry(new_id)
                {
                    let seq = inherit_seq.unwrap_or_else(|| {
                        let next = state.app_seq;
                        state.app_seq = state.app_seq.wrapping_add(1);
                        next
                    });
                    slot.insert(seq);
                }

                // Pending profile entries fulfill on a matching stream.
                // The tombstone-inherit branch below overrides this so
                // in-session edits beat the saved profile.
                consume_pending_for_stream(state, new_id);

                if let Some((volume, muted, target_sink_name)) = inherit {
                    // Apply locally first so the UI doesn't wait for the
                    // Props roundtrip.
                    if let Some(target) = state.streams.get_mut(&new_id) {
                        target.set_uniform_volume(volume);
                        target.muted = muted;
                        target.target_sink_name = target_sink_name.clone();
                    }
                    if let Some(h) = worker::handle() {
                        h.send(Command::SetVolume {
                            node_id: new_id,
                            volume,
                        });
                        h.send(Command::SetMute {
                            node_id: new_id,
                            mute: muted,
                        });
                        // Only replay a pin; replaying `None` would clear
                        // the route the fresh stream came back with.
                        if target_sink_name.is_some() {
                            h.send(Command::SetStreamTarget {
                                node_id: new_id,
                                sink_node_name: target_sink_name,
                            });
                        }
                    }
                }
            }
            WorkerEvent::StreamRemoved(id) => {
                // Sinks/sources drop immediately on unplug. App streams
                // tombstone (stay in the list) unless ephemeral event
                // sounds, which would litter the list with ghosts.
                let should_tombstone = state
                    .streams
                    .get(&id)
                    .is_some_and(|s| matches!(s.kind, StreamKind::Application) && !is_ephemeral(s));
                if should_tombstone {
                    state.tombstoned.insert(id);
                } else {
                    state.streams.remove(&id);
                    state.tombstoned.remove(&id);
                    state.app_order.remove(&id);
                }
            }
            WorkerEvent::Error(msg) => {
                state.status = Some(msg);
            }
        },
        Message::VolumeChanged(id, slider_cubic) => {
            // Sliders are perceptual (cubic); PipeWire wants linear gain.
            let linear = cubic_to_linear(slider_cubic);
            if let Some(s) = state.streams.get_mut(&id) {
                s.set_uniform_volume(linear);
            }
            // Tombstoned rows have no live Node; skip the command. The
            // local update is replayed if the app reconnects.
            if !state.tombstoned.contains(&id)
                && let Some(h) = worker::handle()
            {
                h.send(Command::SetVolume {
                    node_id: id,
                    volume: linear,
                });
            }
            mark_dirty(state);
        }
        Message::MuteToggled(id) => {
            let tombstoned = state.tombstoned.contains(&id);
            let toggle = state.streams.get_mut(&id).map(|s| {
                s.muted = !s.muted;
                (s.muted, s.average_volume())
            });
            if tombstoned {
                // Local flip only; the reconnect path replays it. Still
                // dirty so the autosave tick keeps the gesture.
                mark_dirty(state);
                return;
            }
            if let (Some(h), Some((muted, volume))) = (worker::handle(), toggle) {
                send_mute_reassert(&h, id, muted, volume);
            }
            mark_dirty(state);
        }
        Message::GroupVolumeChanged { key, cubic } => {
            let cubic = cubic.clamp(0.0, MAX_VOLUME);
            let members = members_of_app_row(state, &key);
            if members.is_empty() {
                return;
            }
            apply_group_volume(state, &members, cubic);
            mark_dirty(state);
        }
        Message::GroupMuteToggled(key) => {
            let members = members_of_app_row(state, &key);
            if members.is_empty() {
                return;
            }
            apply_group_mute(state, &members);
            mark_dirty(state);
        }
        Message::GroupSetStreamTarget { key, sink_id } => {
            let sink_name = state
                .streams
                .get(&sink_id)
                .filter(|s| matches!(s.kind, StreamKind::Sink))
                .and_then(|s| s.node_name.clone());
            let Some(sink_name) = sink_name else { return };
            let members = members_of_app_row(state, &key);
            apply_group_target(state, &members, Some(&sink_name));
            mark_dirty(state);
        }
        Message::GroupClearStreamTarget(key) => {
            let members = members_of_app_row(state, &key);
            apply_group_target(state, &members, None);
            mark_dirty(state);
        }
        Message::GroupToggleExpanded(key) => {
            // Only multi-member groups have anything to expand into.
            let member_count = members_of_app_row(state, &key).len();
            if member_count <= 1 {
                state.expanded_groups.remove(&key);
                return;
            }
            if !state.expanded_groups.insert(key.clone()) {
                state.expanded_groups.remove(&key);
            }
        }
        Message::MprisChanged => {}
        Message::MakeDefault(id) => make_default(state, id, StreamKind::Sink),
        Message::MakeDefaultSource(id) => make_default(state, id, StreamKind::Source),
        Message::ToggleSection(section) => {
            state.section_filter.toggle(section);
            mark_dirty(state);
        }
        Message::MuteAllToggled => {
            // If anything is unmuted, mute all outputs; else unmute all.
            let target_mute = !state.all_outputs_muted();
            let sink_ids: Vec<u32> = state
                .streams
                .values()
                .filter(|s| matches!(s.kind, StreamKind::Sink))
                .map(|s| s.id)
                .collect();
            for id in sink_ids {
                set_sink_mute(state, id, target_mute);
            }
            mark_dirty(state);
        }
        Message::SetStreamTarget { app_id, sink_id } => {
            let sink_name = state
                .streams
                .get(&sink_id)
                .filter(|s| matches!(s.kind, StreamKind::Sink))
                .and_then(|s| s.node_name.clone());
            let Some(sink_name) = sink_name else {
                return;
            };
            // Optimistic local update so the UI stays snappy; the
            // metadata listener echoes the write back later.
            set_app_target(state, &worker::handle(), app_id, Some(&sink_name));
            mark_dirty(state);
        }
        Message::ResetAllStreamTargets => {
            let ids: Vec<u32> = state
                .streams
                .iter()
                .filter(|(_, s)| {
                    matches!(s.kind, StreamKind::Application) && s.target_sink_name.is_some()
                })
                .map(|(id, _)| *id)
                .collect();
            let handle = worker::handle();
            for id in ids {
                set_app_target(state, &handle, id, None);
            }
            mark_dirty(state);
        }
        Message::ClearStreamTarget(app_id) => {
            set_app_target(state, &worker::handle(), app_id, None);
            mark_dirty(state);
        }
        Message::ApplyProfile(name) => {
            apply_profile(state, &name);
        }
        Message::DeleteProfile(name) => {
            let was_active = state.profiles.active.as_deref() == Some(name.as_str());
            if state.profiles.remove(&name) {
                if was_active {
                    state.pending = Pending::default();
                }
                ensure_active_profile(state);
                persist_state(state);
            }
        }
        Message::ReorderProfile {
            name,
            target,
            before,
        } => {
            if state.profiles.reorder(&name, &target, before) {
                persist_state(state);
            }
        }
        Message::OpenCreateProfileModal => {
            state.modal = Some(Modal::CreateProfile {
                name: String::new(),
                error: None,
            });
        }
        Message::OpenRenameProfileModal(old_name) => {
            if state.profiles.find(&old_name).is_some() {
                state.modal = Some(Modal::RenameProfile {
                    name: old_name.clone(),
                    old_name,
                    error: None,
                });
            }
        }
        Message::OpenDeleteProfileModal(name) => {
            if state.profiles.find(&name).is_some() {
                state.modal = Some(Modal::DeleteProfile { name });
            }
        }
        Message::ModalNameChanged(value) => match state.modal.as_mut() {
            Some(Modal::CreateProfile { name, error }) => {
                *name = value;
                *error = None;
            }
            Some(Modal::RenameProfile { name, error, .. }) => {
                *name = value;
                *error = None;
            }
            _ => {}
        },
        Message::ModalConfirm => {
            confirm_modal(state);
        }
        Message::ModalDismiss => {
            state.modal = None;
        }
        Message::AutoSaveTick => {
            autosave(state);
        }
        Message::GeometryChanged {
            width,
            height,
            maximized,
        } => {
            let next = Geometry {
                width,
                height,
                maximized,
            };
            if state.geometry != next {
                state.geometry = next;
                state.geometry_dirty = true;
            }
        }
        Message::TogglePalette => {
            // A modal owns focus; don't stack the palette on top of it.
            if state.modal.is_some() && !state.palette_open {
                return;
            }
            state.palette_open = !state.palette_open;
            state.palette_query.clear();
            state.palette_selected = 0;
        }
        Message::PaletteQueryChanged(q) => {
            state.palette_query = q;
            state.palette_selected = 0;
        }
        Message::PaletteSelectPrev => {
            let count = filtered_palette_count(state);
            if count == 0 {
                state.palette_selected = 0;
                return;
            }
            let last = count - 1;
            state.palette_selected = if state.palette_selected == 0 {
                last
            } else {
                state.palette_selected - 1
            };
        }
        Message::PaletteSelectNext => {
            let count = filtered_palette_count(state);
            if count == 0 {
                state.palette_selected = 0;
                return;
            }
            let last = count - 1;
            state.palette_selected = if state.palette_selected >= last {
                0
            } else {
                state.palette_selected + 1
            };
        }
    }
}

/// Palette rows matching the current query, capped at `MAX_VISIBLE`.
fn filtered_palette_count(state: &App) -> usize {
    let cmds = command_palette::build_commands(state);
    let filtered = command_palette::filter_commands(&cmds, &state.palette_query);
    filtered.len().min(command_palette::MAX_VISIBLE)
}

/// Turn a confirmed modal into its action (create/rename/delete).
/// Validation failures surface inline and keep the modal open.
fn confirm_modal(state: &mut App) {
    let Some(modal) = state.modal.take() else {
        return;
    };
    match modal {
        Modal::CreateProfile { name, .. } => {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                state.modal = Some(Modal::CreateProfile {
                    name,
                    error: Some("name cannot be empty".into()),
                });
                return;
            }
            let profile = Profile::snapshot(
                trimmed.to_string(),
                &state.streams,
                current_default(state, StreamKind::Sink),
                current_default(state, StreamKind::Source),
                state.section_filter,
            );
            state.profiles.insert_or_replace(profile);
            state.profiles.active = Some(trimmed.to_string());
            state.pending = Pending::default();
            state.dirty = false;
            persist_state(state);
        }
        Modal::RenameProfile { old_name, name, .. } => {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                state.modal = Some(Modal::RenameProfile {
                    old_name,
                    name,
                    error: Some("name cannot be empty".into()),
                });
                return;
            }
            if trimmed == old_name {
                // No-op rename, just close.
                return;
            }
            if state.profiles.find(trimmed).is_some() {
                let error = Some(format!("a profile named '{trimmed}' already exists"));
                state.modal = Some(Modal::RenameProfile {
                    old_name,
                    name,
                    error,
                });
                return;
            }
            if !state.profiles.rename(&old_name, trimmed.to_string()) {
                state.status = Some(format!("profile '{old_name}' not found"));
                return;
            }
            persist_state(state);
        }
        Modal::DeleteProfile { name } => {
            let was_active = state.profiles.active.as_deref() == Some(name.as_str());
            if state.profiles.remove(&name) {
                if was_active {
                    state.pending = Pending::default();
                }
                ensure_active_profile(state);
                persist_state(state);
            }
        }
    }
}

/// Mark the active profile as having uncommitted changes. No-op when no
/// profile is active.
fn mark_dirty(state: &mut App) {
    if state.profiles.active.is_some() {
        state.dirty = true;
    }
}

/// Flush uncommitted state to disk. `dirty` re-snapshots the active
/// profile before writing; `geometry_dirty` writes the header alone, so a
/// resize before any stream connects can't clobber saved presets with an
/// empty snapshot.
fn autosave(state: &mut App) {
    if !state.dirty && !state.geometry_dirty {
        return;
    }
    if state.dirty {
        snapshot_active_profile(state);
    }
    state.dirty = false;
    state.geometry_dirty = false;
    persist_state(state);
}

/// Fold live state (device settings + section filter) into the active
/// profile. No-op when no profile is active.
fn snapshot_active_profile(state: &mut App) {
    let Some(active_name) = state.profiles.active.clone() else {
        return;
    };
    let snap = Profile::snapshot(
        active_name,
        &state.streams,
        current_default(state, StreamKind::Sink),
        current_default(state, StreamKind::Source),
        state.section_filter,
    );
    state.profiles.insert_or_replace(snap);
}

/// Write window + profiles to disk, surfacing IO errors in the status bar.
/// No-op when `store_path` is unset.
fn persist_state(state: &mut App) {
    let Some(path) = state.store_path.clone() else {
        return;
    };
    let snapshot = store::State {
        window: state.geometry,
        profiles: state.profiles.clone(),
    };
    if let Err(e) = store::save_to(&path, &snapshot) {
        state.status = Some(format!("save state: {e:#}"));
    }
}

/// `node.name` of the current system-default device of `kind`, or `None`.
fn current_default(state: &App, kind: StreamKind) -> Option<&str> {
    state
        .streams
        .values()
        .find(|s| s.kind == kind && s.is_default)
        .and_then(|s| s.node_name.as_deref())
}

/// Make stream `id` the system default for its direction. No-op if `id`
/// isn't a non-default device of `kind` or no worker handle exists.
fn make_default(state: &mut App, id: u32, kind: StreamKind) {
    let command: fn(String) -> Command = match kind {
        StreamKind::Sink => |node_name| Command::SetDefaultSink { node_name },
        StreamKind::Source => |node_name| Command::SetDefaultSource { node_name },
        StreamKind::Application => return,
    };
    let target = state
        .streams
        .get(&id)
        .filter(|s| s.kind == kind && !s.is_default)
        .and_then(|s| s.node_name.clone());
    if let (Some(h), Some(node_name)) = (worker::handle(), target) {
        h.send(command(node_name));
        mark_dirty(state);
    }
}

/// Push `SetMute`, then re-assert `SetVolume` on unmute. Bluetooth AVRCP
/// sinks zero their device volume on mute and don't restore it on unmute,
/// so the re-assert snaps them back. Benign no-op elsewhere.
fn send_mute_reassert(h: &worker::Handle, node_id: u32, mute: bool, volume: f32) {
    h.send(Command::SetMute { node_id, mute });
    if !mute {
        h.send(Command::SetVolume { node_id, volume });
    }
}

/// Set one sink's mute: flip the local flag, then push via
/// [`send_mute_reassert`]. Tombstoned nodes get the local flip only.
fn set_sink_mute(state: &mut App, id: u32, mute: bool) {
    let tombstoned = state.tombstoned.contains(&id);
    let volume = state.streams.get_mut(&id).map(|s| {
        s.muted = mute;
        s.average_volume()
    });
    if tombstoned {
        return;
    }
    if let (Some(h), Some(volume)) = (worker::handle(), volume) {
        send_mute_reassert(&h, id, mute, volume);
    }
}

/// Push every entry of the named profile onto live state. Unmatched
/// entries go into `state.pending` and apply when the stream connects.
fn apply_profile(state: &mut App, name: &str) {
    // Flush in-flight changes to the previous profile first. Re-applying
    // the same profile is NOT a flush; it's a "revert to saved" gesture.
    if state.dirty
        && let Some(prev) = state.profiles.active.as_deref()
        && prev != name
    {
        autosave(state);
    }

    let Some(profile) = state.profiles.find(name).cloned() else {
        state.status = Some(format!("profile '{name}' not found"));
        return;
    };

    let handle = worker::handle();
    let mut pending = Pending::default();

    for (node_name, settings) in &profile.sinks {
        if !apply_device_entry(state, &handle, StreamKind::Sink, node_name, settings) {
            pending.sinks.insert(node_name.clone(), *settings);
        }
    }

    for (node_name, settings) in &profile.sources {
        if !apply_device_entry(state, &handle, StreamKind::Source, node_name, settings) {
            pending.sources.insert(node_name.clone(), *settings);
        }
    }

    for (key, settings) in &profile.apps {
        let target = state.streams.iter().find_map(|(id, s)| {
            (matches!(s.kind, StreamKind::Application)
                && s.app_identity().as_deref() == Some(key.as_str()))
            .then_some(*id)
        });
        match target {
            Some(id) => apply_app_settings(state, &handle, id, settings),
            None => {
                pending.apps.insert(key.clone(), settings.clone());
            }
        }
    }

    if let Some(default_name) = profile.default_sink.as_deref() {
        let live = state.streams.values().any(|s| {
            matches!(s.kind, StreamKind::Sink) && s.node_name.as_deref() == Some(default_name)
        });
        if live {
            if let Some(h) = &handle {
                h.send(Command::SetDefaultSink {
                    node_name: default_name.to_string(),
                });
            }
        } else {
            pending.default_sink = Some(default_name.to_string());
        }
    }

    if let Some(default_name) = profile.default_source.as_deref() {
        let live = state.streams.values().any(|s| {
            matches!(s.kind, StreamKind::Source) && s.node_name.as_deref() == Some(default_name)
        });
        if live {
            if let Some(h) = &handle {
                h.send(Command::SetDefaultSource {
                    node_name: default_name.to_string(),
                });
            }
        } else {
            pending.default_source = Some(default_name.to_string());
        }
    }

    state.section_filter = profile.section_filter;
    state.pending = pending;
    state.profiles.active = Some(profile.name);
    state.dirty = false;
    persist_state(state);
}

/// Push a saved device preset (volume + mute) onto a live sink or source.
/// Direction-agnostic; devices carry no per-stream target.
fn apply_device_settings(
    state: &mut App,
    handle: &Option<worker::Handle>,
    node_id: u32,
    settings: &DeviceSettings,
) {
    let tombstoned = state.tombstoned.contains(&node_id);
    if let Some(s) = state.streams.get_mut(&node_id) {
        s.set_uniform_volume(settings.volume);
        s.muted = settings.muted;
    }
    if tombstoned {
        return;
    }
    let Some(h) = handle else { return };
    // SetVolume, then SetMute with the unmute re-assert.
    h.send(Command::SetVolume {
        node_id,
        volume: settings.volume,
    });
    send_mute_reassert(h, node_id, settings.muted, settings.volume);
}

/// Apply one device entry of `kind`. Returns `true` if a matching live
/// stream took the settings, `false` so the caller can stash it in pending.
fn apply_device_entry(
    state: &mut App,
    handle: &Option<worker::Handle>,
    kind: StreamKind,
    node_name: &str,
    settings: &DeviceSettings,
) -> bool {
    let target = state.streams.iter().find_map(|(id, s)| {
        (s.kind == kind && s.node_name.as_deref() == Some(node_name)).then_some(*id)
    });
    match target {
        Some(id) => {
            apply_device_settings(state, handle, id, settings);
            true
        }
        None => false,
    }
}

fn apply_app_settings(
    state: &mut App,
    handle: &Option<worker::Handle>,
    node_id: u32,
    settings: &AppSettings,
) {
    let tombstoned = state.tombstoned.contains(&node_id);
    if let Some(s) = state.streams.get_mut(&node_id) {
        s.set_uniform_volume(settings.volume);
        s.muted = settings.muted;
        s.target_sink_name = settings.target_sink_name.clone();
    }
    if tombstoned {
        return;
    }
    let Some(h) = handle else { return };
    h.send(Command::SetVolume {
        node_id,
        volume: settings.volume,
    });
    h.send(Command::SetMute {
        node_id,
        mute: settings.muted,
    });
    h.send(Command::SetStreamTarget {
        node_id,
        sink_node_name: settings.target_sink_name.clone(),
    });
}

/// Rescale every member of a collapsed app row toward `target_cubic`,
/// preserving relative levels by multiplying each by
/// `target_cubic / master_old_cubic` (master = loudest member). When the
/// master is ~0 (all silent) write `target_cubic` to every member; members
/// clamp to `MAX_VOLUME`.
fn apply_group_volume(state: &mut App, members: &[u32], target_cubic: f32) {
    let master_old_cubic = members
        .iter()
        .filter_map(|id| state.streams.get(id))
        .map(|s| linear_to_cubic(s.average_volume()))
        .fold(0.0_f32, f32::max);

    let plan: Vec<(u32, f32)> = if master_old_cubic > 1e-6 {
        let ratio = target_cubic / master_old_cubic;
        members
            .iter()
            .filter_map(|id| {
                state.streams.get(id).map(|s| {
                    let new_cubic =
                        (linear_to_cubic(s.average_volume()) * ratio).clamp(0.0, MAX_VOLUME);
                    (*id, cubic_to_linear(new_cubic))
                })
            })
            .collect()
    } else {
        let linear = cubic_to_linear(target_cubic);
        members.iter().map(|id| (*id, linear)).collect()
    };

    let handle = worker::handle();
    for (id, linear) in plan {
        if let Some(s) = state.streams.get_mut(&id) {
            s.set_uniform_volume(linear);
        }
        if state.tombstoned.contains(&id) {
            continue;
        }
        if let Some(h) = &handle {
            h.send(Command::SetVolume {
                node_id: id,
                volume: linear,
            });
        }
    }
}

/// Toggle a collapsed app row's mute. If every member is muted, unmute
/// all; otherwise mute all. The unmute re-assert applies per member.
fn apply_group_mute(state: &mut App, members: &[u32]) {
    let all_muted = members
        .iter()
        .filter_map(|id| state.streams.get(id))
        .all(|s| s.muted);
    let new_muted = !all_muted;
    let handle = worker::handle();

    for &id in members {
        let avg = match state.streams.get_mut(&id) {
            Some(s) => {
                s.muted = new_muted;
                s.average_volume()
            }
            None => continue,
        };
        if state.tombstoned.contains(&id) {
            continue;
        }
        let Some(h) = &handle else { continue };
        send_mute_reassert(h, id, new_muted, avg);
    }
}

/// Apply a per-stream output target to one app stream (or clear it when
/// `None`), then push the `target.object` write. Tombstoned rows get the
/// local update only; the reconnect path replays the pin.
fn set_app_target(
    state: &mut App,
    handle: &Option<worker::Handle>,
    id: u32,
    target_sink_name: Option<&str>,
) {
    if let Some(s) = state.streams.get_mut(&id) {
        s.target_sink_name = target_sink_name.map(str::to_string);
    }
    if state.tombstoned.contains(&id) {
        return;
    }
    if let Some(h) = handle {
        h.send(Command::SetStreamTarget {
            node_id: id,
            sink_node_name: target_sink_name.map(str::to_string),
        });
    }
}

/// Apply `target_sink_name` (or clear when `None`) to every member of a
/// collapsed app row.
fn apply_group_target(state: &mut App, members: &[u32], target_sink_name: Option<&str>) {
    let handle = worker::handle();
    for &id in members {
        set_app_target(state, &handle, id, target_sink_name);
    }
}

/// Apply any matching `pending` entry to a freshly-added stream and remove
/// it, so a later reconnect doesn't reapply stale state.
fn consume_pending_for_stream(state: &mut App, stream_id: u32) {
    if state.pending.is_empty() {
        return;
    }
    let Some(stream) = state.streams.get(&stream_id) else {
        return;
    };
    let handle = worker::handle();
    match stream.kind {
        StreamKind::Sink => {
            let Some(node_name) = stream.node_name.clone() else {
                return;
            };
            if let Some(settings) = state.pending.sinks.remove(&node_name) {
                apply_device_settings(state, &handle, stream_id, &settings);
            }
            if state.pending.default_sink.as_deref() == Some(node_name.as_str()) {
                state.pending.default_sink = None;
                if let Some(h) = &handle {
                    h.send(Command::SetDefaultSink { node_name });
                }
            }
        }
        StreamKind::Source => {
            let Some(node_name) = stream.node_name.clone() else {
                return;
            };
            if let Some(settings) = state.pending.sources.remove(&node_name) {
                apply_device_settings(state, &handle, stream_id, &settings);
            }
            if state.pending.default_source.as_deref() == Some(node_name.as_str()) {
                state.pending.default_source = None;
                if let Some(h) = &handle {
                    h.send(Command::SetDefaultSource { node_name });
                }
            }
        }
        StreamKind::Application => {
            let Some(key) = stream.app_identity() else {
                return;
            };
            if let Some(settings) = state.pending.apps.remove(&key) {
                apply_app_settings(state, &handle, stream_id, &settings);
            }
        }
    }
}

/// True for short-lived event sounds (libcanberra Notification/Event/Alarm
/// pings), which skip the tombstone grace window.
pub fn is_ephemeral(s: &AudioStream) -> bool {
    matches!(
        s.media_role.as_deref(),
        Some("Event") | Some("Notification") | Some("Alarm")
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn app_stream(id: u32) -> AudioStream {
        AudioStream {
            id,
            kind: StreamKind::Application,
            name: format!("stream-{id}"),
            app_id: None,
            binary: None,
            pid: None,
            node_name: None,
            media_name: None,
            media_role: None,
            channel_volumes: vec![0.5, 0.5],
            muted: false,
            xdg: None,
            form: None,
            is_default: false,
            target_sink_name: None,
        }
    }

    #[test]
    fn app_identity_prefers_app_id_over_binary() {
        let mut s = app_stream(1);
        s.app_id = Some("com.spotify.Client".into());
        s.binary = Some("spotify".into());
        assert_eq!(s.app_identity().as_deref(), Some("app:com.spotify.Client"));
    }

    #[test]
    fn app_identity_falls_back_to_binary_then_none() {
        let mut s = app_stream(1);
        s.binary = Some("firefox".into());
        assert_eq!(s.app_identity().as_deref(), Some("bin:firefox"));

        let bare = app_stream(2);
        assert_eq!(bare.app_identity(), None);
    }

    #[test]
    fn app_identity_is_none_for_sinks() {
        let mut s = app_stream(1);
        s.kind = StreamKind::Sink;
        s.app_id = Some("com.spotify.Client".into());
        assert_eq!(s.app_identity(), None);
    }

    #[test]
    fn ephemeral_detects_libcanberra_roles() {
        for role in ["Event", "Notification", "Alarm"] {
            let mut s = app_stream(1);
            s.media_role = Some(role.into());
            assert!(is_ephemeral(&s), "expected {role} ephemeral");
        }
        let mut music = app_stream(1);
        music.media_role = Some("Music".into());
        assert!(!is_ephemeral(&music));
        let unset = app_stream(1);
        assert!(!is_ephemeral(&unset));
    }

    fn boot_state() -> App {
        App {
            streams: BTreeMap::new(),
            tombstoned: HashSet::new(),
            expanded_groups: HashSet::new(),
            app_order: BTreeMap::new(),
            app_seq: 0,
            status: None,
            profiles: ProfileStore::default(),
            pending: Pending::default(),
            modal: None,
            dirty: false,
            geometry: Geometry::default(),
            geometry_dirty: false,
            store_path: None,
            palette_open: false,
            palette_query: String::new(),
            palette_selected: 0,
            section_filter: SectionFilter::default(),
        }
    }

    #[test]
    fn stream_removed_for_app_stream_tombstones_instead_of_dropping() {
        let mut state = boot_state();
        let s = app_stream(42);
        state.streams.insert(s.id, s);

        update(
            &mut state,
            Message::Worker(Box::new(WorkerEvent::StreamRemoved(42))),
        );
        assert!(state.tombstoned.contains(&42));
        assert!(state.streams.contains_key(&42));
    }

    #[test]
    fn stream_removed_for_ephemeral_app_stream_drops_immediately() {
        let mut state = boot_state();
        let mut s = app_stream(42);
        s.media_role = Some("Event".into());
        state.streams.insert(s.id, s);

        update(
            &mut state,
            Message::Worker(Box::new(WorkerEvent::StreamRemoved(42))),
        );
        assert!(!state.tombstoned.contains(&42));
        assert!(!state.streams.contains_key(&42));
    }

    #[test]
    fn fresh_stream_evicts_matching_tombstone() {
        let mut state = boot_state();
        let mut old = app_stream(10);
        old.binary = Some("firefox".into());
        state.streams.insert(old.id, old);
        state.tombstoned.insert(10);

        let mut fresh = app_stream(11);
        fresh.binary = Some("firefox".into());
        update(
            &mut state,
            Message::Worker(Box::new(WorkerEvent::StreamAdded(fresh))),
        );

        assert!(!state.streams.contains_key(&10));
        assert!(!state.tombstoned.contains(&10));
        assert!(state.streams.contains_key(&11));
    }

    #[test]
    fn reconnect_inherits_tombstoned_slider_volume() {
        let mut state = boot_state();
        let mut old = app_stream(10);
        old.binary = Some("firefox".into());
        old.channel_volumes = vec![0.9, 0.9];
        old.muted = true;
        state.streams.insert(old.id, old);
        state.tombstoned.insert(10);

        let mut fresh = app_stream(11);
        fresh.binary = Some("firefox".into());
        fresh.channel_volumes = vec![0.2, 0.2];
        fresh.muted = false;
        update(
            &mut state,
            Message::Worker(Box::new(WorkerEvent::StreamAdded(fresh))),
        );

        let live = state.streams.get(&11).expect("fresh row");
        assert!((live.average_volume() - 0.9).abs() < 1e-6);
        assert!(live.muted);
    }

    #[test]
    fn volume_changed_on_tombstoned_row_updates_local_only() {
        let mut state = boot_state();
        let mut s = app_stream(10);
        s.channel_volumes = vec![0.5, 0.5];
        state.streams.insert(s.id, s);
        state.tombstoned.insert(10);

        update(&mut state, Message::VolumeChanged(10, 1.0));
        let row = state.streams.get(&10).expect("row");
        assert!((row.average_volume() - 1.0).abs() < 1e-6);
        assert!(state.tombstoned.contains(&10));
    }

    #[test]
    fn mute_toggled_on_tombstoned_row_flips_local_only() {
        let mut state = boot_state();
        let mut s = app_stream(10);
        s.muted = false;
        state.streams.insert(s.id, s);
        state.tombstoned.insert(10);

        update(&mut state, Message::MuteToggled(10));
        assert!(state.streams.get(&10).expect("row").muted);
        assert!(state.tombstoned.contains(&10));
    }

    #[test]
    fn fresh_stream_without_identity_leaves_tombstone_alone() {
        let mut state = boot_state();
        let mut old = app_stream(10);
        old.binary = Some("firefox".into());
        state.streams.insert(old.id, old);
        state.tombstoned.insert(10);

        let fresh = app_stream(11);
        update(
            &mut state,
            Message::Worker(Box::new(WorkerEvent::StreamAdded(fresh))),
        );

        assert!(state.streams.contains_key(&10));
        assert!(state.tombstoned.contains(&10));
        assert!(state.streams.contains_key(&11));
    }

    #[test]
    fn ephemeral_arrival_does_not_evict_matching_tombstone() {
        // Regression: a ping from the same app must not evict the idle row.
        let mut state = boot_state();
        let mut idle = app_stream(10);
        idle.binary = Some("chrome".into());
        state.streams.insert(idle.id, idle);
        state.tombstoned.insert(10);

        let mut ping = app_stream(11);
        ping.binary = Some("chrome".into());
        ping.media_role = Some("Notification".into());
        update(
            &mut state,
            Message::Worker(Box::new(WorkerEvent::StreamAdded(ping))),
        );

        assert!(
            state.streams.contains_key(&10),
            "ephemeral arrival must not evict the idle row",
        );
        assert!(state.tombstoned.contains(&10));
        assert!(state.streams.contains_key(&11));
    }

    #[test]
    fn app_order_assigned_on_first_sight_and_stable_across_updates() {
        let mut state = boot_state();
        let mut spotify = app_stream(50);
        spotify.binary = Some("spotify".into());
        let mut helium = app_stream(80);
        helium.binary = Some("helium".into());

        update(
            &mut state,
            Message::Worker(Box::new(WorkerEvent::StreamAdded(spotify.clone()))),
        );
        update(
            &mut state,
            Message::Worker(Box::new(WorkerEvent::StreamAdded(helium.clone()))),
        );

        let spotify_seq = state.app_order.get(&50).copied().expect("spotify seq");
        let helium_seq = state.app_order.get(&80).copied().expect("helium seq");
        assert!(spotify_seq < helium_seq, "spotify saw first → lower seq");

        // A Props update must not bump the seq.
        update(
            &mut state,
            Message::Worker(Box::new(WorkerEvent::StreamUpdated(spotify))),
        );
        assert_eq!(state.app_order.get(&50).copied(), Some(spotify_seq));
    }

    #[test]
    fn reclaim_preserves_app_order_seq() {
        // A reclaimed row under a new node id must keep its slot, not
        // mint a fresh (highest) seq and jump past later rows.
        let mut state = boot_state();
        let mut spotify = app_stream(50);
        spotify.binary = Some("spotify".into());
        let mut helium = app_stream(80);
        helium.binary = Some("helium".into());

        update(
            &mut state,
            Message::Worker(Box::new(WorkerEvent::StreamAdded(spotify))),
        );
        update(
            &mut state,
            Message::Worker(Box::new(WorkerEvent::StreamAdded(helium))),
        );
        let original_spotify_seq = state.app_order[&50];
        let helium_seq = state.app_order[&80];

        update(
            &mut state,
            Message::Worker(Box::new(WorkerEvent::StreamRemoved(50))),
        );
        assert!(state.tombstoned.contains(&50));

        // Resume under a new node id; eviction reclaims the slot.
        let mut spotify_v2 = app_stream(120);
        spotify_v2.binary = Some("spotify".into());
        update(
            &mut state,
            Message::Worker(Box::new(WorkerEvent::StreamAdded(spotify_v2))),
        );

        assert!(!state.streams.contains_key(&50));
        assert!(!state.app_order.contains_key(&50));
        assert_eq!(
            state.app_order.get(&120).copied(),
            Some(original_spotify_seq)
        );
        assert!(
            state.app_order[&120] < helium_seq,
            "reclaimed row keeps its slot"
        );
    }

    #[test]
    fn ephemeral_removal_clears_app_order_entry() {
        let mut state = boot_state();
        let mut ping = app_stream(20);
        ping.binary = Some("dunst".into());
        ping.media_role = Some("Event".into());
        update(
            &mut state,
            Message::Worker(Box::new(WorkerEvent::StreamAdded(ping))),
        );
        assert!(state.app_order.contains_key(&20));

        update(
            &mut state,
            Message::Worker(Box::new(WorkerEvent::StreamRemoved(20))),
        );
        assert!(!state.app_order.contains_key(&20));
    }

    fn sink_stream(id: u32, node_name: &str) -> AudioStream {
        let mut s = app_stream(id);
        s.kind = StreamKind::Sink;
        s.node_name = Some(node_name.into());
        s
    }

    #[test]
    fn set_stream_target_pins_to_clicked_sink() {
        let mut state = boot_state();
        state
            .streams
            .insert(100, sink_stream(100, "alsa_output.usb"));
        state
            .streams
            .insert(200, sink_stream(200, "bluez_output.headset"));
        state.streams.insert(10, app_stream(10));

        update(
            &mut state,
            Message::SetStreamTarget {
                app_id: 10,
                sink_id: 100,
            },
        );
        assert_eq!(
            state.streams.get(&10).unwrap().target_sink_name.as_deref(),
            Some("alsa_output.usb"),
        );

        update(
            &mut state,
            Message::SetStreamTarget {
                app_id: 10,
                sink_id: 200,
            },
        );
        assert_eq!(
            state.streams.get(&10).unwrap().target_sink_name.as_deref(),
            Some("bluez_output.headset"),
        );
    }

    #[test]
    fn clear_stream_target_unpins_to_follow_default() {
        let mut state = boot_state();
        let mut s = app_stream(10);
        s.target_sink_name = Some("alsa_output.usb".into());
        state.streams.insert(10, s);

        update(&mut state, Message::ClearStreamTarget(10));
        assert_eq!(state.streams.get(&10).unwrap().target_sink_name, None);
    }

    #[test]
    fn reset_all_stream_targets_clears_every_pin() {
        let mut state = boot_state();
        let mut a = app_stream(10);
        a.target_sink_name = Some("alsa_output.usb".into());
        let mut b = app_stream(11);
        b.target_sink_name = Some("bluez_output.headset".into());
        let untouched_app = app_stream(12);
        let mut sink = app_stream(100);
        sink.kind = StreamKind::Sink;
        sink.node_name = Some("alsa_output.usb".into());
        state.streams.insert(10, a);
        state.streams.insert(11, b);
        state.streams.insert(12, untouched_app);
        state.streams.insert(100, sink);

        update(&mut state, Message::ResetAllStreamTargets);

        assert_eq!(state.streams.get(&10).unwrap().target_sink_name, None);
        assert_eq!(state.streams.get(&11).unwrap().target_sink_name, None);
        assert_eq!(state.streams.get(&12).unwrap().target_sink_name, None);
        assert!(state.streams.contains_key(&100));
    }

    #[test]
    fn reset_all_stream_targets_clears_tombstoned_staged_pin() {
        let mut state = boot_state();
        let mut s = app_stream(10);
        s.target_sink_name = Some("alsa_output.usb".into());
        state.streams.insert(10, s);
        state.tombstoned.insert(10);

        update(&mut state, Message::ResetAllStreamTargets);

        assert_eq!(state.streams.get(&10).unwrap().target_sink_name, None);
        assert!(state.tombstoned.contains(&10));
    }

    #[test]
    fn clear_stream_target_on_tombstoned_row_updates_local_only() {
        let mut state = boot_state();
        let mut s = app_stream(10);
        s.target_sink_name = Some("alsa_output.usb".into());
        state.streams.insert(10, s);
        state.tombstoned.insert(10);

        update(&mut state, Message::ClearStreamTarget(10));
        assert_eq!(state.streams.get(&10).unwrap().target_sink_name, None);
        assert!(state.tombstoned.contains(&10));
    }

    #[test]
    fn set_stream_target_on_tombstoned_row_updates_local_only() {
        let mut state = boot_state();
        state
            .streams
            .insert(100, sink_stream(100, "alsa_output.usb"));
        let mut s = app_stream(10);
        s.binary = Some("firefox".into());
        state.streams.insert(10, s);
        state.tombstoned.insert(10);

        update(
            &mut state,
            Message::SetStreamTarget {
                app_id: 10,
                sink_id: 100,
            },
        );
        assert_eq!(
            state.streams.get(&10).unwrap().target_sink_name.as_deref(),
            Some("alsa_output.usb"),
        );
        assert!(state.tombstoned.contains(&10));
    }

    #[test]
    fn reconnect_inherits_tombstoned_target_pin() {
        let mut state = boot_state();
        state
            .streams
            .insert(100, sink_stream(100, "alsa_output.usb"));
        let mut old = app_stream(10);
        old.binary = Some("firefox".into());
        old.target_sink_name = Some("alsa_output.usb".into());
        state.streams.insert(old.id, old);
        state.tombstoned.insert(10);

        let mut fresh = app_stream(11);
        fresh.binary = Some("firefox".into());
        fresh.target_sink_name = None;
        update(
            &mut state,
            Message::Worker(Box::new(WorkerEvent::StreamAdded(fresh))),
        );

        let live = state.streams.get(&11).expect("fresh row");
        assert_eq!(
            live.target_sink_name.as_deref(),
            Some("alsa_output.usb"),
            "tombstoned target should be replayed onto the fresh Node",
        );
    }

    #[test]
    fn set_stream_target_does_nothing_for_unknown_sink() {
        let mut state = boot_state();
        state.streams.insert(10, app_stream(10));

        update(
            &mut state,
            Message::SetStreamTarget {
                app_id: 10,
                sink_id: 999,
            },
        );
        assert_eq!(state.streams.get(&10).unwrap().target_sink_name, None);
    }

    fn profile_with(
        name: &str,
        sinks: &[(&str, f32, bool)],
        apps: &[(&str, f32, bool, Option<&str>)],
        default_sink: Option<&str>,
    ) -> Profile {
        let mut p = Profile {
            name: name.into(),
            default_sink: default_sink.map(str::to_string),
            ..Profile::default()
        };
        for (n, v, m) in sinks {
            p.sinks.insert(
                (*n).into(),
                DeviceSettings {
                    volume: *v,
                    muted: *m,
                },
            );
        }
        for (k, v, m, t) in apps {
            p.apps.insert(
                (*k).into(),
                AppSettings {
                    volume: *v,
                    muted: *m,
                    target_sink_name: t.map(|s| s.to_string()),
                },
            );
        }
        p
    }

    #[test]
    fn apply_profile_writes_volume_mute_routing_to_live_streams() {
        let mut state = boot_state();
        state
            .streams
            .insert(100, sink_stream(100, "alsa_output.usb"));
        let mut app = app_stream(10);
        app.binary = Some("firefox".into());
        state.streams.insert(10, app);

        state.profiles.insert_or_replace(profile_with(
            "Work",
            &[("alsa_output.usb", 0.7, true)],
            &[("bin:firefox", 0.4, true, Some("alsa_output.usb"))],
            None,
        ));

        update(&mut state, Message::ApplyProfile("Work".into()));

        let sink = state.streams.get(&100).unwrap();
        assert!((sink.average_volume() - 0.7).abs() < 1e-6);
        assert!(sink.muted);

        let app = state.streams.get(&10).unwrap();
        assert!((app.average_volume() - 0.4).abs() < 1e-6);
        assert!(app.muted);
        assert_eq!(app.target_sink_name.as_deref(), Some("alsa_output.usb"));

        assert!(state.pending.is_empty());
        assert_eq!(state.profiles.active.as_deref(), Some("Work"));
    }

    #[test]
    fn apply_profile_stages_unmatched_entries_in_pending() {
        let mut state = boot_state();
        state
            .streams
            .insert(100, sink_stream(100, "alsa_output.usb"));
        let mut firefox = app_stream(10);
        firefox.binary = Some("firefox".into());
        state.streams.insert(10, firefox);

        state.profiles.insert_or_replace(profile_with(
            "Work",
            &[
                ("alsa_output.usb", 0.7, false),
                ("bluez_output.headset", 0.5, false),
            ],
            &[
                ("bin:firefox", 0.4, false, None),
                ("app:com.spotify.Client", 0.6, false, None),
            ],
            Some("bluez_output.headset"),
        ));

        update(&mut state, Message::ApplyProfile("Work".into()));

        assert!((state.streams.get(&100).unwrap().average_volume() - 0.7).abs() < 1e-6);
        assert!((state.streams.get(&10).unwrap().average_volume() - 0.4).abs() < 1e-6);

        assert!(state.pending.sinks.contains_key("bluez_output.headset"));
        assert!(state.pending.apps.contains_key("app:com.spotify.Client"));
        assert_eq!(
            state.pending.default_sink.as_deref(),
            Some("bluez_output.headset"),
        );
    }

    #[test]
    fn pending_sink_applies_when_matching_sink_connects() {
        let mut state = boot_state();
        state.pending.sinks.insert(
            "bluez_output.headset".into(),
            DeviceSettings {
                volume: 0.5,
                muted: true,
            },
        );

        let new_sink = sink_stream(200, "bluez_output.headset");
        update(
            &mut state,
            Message::Worker(Box::new(WorkerEvent::StreamAdded(new_sink))),
        );

        let live = state.streams.get(&200).unwrap();
        assert!((live.average_volume() - 0.5).abs() < 1e-6);
        assert!(live.muted);
        assert!(state.pending.sinks.is_empty());
    }

    #[test]
    fn pending_app_applies_when_matching_app_connects() {
        let mut state = boot_state();
        state.pending.apps.insert(
            "bin:firefox".into(),
            AppSettings {
                volume: 0.25,
                muted: false,
                target_sink_name: Some("alsa_output.usb".into()),
            },
        );

        let mut firefox = app_stream(11);
        firefox.binary = Some("firefox".into());
        update(
            &mut state,
            Message::Worker(Box::new(WorkerEvent::StreamAdded(firefox))),
        );

        let live = state.streams.get(&11).unwrap();
        assert!((live.average_volume() - 0.25).abs() < 1e-6);
        assert_eq!(live.target_sink_name.as_deref(), Some("alsa_output.usb"));
        assert!(state.pending.apps.is_empty());
    }

    #[test]
    fn pending_default_sink_clears_when_matching_sink_connects() {
        let mut state = boot_state();
        state.pending.default_sink = Some("bluez_output.headset".into());

        let s = sink_stream(200, "bluez_output.headset");
        update(
            &mut state,
            Message::Worker(Box::new(WorkerEvent::StreamAdded(s))),
        );

        assert!(state.pending.default_sink.is_none());
    }

    #[test]
    fn delete_last_profile_wipes_pending_and_reseeds_default() {
        let mut state = boot_state();
        state.profiles.insert_or_replace(profile_with(
            "Work",
            &[("alsa_output.usb", 0.7, false)],
            &[],
            None,
        ));
        state.profiles.active = Some("Work".into());
        state.pending.sinks.insert(
            "bluez_output.headset".into(),
            DeviceSettings {
                volume: 0.5,
                muted: false,
            },
        );

        update(&mut state, Message::DeleteProfile("Work".into()));

        assert!(state.profiles.find("Work").is_none());
        // Deleting the last profile re-seeds an active `Default`.
        assert_eq!(state.profiles.profiles.len(), 1);
        assert_eq!(state.profiles.active.as_deref(), Some("Default"));
        assert!(state.pending.is_empty());
    }

    #[test]
    fn delete_active_profile_repoints_active_to_remaining() {
        let mut state = boot_state();
        state
            .profiles
            .insert_or_replace(profile_with("Work", &[], &[], None));
        state
            .profiles
            .insert_or_replace(profile_with("Gaming", &[], &[], None));
        state.profiles.active = Some("Work".into());

        update(&mut state, Message::DeleteProfile("Work".into()));

        assert!(state.profiles.find("Work").is_none());
        // Active re-points to the first remaining profile, never None.
        assert_eq!(state.profiles.active.as_deref(), Some("Gaming"));
    }

    #[test]
    fn delete_inactive_profile_preserves_pending_and_active_marker() {
        let mut state = boot_state();
        state
            .profiles
            .insert_or_replace(profile_with("Work", &[], &[], None));
        state
            .profiles
            .insert_or_replace(profile_with("Gaming", &[], &[], None));
        state.profiles.active = Some("Work".into());
        state.pending.sinks.insert(
            "x".into(),
            DeviceSettings {
                volume: 0.5,
                muted: false,
            },
        );

        update(&mut state, Message::DeleteProfile("Gaming".into()));

        assert!(state.profiles.find("Work").is_some());
        assert!(state.profiles.find("Gaming").is_none());
        assert_eq!(state.profiles.active.as_deref(), Some("Work"));
        assert!(
            !state.pending.is_empty(),
            "untouched delete shouldn't wipe pending"
        );
    }

    #[test]
    fn create_modal_confirm_snapshots_current_state_and_marks_active() {
        let mut state = boot_state();
        let mut sink = sink_stream(100, "alsa_output.usb");
        sink.channel_volumes = vec![0.8, 0.8];
        sink.is_default = true;
        state.streams.insert(100, sink);
        let mut app = app_stream(10);
        app.binary = Some("firefox".into());
        app.channel_volumes = vec![0.3, 0.3];
        state.streams.insert(10, app);

        update(&mut state, Message::OpenCreateProfileModal);
        update(&mut state, Message::ModalNameChanged("Work".into()));
        update(&mut state, Message::ModalConfirm);

        let saved = state.profiles.find("Work").expect("saved profile");
        assert_eq!(saved.default_sink.as_deref(), Some("alsa_output.usb"));
        assert!((saved.sinks["alsa_output.usb"].volume - 0.8).abs() < 1e-6);
        assert!((saved.apps["bin:firefox"].volume - 0.3).abs() < 1e-6);

        assert_eq!(state.profiles.active.as_deref(), Some("Work"));
        assert!(state.modal.is_none());
    }

    #[test]
    fn create_modal_confirm_with_blank_name_keeps_modal_open_with_error() {
        let mut state = boot_state();
        update(&mut state, Message::OpenCreateProfileModal);
        update(&mut state, Message::ModalNameChanged("   ".into()));
        update(&mut state, Message::ModalConfirm);

        match state.modal {
            Some(Modal::CreateProfile { ref error, .. }) => {
                assert!(error.is_some(), "blank submit should surface an error");
            }
            other => panic!("expected create modal still open, got {other:?}"),
        }
    }

    #[test]
    fn rename_modal_renames_in_place_and_updates_active_marker() {
        let mut state = boot_state();
        for name in ["Work", "Gaming", "Quiet"] {
            state
                .profiles
                .insert_or_replace(profile_with(name, &[], &[], None));
        }
        state.profiles.active = Some("Gaming".into());

        update(&mut state, Message::OpenRenameProfileModal("Gaming".into()));
        update(&mut state, Message::ModalNameChanged("Play".into()));
        update(&mut state, Message::ModalConfirm);

        assert!(state.profiles.find("Gaming").is_none());
        assert!(state.profiles.find("Play").is_some());
        assert_eq!(state.profiles.active.as_deref(), Some("Play"));
        assert!(state.modal.is_none());
        let names: Vec<&str> = state
            .profiles
            .profiles
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(names, vec!["Work", "Play", "Quiet"]);
    }

    #[test]
    fn rename_modal_rejects_collision_with_existing_profile() {
        let mut state = boot_state();
        state
            .profiles
            .insert_or_replace(profile_with("Work", &[], &[], None));
        state
            .profiles
            .insert_or_replace(profile_with("Gaming", &[], &[], None));

        update(&mut state, Message::OpenRenameProfileModal("Work".into()));
        update(&mut state, Message::ModalNameChanged("Gaming".into()));
        update(&mut state, Message::ModalConfirm);

        assert!(state.profiles.find("Work").is_some());
        match state.modal {
            Some(Modal::RenameProfile { ref error, .. }) => {
                assert!(error.is_some());
            }
            other => panic!("expected rename modal still open, got {other:?}"),
        }
    }

    #[test]
    fn delete_modal_confirm_removes_profile() {
        let mut state = boot_state();
        state
            .profiles
            .insert_or_replace(profile_with("Work", &[], &[], None));
        state.profiles.active = Some("Work".into());

        update(&mut state, Message::OpenDeleteProfileModal("Work".into()));
        assert!(matches!(state.modal, Some(Modal::DeleteProfile { .. })));
        update(&mut state, Message::ModalConfirm);

        assert!(state.profiles.find("Work").is_none());
        // Deleting the last profile re-seeds the default.
        assert_eq!(state.profiles.active.as_deref(), Some("Default"));
        assert!(state.modal.is_none());
    }

    #[test]
    fn modal_dismiss_closes_without_acting() {
        let mut state = boot_state();
        state
            .profiles
            .insert_or_replace(profile_with("Work", &[], &[], None));

        update(&mut state, Message::OpenDeleteProfileModal("Work".into()));
        update(&mut state, Message::ModalDismiss);

        assert!(state.profiles.find("Work").is_some());
        assert!(state.modal.is_none());
    }

    #[test]
    fn apply_profile_overrides_inherited_tombstone_state() {
        let mut state = boot_state();
        let mut firefox = app_stream(10);
        firefox.binary = Some("firefox".into());
        firefox.channel_volumes = vec![0.9, 0.9];
        state.streams.insert(10, firefox);
        state.tombstoned.insert(10);

        state.profiles.insert_or_replace(profile_with(
            "Quiet",
            &[],
            &[("bin:firefox", 0.1, false, None)],
            None,
        ));

        update(&mut state, Message::ApplyProfile("Quiet".into()));

        let row = state.streams.get(&10).unwrap();
        assert!((row.average_volume() - 0.1).abs() < 1e-6);
        assert!(state.tombstoned.contains(&10));
    }

    #[test]
    fn slider_change_dirties_when_a_profile_is_active() {
        let mut state = boot_state();
        state.streams.insert(10, app_stream(10));
        state
            .profiles
            .insert_or_replace(profile_with("Work", &[], &[], None));
        state.profiles.active = Some("Work".into());

        update(&mut state, Message::VolumeChanged(10, 1.0));
        assert!(state.dirty);
    }

    #[test]
    fn slider_change_without_active_profile_does_not_dirty() {
        let mut state = boot_state();
        state.streams.insert(10, app_stream(10));

        update(&mut state, Message::VolumeChanged(10, 1.0));
        assert!(!state.dirty);
    }

    #[test]
    fn autosave_tick_writes_current_state_into_active_profile() {
        let mut state = boot_state();
        state
            .streams
            .insert(100, sink_stream(100, "alsa_output.usb"));
        let mut app = app_stream(10);
        app.binary = Some("firefox".into());
        state.streams.insert(10, app);
        state.profiles.insert_or_replace(profile_with(
            "Work",
            &[("alsa_output.usb", 0.2, false)],
            &[("bin:firefox", 0.2, false, None)],
            None,
        ));
        state.profiles.active = Some("Work".into());

        update(&mut state, Message::VolumeChanged(100, 1.0));
        update(&mut state, Message::VolumeChanged(10, 1.0));
        assert!(state.dirty);

        update(&mut state, Message::AutoSaveTick);
        assert!(!state.dirty);

        let saved = state.profiles.find("Work").unwrap();
        assert!((saved.sinks["alsa_output.usb"].volume - 1.0).abs() < 1e-6);
        assert!((saved.apps["bin:firefox"].volume - 1.0).abs() < 1e-6);
    }

    #[test]
    fn autosave_tick_is_noop_without_active_profile() {
        let mut state = boot_state();
        state.streams.insert(10, app_stream(10));

        update(&mut state, Message::AutoSaveTick);
        assert!(state.profiles.profiles.is_empty());
        assert!(!state.dirty);
    }

    #[test]
    fn geometry_changed_with_new_values_dirties_state() {
        let mut state = boot_state();
        assert!(!state.geometry_dirty);

        update(
            &mut state,
            Message::GeometryChanged {
                width: 900,
                height: 800,
                maximized: false,
            },
        );

        assert_eq!(state.geometry.width, 900);
        assert_eq!(state.geometry.height, 800);
        assert!(!state.geometry.maximized);
        assert!(state.geometry_dirty);
    }

    #[test]
    fn geometry_changed_with_same_values_does_not_dirty() {
        let mut state = boot_state();
        let original = state.geometry;

        update(
            &mut state,
            Message::GeometryChanged {
                width: original.width,
                height: original.height,
                maximized: original.maximized,
            },
        );

        assert!(!state.geometry_dirty);
    }

    #[test]
    fn autosave_tick_clears_geometry_dirty_even_without_path() {
        // No store_path: persistence is off but the dirty flag must still
        // clear so the tick doesn't retry forever.
        let mut state = boot_state();
        update(
            &mut state,
            Message::GeometryChanged {
                width: 900,
                height: 800,
                maximized: true,
            },
        );
        assert!(state.geometry_dirty);

        update(&mut state, Message::AutoSaveTick);
        assert!(!state.geometry_dirty);
    }

    #[test]
    fn autosave_tick_persists_geometry_to_disk_when_dirty() {
        let dir = std::env::temp_dir().join(format!(
            "bnksound_state_geom_{}_{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("state.bin");

        let mut state = boot_state();
        state.store_path = Some(path.clone());
        update(
            &mut state,
            Message::GeometryChanged {
                width: 1024,
                height: 768,
                maximized: true,
            },
        );
        update(&mut state, Message::AutoSaveTick);

        let loaded = store::load_from(&path).expect("load");
        assert_eq!(loaded.window.width, 1024);
        assert_eq!(loaded.window.height, 768);
        assert!(loaded.window.maximized);
        assert!(!state.geometry_dirty);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn switching_profile_flushes_dirty_changes_to_previous_active() {
        let mut state = boot_state();
        let mut app = app_stream(10);
        app.binary = Some("firefox".into());
        state.streams.insert(10, app);

        state.profiles.insert_or_replace(profile_with(
            "A",
            &[],
            &[("bin:firefox", 0.2, false, None)],
            None,
        ));
        state.profiles.insert_or_replace(profile_with(
            "B",
            &[],
            &[("bin:firefox", 0.9, false, None)],
            None,
        ));
        state.profiles.active = Some("A".into());

        update(&mut state, Message::VolumeChanged(10, 1.0));
        assert!(state.dirty);

        update(&mut state, Message::ApplyProfile("B".into()));

        let saved_a = state.profiles.find("A").unwrap();
        assert!((saved_a.apps["bin:firefox"].volume - 1.0).abs() < 1e-6);
        assert!((state.streams.get(&10).unwrap().average_volume() - 0.9).abs() < 1e-6);
        assert!(!state.dirty);
        assert_eq!(state.profiles.active.as_deref(), Some("B"));
    }

    #[test]
    fn reapplying_active_profile_does_not_flush_dirty() {
        let mut state = boot_state();
        let mut app = app_stream(10);
        app.binary = Some("firefox".into());
        state.streams.insert(10, app);

        state.profiles.insert_or_replace(profile_with(
            "Work",
            &[],
            &[("bin:firefox", 0.2, false, None)],
            None,
        ));
        state.profiles.active = Some("Work".into());

        update(&mut state, Message::VolumeChanged(10, 1.0));
        assert!(state.dirty);

        update(&mut state, Message::ApplyProfile("Work".into()));

        assert!((state.streams.get(&10).unwrap().average_volume() - 0.2).abs() < 1e-6);
        let saved = state.profiles.find("Work").unwrap();
        assert!((saved.apps["bin:firefox"].volume - 0.2).abs() < 1e-6);
        assert!(!state.dirty);
    }

    #[test]
    fn toggle_palette_opens_and_closes_with_reset() {
        let mut state = boot_state();
        state.palette_query = "leftover".into();
        state.palette_selected = 5;

        update(&mut state, Message::TogglePalette);
        assert!(state.palette_open);
        assert!(state.palette_query.is_empty());
        assert_eq!(state.palette_selected, 0);

        state.palette_query = "typed".into();
        state.palette_selected = 3;
        update(&mut state, Message::TogglePalette);
        assert!(!state.palette_open);
        assert!(state.palette_query.is_empty());
        assert_eq!(state.palette_selected, 0);
    }

    #[test]
    fn palette_query_change_resets_selection() {
        let mut state = boot_state();
        state.palette_open = true;
        state.palette_selected = 4;

        update(&mut state, Message::PaletteQueryChanged("mute".into()));
        assert_eq!(state.palette_query, "mute");
        assert_eq!(state.palette_selected, 0);
    }

    #[test]
    fn app_row_key_groups_by_identity_and_falls_back_to_node_id() {
        let mut helium_a = app_stream(113);
        helium_a.binary = Some("helium".into());
        let mut helium_b = app_stream(124);
        helium_b.binary = Some("helium".into());

        assert_eq!(app_row_key(&helium_a), app_row_key(&helium_b));

        let bare = app_stream(7);
        assert_eq!(app_row_key(&bare), "node:7");
    }

    fn helium_pair_state() -> (App, String) {
        let mut state = boot_state();
        let mut a = app_stream(113);
        a.binary = Some("helium".into());
        a.channel_volumes = vec![0.3, 0.3];
        let mut b = app_stream(124);
        b.binary = Some("helium".into());
        b.channel_volumes = vec![1.0, 1.0];
        state.streams.insert(a.id, a);
        state.streams.insert(b.id, b);
        let key = "bin:helium".to_string();
        (state, key)
    }

    #[test]
    fn group_volume_proportional_scaling_preserves_ratio() {
        let (mut state, key) = helium_pair_state();
        // Old master cubic = max(cbrt(0.3), cbrt(1.0)) = 1.0; ratio 0.5.
        let target_cubic = 0.5_f32;
        update(
            &mut state,
            Message::GroupVolumeChanged {
                key: key.clone(),
                cubic: target_cubic,
            },
        );

        let a_linear = state.streams[&113].average_volume();
        let b_linear = state.streams[&124].average_volume();
        // B (the master) lands at target_cubic; A keeps the cubic ratio.
        let b_cubic = linear_to_cubic(b_linear);
        assert!((b_cubic - target_cubic).abs() < 1e-4, "b_cubic={b_cubic}");
        let a_cubic = linear_to_cubic(a_linear);
        let expected_a_cubic = linear_to_cubic(0.3) * 0.5;
        assert!(
            (a_cubic - expected_a_cubic).abs() < 1e-4,
            "a_cubic={a_cubic}, expected≈{expected_a_cubic}",
        );
    }

    #[test]
    fn group_volume_uniform_when_all_members_silent() {
        let mut state = boot_state();
        let mut a = app_stream(113);
        a.binary = Some("helium".into());
        a.channel_volumes = vec![0.0, 0.0];
        let mut b = app_stream(124);
        b.binary = Some("helium".into());
        b.channel_volumes = vec![0.0, 0.0];
        state.streams.insert(a.id, a);
        state.streams.insert(b.id, b);

        update(
            &mut state,
            Message::GroupVolumeChanged {
                key: "bin:helium".into(),
                cubic: 0.5,
            },
        );

        let expected = cubic_to_linear(0.5);
        for id in [113, 124] {
            assert!((state.streams[&id].average_volume() - expected).abs() < 1e-6);
        }
    }

    #[test]
    fn group_volume_clamps_each_member_to_max_volume() {
        let mut state = boot_state();
        let mut a = app_stream(113);
        a.binary = Some("helium".into());
        a.channel_volumes = vec![0.5, 0.5];
        let mut b = app_stream(124);
        b.binary = Some("helium".into());
        b.channel_volumes = vec![1.0, 1.0];
        state.streams.insert(a.id, a);
        state.streams.insert(b.id, b);

        // Master cubic ~= 1.0 → ratio 1.5; member B would land at 1.5
        // cubic but stays at MAX_VOLUME, while A scales freely.
        update(
            &mut state,
            Message::GroupVolumeChanged {
                key: "bin:helium".into(),
                cubic: MAX_VOLUME,
            },
        );

        let b_cubic = linear_to_cubic(state.streams[&124].average_volume());
        assert!((b_cubic - MAX_VOLUME).abs() < 1e-4);
    }

    #[test]
    fn group_mute_mutes_all_when_any_unmuted() {
        let (mut state, key) = helium_pair_state();
        // a unmuted, b muted → toggling should mute all.
        state.streams.get_mut(&124).unwrap().muted = true;

        update(&mut state, Message::GroupMuteToggled(key));
        assert!(state.streams[&113].muted);
        assert!(state.streams[&124].muted);
    }

    #[test]
    fn group_mute_unmutes_all_when_already_all_muted() {
        let (mut state, key) = helium_pair_state();
        state.streams.get_mut(&113).unwrap().muted = true;
        state.streams.get_mut(&124).unwrap().muted = true;

        update(&mut state, Message::GroupMuteToggled(key));
        assert!(!state.streams[&113].muted);
        assert!(!state.streams[&124].muted);
    }

    #[test]
    fn group_set_target_pins_every_member() {
        let (mut state, key) = helium_pair_state();
        state
            .streams
            .insert(100, sink_stream(100, "alsa_output.usb"));

        update(
            &mut state,
            Message::GroupSetStreamTarget { key, sink_id: 100 },
        );
        assert_eq!(
            state.streams[&113].target_sink_name.as_deref(),
            Some("alsa_output.usb")
        );
        assert_eq!(
            state.streams[&124].target_sink_name.as_deref(),
            Some("alsa_output.usb")
        );
    }

    #[test]
    fn group_clear_target_unpins_every_member() {
        let (mut state, key) = helium_pair_state();
        state.streams.get_mut(&113).unwrap().target_sink_name = Some("alsa_output.usb".into());
        state.streams.get_mut(&124).unwrap().target_sink_name = Some("alsa_output.usb".into());

        update(&mut state, Message::GroupClearStreamTarget(key));
        assert_eq!(state.streams[&113].target_sink_name, None);
        assert_eq!(state.streams[&124].target_sink_name, None);
    }

    #[test]
    fn group_toggle_expanded_flips_membership_for_multi_member_groups() {
        let (mut state, key) = helium_pair_state();
        assert!(state.expanded_groups.is_empty());

        update(&mut state, Message::GroupToggleExpanded(key.clone()));
        assert!(state.expanded_groups.contains(&key));

        update(&mut state, Message::GroupToggleExpanded(key.clone()));
        assert!(!state.expanded_groups.contains(&key));
    }

    #[test]
    fn group_toggle_expanded_noops_for_single_member_groups() {
        let mut state = boot_state();
        let mut spotify = app_stream(50);
        spotify.binary = Some("spotify".into());
        state.streams.insert(50, spotify);

        update(
            &mut state,
            Message::GroupToggleExpanded("bin:spotify".into()),
        );
        assert!(
            state.expanded_groups.is_empty(),
            "single-member group should not expand"
        );
    }

    #[test]
    fn group_toggle_expanded_drops_key_when_group_shrinks_to_one() {
        // Two-tab Helium expanded; if one tab disappears the toggle
        // path normalises away the now-pointless expansion the next
        // time the user tries to interact with it.
        let (mut state, key) = helium_pair_state();
        state.expanded_groups.insert(key.clone());
        state.streams.remove(&124);

        update(&mut state, Message::GroupToggleExpanded(key.clone()));
        assert!(!state.expanded_groups.contains(&key));
    }

    #[test]
    fn members_of_app_row_filters_to_matching_app_streams_only() {
        let (mut state, key) = helium_pair_state();
        // Spotify and a sink should not appear among Helium members.
        let mut spotify = app_stream(99);
        spotify.binary = Some("spotify".into());
        state.streams.insert(99, spotify);
        state
            .streams
            .insert(100, sink_stream(100, "alsa_output.usb"));

        let mut members = members_of_app_row(&state, &key);
        members.sort();
        assert_eq!(members, vec![113, 124]);
    }

    #[test]
    fn palette_select_next_and_prev_wrap_within_filtered_count() {
        let mut state = boot_state();
        state.palette_open = true;
        // No streams, no profiles → only "profile: create" matches, so
        // the visible window has exactly one row.
        update(&mut state, Message::PaletteSelectNext);
        assert_eq!(state.palette_selected, 0, "single-row list stays put");
        update(&mut state, Message::PaletteSelectPrev);
        assert_eq!(state.palette_selected, 0);

        // Two profiles add enough commands for navigation to move.
        state.profiles.insert_or_replace(Profile {
            name: "A".into(),
            ..Profile::default()
        });
        state.profiles.insert_or_replace(Profile {
            name: "B".into(),
            ..Profile::default()
        });

        update(&mut state, Message::PaletteSelectNext);
        assert_eq!(state.palette_selected, 1);
        update(&mut state, Message::PaletteSelectPrev);
        assert_eq!(state.palette_selected, 0);
        // Wrap-around from the top.
        update(&mut state, Message::PaletteSelectPrev);
        let cmd_count = command_palette::build_commands(&state).len();
        assert_eq!(state.palette_selected, cmd_count - 1);
    }

    fn source_stream(id: u32, node_name: &str) -> AudioStream {
        let mut s = sink_stream(id, node_name);
        s.kind = StreamKind::Source;
        s
    }

    #[test]
    fn apply_profile_applies_live_sources_and_stages_unmatched() {
        let mut state = boot_state();
        // Live capture device matching one profile source entry.
        state
            .streams
            .insert(300, source_stream(300, "alsa_input.usb"));

        let mut profile = Profile {
            name: "Call".into(),
            // Default points at a source that isn't connected yet, so it
            // must be staged rather than applied.
            default_source: Some("bluez_input.headset".into()),
            ..Profile::default()
        };
        profile.sources.insert(
            "alsa_input.usb".into(),
            DeviceSettings {
                volume: 0.6,
                muted: true,
            },
        );
        profile.sources.insert(
            "bluez_input.headset".into(),
            DeviceSettings {
                volume: 0.4,
                muted: false,
            },
        );
        state.profiles.insert_or_replace(profile);

        update(&mut state, Message::ApplyProfile("Call".into()));

        // Live source took the saved volume + mute locally.
        let mic = state.streams.get(&300).unwrap();
        assert!((mic.average_volume() - 0.6).abs() < 1e-6);
        assert!(mic.muted);

        // Absent source + default both staged for when the device shows up.
        assert!(state.pending.sources.contains_key("bluez_input.headset"));
        assert_eq!(
            state.pending.default_source.as_deref(),
            Some("bluez_input.headset"),
        );
    }

    #[test]
    fn pending_source_applies_when_matching_source_connects() {
        let mut state = boot_state();
        state.pending.sources.insert(
            "bluez_input.headset".into(),
            DeviceSettings {
                volume: 0.5,
                muted: true,
            },
        );

        let new_source = source_stream(400, "bluez_input.headset");
        update(
            &mut state,
            Message::Worker(Box::new(WorkerEvent::StreamAdded(new_source))),
        );

        let live = state.streams.get(&400).unwrap();
        assert!((live.average_volume() - 0.5).abs() < 1e-6);
        assert!(live.muted);
        assert!(state.pending.sources.is_empty());
    }

    #[test]
    fn stream_removed_for_source_drops_immediately() {
        let mut state = boot_state();
        state
            .streams
            .insert(300, source_stream(300, "alsa_input.usb"));

        update(
            &mut state,
            Message::Worker(Box::new(WorkerEvent::StreamRemoved(300))),
        );

        // Capture devices vanish on unplug just like sinks, no ghost row.
        assert!(!state.streams.contains_key(&300));
        assert!(!state.tombstoned.contains(&300));
    }

    #[test]
    fn toggle_section_flips_only_that_section() {
        let mut state = boot_state();
        // An active profile so the toggle has somewhere to persist.
        state.profiles.insert_or_replace(Profile {
            name: "Work".into(),
            ..Profile::default()
        });
        state.profiles.active = Some("Work".into());
        assert!(state.shows_section(Section::Inputs));

        update(&mut state, Message::ToggleSection(Section::Inputs));
        assert!(!state.shows_section(Section::Inputs));
        // The toggle dirties the active profile so the autosave tick
        // folds the new layout into it.
        assert!(state.dirty);
        // Other sections are untouched.
        assert!(state.shows_section(Section::Outputs));
        assert!(state.shows_section(Section::Apps));

        // Toggling again restores it.
        update(&mut state, Message::ToggleSection(Section::Inputs));
        assert!(state.shows_section(Section::Inputs));
    }

    #[test]
    fn toggled_filter_persists_into_active_profile_on_autosave() {
        let mut state = boot_state();
        state.profiles.insert_or_replace(Profile {
            name: "Work".into(),
            ..Profile::default()
        });
        state.profiles.active = Some("Work".into());

        update(&mut state, Message::ToggleSection(Section::Apps));
        update(&mut state, Message::AutoSaveTick);

        let saved = state.profiles.find("Work").unwrap();
        assert!(!saved.section_filter.apps);
        assert!(saved.section_filter.outputs);
        assert!(saved.section_filter.inputs);
    }

    #[test]
    fn mute_all_mutes_every_output_then_unmutes() {
        let mut state = boot_state();
        state.streams.insert(1, sink_stream(1, "alsa_output.a"));
        state.streams.insert(2, sink_stream(2, "alsa_output.b"));
        // A source is left untouched by the output-only mute-all.
        state.streams.insert(3, source_stream(3, "alsa_input.mic"));
        assert!(!state.all_outputs_muted());

        update(&mut state, Message::MuteAllToggled);
        assert!(state.streams[&1].muted);
        assert!(state.streams[&2].muted);
        assert!(!state.streams[&3].muted);
        assert!(state.all_outputs_muted());

        update(&mut state, Message::MuteAllToggled);
        assert!(!state.streams[&1].muted);
        assert!(!state.streams[&2].muted);
        assert!(!state.all_outputs_muted());
    }
}
