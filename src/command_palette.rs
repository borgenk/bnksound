//! Ctrl+K command palette: derives a flat list of executable commands
//! from the current `App` snapshot, with fuzzy filtering. Each row
//! dispatches an embedded `Message` on activation. The list is rebuilt
//! every keystroke, so there's no separate model to sync.

use crate::domain::{Stream as AudioStream, StreamKind};
use crate::state::{App, Message};

/// One palette row: a label and the `Message` dispatched on activation.
pub struct PaletteCommand {
    pub label: String,
    pub message: Message,
}

/// Build the full command list from the current app snapshot. Order is
/// fixed (profiles, sinks, mute, pins); ranking happens in
/// [`filter_commands`].
pub fn build_commands(state: &App) -> Vec<PaletteCommand> {
    let mut cmds = Vec::new();

    // Create / rename / delete open a modal so the palette never mutates
    // persisted state without a confirm step.
    cmds.push(PaletteCommand {
        label: "profile: create".into(),
        message: Message::OpenCreateProfileModal,
    });
    for profile in &state.profiles.profiles {
        let active = state.profiles.active.as_deref() == Some(profile.name.as_str());
        let suffix = if active { " (active)" } else { "" };
        cmds.push(PaletteCommand {
            label: format!("profile: apply \u{2192} {}{}", profile.name, suffix),
            message: Message::ApplyProfile(profile.name.clone()),
        });
    }
    for profile in &state.profiles.profiles {
        cmds.push(PaletteCommand {
            label: format!("profile: rename {}", profile.name),
            message: Message::OpenRenameProfileModal(profile.name.clone()),
        });
    }
    for profile in &state.profiles.profiles {
        cmds.push(PaletteCommand {
            label: format!("profile: delete {}", profile.name),
            message: Message::OpenDeleteProfileModal(profile.name.clone()),
        });
    }

    let mut sinks: Vec<&AudioStream> = Vec::new();
    let mut sources: Vec<&AudioStream> = Vec::new();
    let mut apps: Vec<&AudioStream> = Vec::new();
    for s in state.streams.values() {
        match s.kind {
            StreamKind::Sink => sinks.push(s),
            StreamKind::Source => sources.push(s),
            StreamKind::Application => apps.push(s),
        }
    }
    sinks.sort_by_key(|s| s.id);
    sources.sort_by_key(|s| s.id);
    apps.sort_by_key(|s| s.id);

    for s in &sinks {
        if !s.is_default {
            cmds.push(PaletteCommand {
                label: format!("sink: make default \u{2192} {}", s.display_name()),
                message: Message::MakeDefault(s.id),
            });
        }
    }
    for s in &sources {
        if !s.is_default {
            cmds.push(PaletteCommand {
                label: format!("input: make default \u{2192} {}", s.display_name()),
                message: Message::MakeDefaultSource(s.id),
            });
        }
    }

    for s in &sinks {
        let verb = if s.muted { "unmute" } else { "mute" };
        cmds.push(PaletteCommand {
            label: format!("sink: {verb} {}", s.display_name()),
            message: Message::MuteToggled(s.id),
        });
    }
    for s in &sources {
        let verb = if s.muted { "unmute" } else { "mute" };
        cmds.push(PaletteCommand {
            // MuteToggled is kind-agnostic (keyed by node id).
            label: format!("input: {verb} {}", s.display_name()),
            message: Message::MuteToggled(s.id),
        });
    }
    for s in &apps {
        let verb = if s.muted { "unmute" } else { "mute" };
        cmds.push(PaletteCommand {
            label: format!("app: {verb} {}", s.display_name()),
            message: Message::MuteToggled(s.id),
        });
    }

    // Per-app pin commands. With a single sink there's nothing to route,
    // so skip them entirely.
    if sinks.len() >= 2 {
        let any_pinned = apps.iter().any(|s| s.target_sink_name.is_some());
        if any_pinned {
            cmds.push(PaletteCommand {
                label: "pin: reset all".into(),
                message: Message::ResetAllStreamTargets,
            });
        }
        for app in &apps {
            if app.target_sink_name.is_some() {
                cmds.push(PaletteCommand {
                    label: format!("pin: clear {}", app.display_name()),
                    message: Message::ClearStreamTarget(app.id),
                });
            }
            for sink in &sinks {
                // Already-pinned routes don't need a no-op palette entry.
                let already_here = app.target_sink_name.is_some()
                    && app.target_sink_name.as_deref() == sink.node_name.as_deref();
                if already_here {
                    continue;
                }
                cmds.push(PaletteCommand {
                    label: format!(
                        "pin: {} \u{2192} {}",
                        app.display_name(),
                        sink.display_name()
                    ),
                    message: Message::SetStreamTarget {
                        app_id: app.id,
                        sink_id: sink.id,
                    },
                });
            }
        }
    }

    cmds
}

