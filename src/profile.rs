//! Profile data model: pure value types capturing the user-visible audio
//! state (per-device and per-app volume/mute, sink pins, defaults, section
//! filter) as named presets. Sinks/sources key by `node.name`, apps by
//! [`Stream::app_identity`] since node ids are ephemeral. Persistence
//! lives in [`crate::store`].

use std::collections::BTreeMap;

use crate::domain::{SectionFilter, Stream, StreamKind};

/// Saved volume + mute for one device (sink or source), keyed by
/// `node.name`. Shared by both directions; pins live on [`AppSettings`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeviceSettings {
    /// Linear gain, matching PipeWire's `channelVolumes` (raw, the UI
    /// cubic conversion happens at the slider).
    pub volume: f32,
    pub muted: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppSettings {
    pub volume: f32,
    pub muted: bool,
    /// `Some(node_name)` pins the app to that sink, `None` follows the
    /// default sink. Same semantics as `Stream::target_sink_name`.
    pub target_sink_name: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Profile {
    pub name: String,
    /// Sinks keyed by `node.name` (e.g. `alsa_output.usb`,
    /// `bluez_output.headset`). Stable across reboots / reconnects.
    pub sinks: BTreeMap<String, DeviceSettings>,
    /// App streams keyed by [`Stream::app_identity`] (e.g.
    /// `app:com.spotify.Client`, `bin:firefox`).
    pub apps: BTreeMap<String, AppSettings>,
    /// Sources (capture devices) keyed by `node.name`, mirroring `sinks`.
    pub sources: BTreeMap<String, DeviceSettings>,
    /// Default sink at save time, replayed on apply via `SetDefaultSink`.
    pub default_sink: Option<String>,
    /// Default source at save time, replayed on apply via `SetDefaultSource`.
    pub default_source: Option<String>,
    /// IN/OUT/APP column visibility, restored on apply and snapshotted on
    /// save. Defaults to all-on.
    pub section_filter: SectionFilter,
}

impl Profile {
    /// Snapshot the user-visible state into a fresh profile. Streams
    /// without a stable key (no `node.name` / no `app_identity()`) are
    /// skipped, since they can't be re-matched on apply.
    pub fn snapshot(
        name: String,
        streams: &BTreeMap<u32, Stream>,
        default_sink: Option<&str>,
        default_source: Option<&str>,
        section_filter: SectionFilter,
    ) -> Self {
        let mut sinks = BTreeMap::new();
        let mut sources = BTreeMap::new();
        let mut apps = BTreeMap::new();
        for stream in streams.values() {
            match stream.kind {
                StreamKind::Sink => {
                    if let Some(node_name) = stream.node_name.as_deref() {
                        sinks.insert(
                            node_name.to_string(),
                            DeviceSettings {
                                volume: stream.average_volume(),
                                muted: stream.muted,
                            },
                        );
                    }
                }
                StreamKind::Source => {
                    if let Some(node_name) = stream.node_name.as_deref() {
                        sources.insert(
                            node_name.to_string(),
                            DeviceSettings {
                                volume: stream.average_volume(),
                                muted: stream.muted,
                            },
                        );
                    }
                }
                StreamKind::Application => {
                    if let Some(key) = stream.app_identity() {
                        apps.insert(
                            key,
                            AppSettings {
                                volume: stream.average_volume(),
                                muted: stream.muted,
                                target_sink_name: stream.target_sink_name.clone(),
                            },
                        );
                    }
                }
            }
        }
        Profile {
            name,
            sinks,
            apps,
            sources,
            default_sink: default_sink.map(str::to_string),
            default_source: default_source.map(str::to_string),
            section_filter,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProfileStore {
    pub profiles: Vec<Profile>,
    /// Name of the last-applied profile, highlighting the active chip.
    /// `None` when none is applied or the matching profile is deleted.
    pub active: Option<String>,
}

/// Profile entries with no matching live stream at apply time, consumed
/// as matching streams arrive. Re-populated on each profile apply.
#[derive(Debug, Clone, Default)]
pub struct Pending {
    pub sinks: BTreeMap<String, DeviceSettings>,
    pub apps: BTreeMap<String, AppSettings>,
    pub sources: BTreeMap<String, DeviceSettings>,
    pub default_sink: Option<String>,
    pub default_source: Option<String>,
}

impl Pending {
    pub fn is_empty(&self) -> bool {
        self.sinks.is_empty()
            && self.apps.is_empty()
            && self.sources.is_empty()
            && self.default_sink.is_none()
            && self.default_source.is_none()
    }
}

impl ProfileStore {
    pub fn find(&self, name: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.name == name)
    }

    pub fn insert_or_replace(&mut self, profile: Profile) {
        if let Some(slot) = self.profiles.iter_mut().find(|p| p.name == profile.name) {
            *slot = profile;
        } else {
            self.profiles.push(profile);
        }
    }

    /// Remove a profile by name, clearing the active marker if it pointed
    /// at the removed profile.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.profiles.len();
        self.profiles.retain(|p| p.name != name);
        if self.active.as_deref() == Some(name) {
            self.active = None;
        }
        self.profiles.len() != before
    }

    /// Rename in place so the chip keeps its slot. Does not check for
    /// name collisions; the caller must reject those.
    pub fn rename(&mut self, old_name: &str, new_name: String) -> bool {
        let Some(idx) = self.profiles.iter().position(|p| p.name == old_name) else {
            return false;
        };
        self.profiles[idx].name = new_name.clone();
        if self.active.as_deref() == Some(old_name) {
            self.active = Some(new_name);
        }
        true
    }

    /// Move `name` immediately before or after `target` in the row.
    /// Returns `false` (no mutation) when `name == target`, either profile
    /// is missing, or the order wouldn't change.
    pub fn reorder(&mut self, name: &str, target: &str, before: bool) -> bool {
        if name == target {
            return false;
        }
        let Some(src_idx) = self.profiles.iter().position(|p| p.name == name) else {
            return false;
        };
        let Some(target_idx) = self.profiles.iter().position(|p| p.name == target) else {
            return false;
        };
        let desired_idx = if before { target_idx } else { target_idx + 1 };
        // Removing the source first shifts everything right of src_idx down
        // by one, so adjust the insertion index to compensate.
        let insert_at = if src_idx < desired_idx {
            desired_idx - 1
        } else {
            desired_idx
        };
        if insert_at == src_idx {
            return false;
        }
        let moved = self.profiles.remove(src_idx);
        self.profiles.insert(insert_at, moved);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{SectionFilter, StreamKind};

    fn app_stream(id: u32, ident: &str) -> Stream {
        Stream {
            id,
            kind: StreamKind::Application,
            name: format!("stream-{id}"),
            app_id: None,
            binary: Some(ident.into()),
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

    fn sink_stream(id: u32, node_name: &str) -> Stream {
        let mut s = app_stream(id, "");
        s.kind = StreamKind::Sink;
        s.binary = None;
        s.node_name = Some(node_name.into());
        s
    }

    #[test]
    fn snapshot_keys_sinks_by_node_name_and_apps_by_identity() {
        let mut streams = BTreeMap::new();
        let mut sink = sink_stream(100, "alsa_output.usb");
        sink.channel_volumes = vec![0.8, 0.8];
        streams.insert(sink.id, sink);

        let mut app = app_stream(10, "firefox");
        app.channel_volumes = vec![0.3, 0.3];
        app.muted = true;
        app.target_sink_name = Some("bluez_output.headset".into());
        streams.insert(app.id, app);

        let profile = Profile::snapshot(
            "Work".into(),
            &streams,
            Some("alsa_output.usb"),
            None,
            SectionFilter::default(),
        );

        assert_eq!(profile.name, "Work");
        assert_eq!(profile.default_sink.as_deref(), Some("alsa_output.usb"));
        assert_eq!(profile.sinks.len(), 1);
        let saved_sink = profile.sinks.get("alsa_output.usb").unwrap();
        assert!((saved_sink.volume - 0.8).abs() < 1e-6);
        assert!(!saved_sink.muted);

        let saved_app = profile.apps.get("bin:firefox").unwrap();
        assert!((saved_app.volume - 0.3).abs() < 1e-6);
        assert!(saved_app.muted);
        assert_eq!(
            saved_app.target_sink_name.as_deref(),
            Some("bluez_output.headset")
        );
    }

    #[test]
    fn snapshot_skips_streams_without_stable_key() {
        let mut streams = BTreeMap::new();
        // Sink with no node_name: can't be re-matched on apply.
        let sink_no_name = sink_stream(100, "");
        let mut sink_no_name = sink_no_name;
        sink_no_name.node_name = None;
        streams.insert(sink_no_name.id, sink_no_name);

        // App with no identifying hints.
        let mut bare_app = app_stream(10, "");
        bare_app.binary = None;
        streams.insert(bare_app.id, bare_app);

        let profile = Profile::snapshot("p".into(), &streams, None, None, SectionFilter::default());
        assert!(profile.sinks.is_empty());
        assert!(profile.apps.is_empty());
    }

    #[test]
    fn insert_or_replace_overwrites_by_name() {
        let mut store = ProfileStore::default();
        store.insert_or_replace(Profile {
            name: "Work".into(),
            default_sink: Some("a".into()),
            ..Profile::default()
        });
        store.insert_or_replace(Profile {
            name: "Work".into(),
            default_sink: Some("b".into()),
            ..Profile::default()
        });
        assert_eq!(store.profiles.len(), 1);
        assert_eq!(store.profiles[0].default_sink.as_deref(), Some("b"));
    }

    #[test]
    fn rename_preserves_slot_position() {
        let mut store = ProfileStore::default();
        for name in ["A", "B", "C"] {
            store.insert_or_replace(Profile {
                name: name.into(),
                ..Profile::default()
            });
        }
        assert!(store.rename("B", "Beta".into()));
        let names: Vec<&str> = store.profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["A", "Beta", "C"]);
    }

    #[test]
    fn rename_updates_active_marker_when_active_profile_is_renamed() {
        let mut store = ProfileStore::default();
        store.insert_or_replace(Profile {
            name: "Work".into(),
            ..Profile::default()
        });
        store.active = Some("Work".into());
        assert!(store.rename("Work", "Workspace".into()));
        assert_eq!(store.active.as_deref(), Some("Workspace"));
    }

    #[test]
    fn rename_returns_false_for_unknown_profile() {
        let mut store = ProfileStore::default();
        store.insert_or_replace(Profile {
            name: "Work".into(),
            ..Profile::default()
        });
        assert!(!store.rename("Missing", "Other".into()));
        assert_eq!(store.profiles.len(), 1);
        assert_eq!(store.profiles[0].name, "Work");
    }

    fn store_with(names: &[&str]) -> ProfileStore {
        let mut store = ProfileStore::default();
        for name in names {
            store.insert_or_replace(Profile {
                name: (*name).into(),
                ..Profile::default()
            });
        }
        store
    }

    fn order(store: &ProfileStore) -> Vec<&str> {
        store.profiles.iter().map(|p| p.name.as_str()).collect()
    }

    #[test]
    fn reorder_moves_chip_before_target() {
        let mut store = store_with(&["A", "B", "C", "D"]);
        assert!(store.reorder("D", "B", true));
        assert_eq!(order(&store), vec!["A", "D", "B", "C"]);
    }

    #[test]
    fn reorder_moves_chip_after_target() {
        let mut store = store_with(&["A", "B", "C", "D"]);
        assert!(store.reorder("A", "C", false));
        assert_eq!(order(&store), vec!["B", "C", "A", "D"]);
    }

    #[test]
    fn reorder_handles_forward_move_with_adjusted_index() {
        let mut store = store_with(&["A", "B", "C", "D"]);
        // Forward move: removing A shifts the target, reorder compensates.
        assert!(store.reorder("A", "D", true));
        assert_eq!(order(&store), vec!["B", "C", "A", "D"]);
    }

    #[test]
    fn reorder_is_noop_when_position_would_not_change() {
        let mut store = store_with(&["A", "B", "C"]);
        // B already sits right before C.
        assert!(!store.reorder("B", "C", true));
        // A already sits right before B.
        assert!(!store.reorder("A", "B", true));
        assert_eq!(order(&store), vec!["A", "B", "C"]);
    }

    #[test]
    fn reorder_is_noop_when_dropping_on_self() {
        let mut store = store_with(&["A", "B"]);
        assert!(!store.reorder("A", "A", true));
        assert!(!store.reorder("A", "A", false));
        assert_eq!(order(&store), vec!["A", "B"]);
    }

    #[test]
    fn reorder_is_noop_when_either_profile_is_missing() {
        let mut store = store_with(&["A", "B"]);
        assert!(!store.reorder("Missing", "A", true));
        assert!(!store.reorder("A", "Missing", true));
        assert_eq!(order(&store), vec!["A", "B"]);
    }

    #[test]
    fn remove_clears_active_when_dropping_active_profile() {
        let mut store = ProfileStore::default();
        store.insert_or_replace(Profile {
            name: "Work".into(),
            ..Profile::default()
        });
        store.active = Some("Work".into());
        assert!(store.remove("Work"));
        assert!(store.active.is_none());
    }

    #[test]
    fn remove_preserves_active_when_dropping_other_profile() {
        let mut store = ProfileStore::default();
        store.insert_or_replace(Profile {
            name: "Work".into(),
            ..Profile::default()
        });
        store.insert_or_replace(Profile {
            name: "Gaming".into(),
            ..Profile::default()
        });
        store.active = Some("Work".into());
        assert!(store.remove("Gaming"));
        assert_eq!(store.active.as_deref(), Some("Work"));
    }

    fn source_stream(id: u32, node_name: &str) -> Stream {
        let mut s = sink_stream(id, node_name);
        s.kind = StreamKind::Source;
        s
    }

    #[test]
    fn snapshot_captures_sources_keyed_by_node_name() {
        let mut streams = BTreeMap::new();
        let mut mic = source_stream(200, "alsa_input.usb");
        mic.channel_volumes = vec![0.6, 0.6];
        mic.muted = true;
        streams.insert(mic.id, mic);

        let profile = Profile::snapshot(
            "Call".into(),
            &streams,
            None,
            Some("alsa_input.usb"),
            SectionFilter::default(),
        );

        assert_eq!(profile.default_source.as_deref(), Some("alsa_input.usb"));
        assert_eq!(profile.sources.len(), 1);
        let saved = profile.sources.get("alsa_input.usb").unwrap();
        assert!((saved.volume - 0.6).abs() < 1e-6);
        assert!(saved.muted);
        // Sources don't leak into the sinks map.
        assert!(profile.sinks.is_empty());
    }
}