/// Indices into `commands` ranked by match against `query`, best first.
/// An empty query returns every index in original order.
pub fn filter_commands(commands: &[PaletteCommand], query: &str) -> Vec<usize> {
    if query.is_empty() {
        return (0..commands.len()).collect();
    }
    let query_lower = query.to_lowercase();
    let mut scored: Vec<(usize, u8)> = commands
        .iter()
        .enumerate()
        .filter_map(|(i, c)| score_match(&c.label.to_lowercase(), &query_lower).map(|s| (i, s)))
        .collect();
    // Stable sort preserves the build_commands ordering within each tier.
    scored.sort_by_key(|&(_, s)| s);
    scored.into_iter().map(|(i, _)| i).collect()
}

/// Tiered match score. Lower wins.
///   0 = every query word is a prefix of some label word
///   1 = the query is a substring of the label
///   2 = every query character appears in order somewhere in the label
///   None = no match
fn score_match(label: &str, query: &str) -> Option<u8> {
    let label_words: Vec<&str> = label
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let query_words: Vec<&str> = query.split_whitespace().collect();

    if !query_words.is_empty() {
        let all_prefix = query_words
            .iter()
            .all(|qw| label_words.iter().any(|lw| lw.starts_with(qw)));
        if all_prefix {
            return Some(0);
        }
    }

    if label.contains(query) {
        return Some(1);
    }

    let mut chars = label.chars();
    let is_fuzzy = query.chars().all(|qc| chars.any(|lc| lc == qc));
    if is_fuzzy {
        return Some(2);
    }

    None
}

/// Hard cap on visible rows so a runaway list (lots of apps × lots of
/// sinks) doesn't blow up the panel height.
pub const MAX_VISIBLE: usize = 40;

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use super::*;
    use crate::geometry::Geometry;
    use crate::profile::{Pending, Profile, ProfileStore};
    use crate::state::App;

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
            section_filter: crate::domain::SectionFilter::default(),
        }
    }

    fn app_stream(id: u32) -> AudioStream {
        AudioStream {
            id,
            kind: StreamKind::Application,
            name: format!("app-{id}"),
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

    fn sink_stream(id: u32, node_name: &str) -> AudioStream {
        let mut s = app_stream(id);
        s.kind = StreamKind::Sink;
        s.name = format!("sink-{id}");
        s.node_name = Some(node_name.into());
        s
    }

    fn empty_profile(name: &str) -> Profile {
        Profile {
            name: name.into(),
            ..Profile::default()
        }
    }

    #[test]
    fn always_includes_create() {
        let cmds = build_commands(&boot_state());
        assert!(cmds.iter().any(|c| c.label == "profile: create"));
    }

    #[test]
    fn applies_renames_and_deletes_appear_for_each_profile() {
        let mut state = boot_state();
        state.profiles.insert_or_replace(empty_profile("Work"));
        state.profiles.insert_or_replace(empty_profile("Gaming"));
        let cmds = build_commands(&state);

        let labels: Vec<&str> = cmds.iter().map(|c| c.label.as_str()).collect();
        assert!(
            labels
                .iter()
                .any(|l| l.contains("apply") && l.contains("Work"))
        );
        assert!(
            labels
                .iter()
                .any(|l| l.contains("apply") && l.contains("Gaming"))
        );
        assert!(labels.iter().any(|l| l.contains("rename Work")));
        assert!(labels.iter().any(|l| l.contains("rename Gaming")));
        assert!(labels.iter().any(|l| l.contains("delete Work")));
        assert!(labels.iter().any(|l| l.contains("delete Gaming")));
    }

    #[test]
    fn delete_dispatches_modal_open_not_direct_delete() {
        let mut state = boot_state();
        state.profiles.insert_or_replace(empty_profile("Work"));
        let cmds = build_commands(&state);

        let delete = cmds
            .iter()
            .find(|c| c.label == "profile: delete Work")
            .expect("delete command");
        assert!(matches!(
            delete.message,
            Message::OpenDeleteProfileModal(ref n) if n == "Work",
        ));
    }

    #[test]
    fn active_profile_marked_in_apply_label() {
        let mut state = boot_state();
        state.profiles.insert_or_replace(empty_profile("Work"));
        state.profiles.active = Some("Work".into());

        let cmds = build_commands(&state);
        assert!(
            cmds.iter()
                .any(|c| c.label.contains("apply") && c.label.contains("(active)")),
        );
    }

    #[test]
    fn make_default_only_offered_for_non_default_sinks() {
        let mut state = boot_state();
        let mut default = sink_stream(100, "alsa_output.usb");
        default.is_default = true;
        state.streams.insert(100, default);
        state
            .streams
            .insert(200, sink_stream(200, "bluez_output.headset"));

        let cmds = build_commands(&state);
        let make_default: Vec<&str> = cmds
            .iter()
            .filter(|c| c.label.starts_with("sink: make default"))
            .map(|c| c.label.as_str())
            .collect();
        assert_eq!(make_default.len(), 1);
        assert!(make_default[0].contains("sink-200"));
    }

    #[test]
    fn mute_label_flips_with_state() {
        let mut state = boot_state();
        let mut muted = app_stream(10);
        muted.muted = true;
        state.streams.insert(10, muted);
        state.streams.insert(11, app_stream(11));

        let cmds = build_commands(&state);
        assert!(cmds.iter().any(|c| c.label == "app: unmute app-10"));
        assert!(cmds.iter().any(|c| c.label == "app: mute app-11"));
    }

    #[test]
    fn no_pin_commands_with_a_single_sink() {
        let mut state = boot_state();
        state
            .streams
            .insert(100, sink_stream(100, "alsa_output.usb"));
        state.streams.insert(10, app_stream(10));
        let cmds = build_commands(&state);
        assert!(cmds.iter().all(|c| !c.label.starts_with("pin:")));
    }

    #[test]
    fn pin_commands_for_each_app_sink_pair() {
        let mut state = boot_state();
        state
            .streams
            .insert(100, sink_stream(100, "alsa_output.usb"));
        state
            .streams
            .insert(200, sink_stream(200, "bluez_output.headset"));
        state.streams.insert(10, app_stream(10));

        let cmds = build_commands(&state);
        let pins: Vec<&str> = cmds
            .iter()
            .filter(|c| c.label.starts_with("pin: app-10 "))
            .map(|c| c.label.as_str())
            .collect();
        assert_eq!(pins.len(), 2, "expected one pin per sink, got {pins:?}");
    }

    #[test]
    fn pin_to_already_pinned_sink_is_skipped() {
        let mut state = boot_state();
        state
            .streams
            .insert(100, sink_stream(100, "alsa_output.usb"));
        state
            .streams
            .insert(200, sink_stream(200, "bluez_output.headset"));
        let mut app = app_stream(10);
        app.target_sink_name = Some("alsa_output.usb".into());
        state.streams.insert(10, app);

        let cmds = build_commands(&state);
        let pins_to_usb: Vec<&str> = cmds
            .iter()
            .filter(|c| {
                c.label.starts_with("pin: app-10 \u{2192} ") && c.label.contains("sink-100")
            })
            .map(|c| c.label.as_str())
            .collect();
        assert!(
            pins_to_usb.is_empty(),
            "already-pinned route should not appear"
        );
        assert!(
            cmds.iter().any(|c| c.label == "pin: clear app-10"),
            "clear command should appear for pinned apps",
        );
        assert!(
            cmds.iter().any(|c| c.label == "pin: reset all"),
            "reset-all should appear whenever any app is pinned",
        );
    }

    #[test]
    fn reset_all_absent_with_no_pins() {
        let mut state = boot_state();
        state
            .streams
            .insert(100, sink_stream(100, "alsa_output.usb"));
        state
            .streams
            .insert(200, sink_stream(200, "bluez_output.headset"));
        state.streams.insert(10, app_stream(10));

        let cmds = build_commands(&state);
        assert!(cmds.iter().all(|c| c.label != "pin: reset all"));
    }

    fn cmd(label: &str) -> PaletteCommand {
        PaletteCommand {
            label: label.into(),
            message: Message::TogglePalette,
        }
    }

    #[test]
    fn filter_empty_query_returns_everything_in_order() {
        let cmds = vec![cmd("alpha"), cmd("beta"), cmd("gamma")];
        assert_eq!(filter_commands(&cmds, ""), vec![0, 1, 2]);
    }

    #[test]
    fn filter_prefers_word_prefix_over_substring_over_fuzzy() {
        let cmds = vec![
            cmd("sink: mute headphones"), // fuzzy
            cmd("set headphones"),        // prefix for "se h"
            cmd("the shuttle"),           // substring
        ];
        let out = filter_commands(&cmds, "se h");
        // Prefix tier must rank first.
        assert_eq!(out.first().copied(), Some(1));
    }

    #[test]
    fn filter_excludes_non_matches() {
        let cmds = vec![cmd("alpha bravo"), cmd("xyz")];
        let out = filter_commands(&cmds, "alpha");
        assert_eq!(out, vec![0]);
    }

    #[test]
    fn score_match_prefix_substring_fuzzy_none() {
        assert_eq!(score_match("sink mute usb", "si mu"), Some(0));
        assert_eq!(score_match("apply profile work", "ile w"), Some(1));
        assert_eq!(score_match("save current", "svcr"), Some(2));
        assert_eq!(score_match("apply profile", "zzz"), None);
    }

    fn source_stream(id: u32, node_name: &str) -> AudioStream {
        let mut s = sink_stream(id, node_name);
        s.kind = StreamKind::Source;
        s.name = format!("source-{id}");
        s
    }

    #[test]
    fn sources_get_make_default_and_mute_commands() {
        let mut state = boot_state();
        let mic = source_stream(300, "alsa_input.usb");
        state.streams.insert(mic.id, mic);

        let labels: Vec<String> = build_commands(&state)
            .into_iter()
            .map(|c| c.label)
            .collect();

        // A non-default source offers "make default"; every source offers mute.
        assert!(labels.iter().any(|l| l.starts_with("input: make default")));
        assert!(labels.iter().any(|l| l.starts_with("input: mute")));
    }

    #[test]
    fn default_source_has_no_make_default_command() {
        let mut state = boot_state();
        let mut mic = source_stream(300, "alsa_input.usb");
        mic.is_default = true;
        state.streams.insert(mic.id, mic);

        let labels: Vec<String> = build_commands(&state)
            .into_iter()
            .map(|c| c.label)
            .collect();

        assert!(!labels.iter().any(|l| l.starts_with("input: make default")));
        // The mute command is still offered for the default source.
        assert!(labels.iter().any(|l| l.starts_with("input: mute")));
    }
}
