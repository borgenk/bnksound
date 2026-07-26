//! The view snapshot: state projected into render-ready rows and overlays.
//!
//! Everything the renderer draws comes from here, owned (no borrow of state) so
//! layout and paint can run after the snapshot is built. It is rebuilt on state
//! change, not on every meter tick; meter ticks update retained meter values
//! and touch only the meter rectangles.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::command_palette;
use crate::domain::{
    DeviceForm, SectionFilter, Stream as AudioStream, StreamKind, linear_to_cubic,
};
use crate::state::{App, Message, Modal};
use crate::ui::layout::RowId;
use crate::ui::layout::metrics::VOLUME_WARNING_THRESHOLD;
use crate::view::app_group::{self, RenderedAppRow};

/// A sink or source column.
pub struct DeviceRowView {
    pub id: RowId,
    pub node_id: u32,
    pub label: String,
    pub sublabel: String,
    /// Slider position, cubic gain (0 = silent, 1 = unity).
    pub cubic: f32,
    pub muted: bool,
    pub is_default: bool,
    /// Cubic above the warning threshold; the fill turns to the warning color.
    pub warning: bool,
}

/// An application column: a collapsed group or an expanded member sub-row.
pub struct AppRowView {
    pub id: RowId,
    pub is_member: bool,
    pub label: String,
    /// The name the fallback icon tints and initials from.
    pub icon_key: String,
    /// The resolved freedesktop icon file, drawn when it decodes.
    pub icon_path: Option<PathBuf>,
    pub cubic: f32,
    pub muted: bool,
    pub tombstoned: bool,
    pub warning: bool,
    pub member_count: usize,
    pub is_expanded: bool,
    pub can_expand: bool,
    /// The sink this row is pinned to, or None for autoroute. A group resolves
    /// to Some only when every member shares one pin.
    pub target_sink: Option<u32>,
}

/// A sink the app target picker can pin to.
pub struct SinkOption {
    pub id: u32,
    pub label: String,
    /// One-character button label, from the device form ("HEADSET" -> 'H').
    pub short: char,
}

/// One row in the profile selector menu.
pub struct ProfileRowView {
    pub name: String,
    pub active: bool,
}

/// The profile selector: its rows and the active profile. Whether the menu is
/// dropped open is transient UI state, not part of the snapshot.
pub struct ProfileMenuView {
    pub rows: Vec<ProfileRowView>,
    pub active: Option<String>,
}

/// The command palette overlay.
pub struct PaletteView {
    pub open: bool,
    pub query: String,
    pub rows: Vec<String>,
    /// The message each visible row activates, parallel to `rows`.
    pub messages: Vec<Message>,
    pub selected: usize,
    /// First row the panel shows. Layout narrows the window to what fits and
    /// keeps the selection inside it.
    pub scroll: usize,
}

/// A create / rename / delete modal.
pub struct ModalView {
    pub title: String,
    pub body: String,
    pub input_visible: bool,
    pub input_value: String,
    pub error: Option<String>,
    pub confirm_label: String,
    pub destructive: bool,
}

/// The whole render-ready projection.
pub struct ViewSnapshot {
    pub sinks: Vec<DeviceRowView>,
    pub sources: Vec<DeviceRowView>,
    pub app_rows: Vec<AppRowView>,
    pub sink_options: Vec<SinkOption>,
    pub filter: SectionFilter,
    pub show_sinks: bool,
    pub show_sources: bool,
    pub show_apps: bool,
    pub profile: ProfileMenuView,
    pub palette: PaletteView,
    pub modal: Option<ModalView>,
    pub status: Option<String>,
    /// PipeWire node id to the rows its peak feeds (a device row, or an app
    /// group row plus its member sub-row when expanded).
    pub meter_routes: HashMap<u32, Vec<RowId>>,
}

/// Project state into a snapshot. `resolve_title` supplies MPRIS-enriched titles
/// for single-stream app rows (a no-op closure disables enrichment).
pub fn build_snapshot(state: &App, resolve_title: impl Fn(u32) -> Option<String>) -> ViewSnapshot {
    let mut sinks: Vec<&AudioStream> = Vec::new();
    let mut sources: Vec<&AudioStream> = Vec::new();
    let mut app_streams: Vec<&AudioStream> = Vec::new();
    for s in state.streams.values() {
        match s.kind {
            StreamKind::Sink => sinks.push(s),
            StreamKind::Source => sources.push(s),
            StreamKind::Application => app_streams.push(s),
        }
    }
    let device_sort =
        |s: &&AudioStream| (s.form.map(DeviceForm::sort_key).unwrap_or(u8::MAX), s.id);
    sinks.sort_by_key(device_sort);
    sources.sort_by_key(device_sort);

    // node.name -> sink id, to resolve an app's pinned target name to a sink.
    let sink_by_name: HashMap<&str, u32> = sinks
        .iter()
        .filter_map(|s| s.node_name.as_deref().map(|n| (n, s.id)))
        .collect();

    let mut meter_routes: HashMap<u32, Vec<RowId>> = HashMap::new();

    let sink_rows: Vec<DeviceRowView> = sinks
        .iter()
        .map(|s| {
            meter_routes.insert(s.id, vec![RowId::Sink(s.id)]);
            device_row(s, RowId::Sink(s.id))
        })
        .collect();
    let source_rows: Vec<DeviceRowView> = sources
        .iter()
        .map(|s| {
            meter_routes.insert(s.id, vec![RowId::Source(s.id)]);
            device_row(s, RowId::Source(s.id))
        })
        .collect();

    let sink_options: Vec<SinkOption> = sink_rows
        .iter()
        .map(|s| SinkOption {
            id: s.node_id,
            label: s.label.clone(),
            short: s
                .sublabel
                .chars()
                .chain(s.label.chars())
                .find(|c| c.is_alphanumeric())
                .unwrap_or('?'),
        })
        .collect();

    // App grouping and the peak routing plan.
    let groups = app_group::group_app_streams(&app_streams, &state.app_order);
    let (rendered, routes) = app_group::render_plan(&groups, &state.expanded_groups);
    for (node_id, route) in &routes {
        let mut rows = vec![RowId::AppGroup(route.group_key.to_string())];
        if route.has_member_row {
            rows.push(RowId::AppMember(*node_id));
        }
        meter_routes.insert(*node_id, rows);
    }

    let app_rows: Vec<AppRowView> = rendered
        .iter()
        .map(|row| app_row(row, state, &sink_by_name, &resolve_title))
        .collect();

    let show_sinks = state.shows_section(crate::domain::Section::Outputs) && !sink_rows.is_empty();
    let show_sources =
        state.shows_section(crate::domain::Section::Inputs) && !source_rows.is_empty();
    let show_apps = state.shows_section(crate::domain::Section::Apps) && !app_rows.is_empty();

    ViewSnapshot {
        sinks: sink_rows,
        sources: source_rows,
        app_rows,
        sink_options,
        filter: state.section_filter,
        show_sinks,
        show_sources,
        show_apps,
        profile: profile_menu(state),
        palette: palette(state),
        modal: modal(state),
        status: state.status.clone(),
        meter_routes,
    }
}

fn device_row(s: &AudioStream, id: RowId) -> DeviceRowView {
    let cubic = linear_to_cubic(s.average_volume());
    DeviceRowView {
        id,
        node_id: s.id,
        label: s.display_name().to_string(),
        sublabel: s
            .form
            .map(DeviceForm::display_label)
            .unwrap_or("")
            .to_string(),
        cubic,
        muted: s.muted,
        is_default: s.is_default,
        warning: cubic > VOLUME_WARNING_THRESHOLD,
    }
}

fn app_row(
    row: &RenderedAppRow,
    state: &App,
    sink_by_name: &HashMap<&str, u32>,
    resolve_title: &impl Fn(u32) -> Option<String>,
) -> AppRowView {
    match row {
        RenderedAppRow::Group { group, expanded } => {
            let info = group.to_info(&state.tombstoned, *expanded, resolve_title);
            AppRowView {
                id: RowId::AppGroup(info.key.to_string()),
                is_member: false,
                label: info.display_name,
                icon_key: group
                    .members
                    .iter()
                    .min_by_key(|s| s.id)
                    .map(|s| s.display_name().to_string())
                    .unwrap_or_default(),
                icon_path: info.xdg.and_then(|x| x.icon_path.clone()),
                cubic: info.master_cubic,
                muted: info.all_muted,
                tombstoned: info.all_tombstoned,
                warning: info.master_cubic > VOLUME_WARNING_THRESHOLD,
                member_count: info.member_count,
                is_expanded: info.is_expanded,
                can_expand: info.member_count > 1,
                target_sink: info
                    .effective_target
                    .and_then(|name| sink_by_name.get(name).copied()),
            }
        }
        RenderedAppRow::Member { stream, .. } => {
            let cubic = linear_to_cubic(stream.average_volume());
            AppRowView {
                id: RowId::AppMember(stream.id),
                is_member: true,
                label: member_label(stream),
                // Keyed on the app, not the stream: every member of a group
                // wears the same icon, so they share one cache entry.
                icon_key: stream.display_name().to_string(),
                icon_path: stream.xdg.as_ref().and_then(|x| x.icon_path.clone()),
                cubic,
                muted: stream.muted,
                tombstoned: state.tombstoned.contains(&stream.id),
                warning: cubic > VOLUME_WARNING_THRESHOLD,
                member_count: 1,
                is_expanded: false,
                can_expand: false,
                target_sink: stream
                    .target_sink_name
                    .as_deref()
                    .and_then(|name| sink_by_name.get(name).copied()),
            }
        }
    }
}

/// What a member sub-column is called. Every member shares the group's app
/// name, so that would label them all alike; media.name names the individual
/// stream where it says anything, and the node id does where it says nothing.
/// MPRIS is deliberately left out of it: a browser exposes one endpoint per
/// instance, so each tab would read as whatever is playing in one of them.
fn member_label(stream: &AudioStream) -> String {
    stream
        .media_name
        .as_deref()
        .filter(|n| !n.is_empty() && *n != "Playback")
        .map(str::to_string)
        .unwrap_or_else(|| format!("Stream {}", stream.id))
}

fn profile_menu(state: &App) -> ProfileMenuView {
    ProfileMenuView {
        rows: state
            .profiles
            .profiles
            .iter()
            .map(|p| ProfileRowView {
                name: p.name.clone(),
                active: state.profiles.active.as_deref() == Some(p.name.as_str()),
            })
            .collect(),
        active: state.profiles.active.clone(),
    }
}

fn palette(state: &App) -> PaletteView {
    let Some(palette) = &state.palette else {
        return PaletteView {
            open: false,
            query: String::new(),
            rows: Vec::new(),
            messages: Vec::new(),
            selected: 0,
            scroll: 0,
        };
    };
    let cmds = command_palette::build_commands(state);
    let visible: Vec<usize> = command_palette::filter_commands(&cmds, &palette.query)
        .into_iter()
        .take(command_palette::MAX_VISIBLE)
        .collect();
    let rows = visible.iter().map(|&i| cmds[i].label.clone()).collect();
    let messages = visible.iter().map(|&i| cmds[i].message.clone()).collect();
    let selected = palette.selected.min(visible.len().saturating_sub(1));
    let scroll = palette
        .scroll
        .min(visible.len().saturating_sub(command_palette::VISIBLE_ROWS));
    PaletteView {
        open: true,
        query: palette.query.clone(),
        rows,
        messages,
        selected,
        scroll,
    }
}

fn modal(state: &App) -> Option<ModalView> {
    let modal = state.modal.as_ref()?;
    Some(match modal {
        Modal::CreateProfile { name, error } => ModalView {
            title: "Create profile".to_string(),
            body: String::new(),
            input_visible: true,
            input_value: name.clone(),
            error: error.clone(),
            confirm_label: "Create".to_string(),
            destructive: false,
        },
        Modal::RenameProfile { name, error, .. } => ModalView {
            title: "Rename profile".to_string(),
            body: String::new(),
            input_visible: true,
            input_value: name.clone(),
            error: error.clone(),
            confirm_label: "Rename".to_string(),
            destructive: false,
        },
        Modal::DeleteProfile { name } => ModalView {
            title: "Delete profile".to_string(),
            body: format!("Delete the profile \u{201c}{name}\u{201d}?"),
            input_visible: false,
            input_value: String::new(),
            error: None,
            confirm_label: "Delete".to_string(),
            destructive: true,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DeviceForm, MAX_VOLUME, SinkForm, StreamKind};
    use crate::state;

    fn no_titles(_pid: u32) -> Option<String> {
        None
    }

    fn stream(id: u32, kind: StreamKind) -> AudioStream {
        AudioStream {
            id,
            kind,
            name: format!("node-{id}"),
            app_id: None,
            binary: None,
            pid: None,
            node_name: Some(format!("node.name.{id}")),
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

    fn app() -> App {
        state::empty()
    }

    #[test]
    fn empty_state_projects_nothing_visible() {
        let snap = build_snapshot(&app(), no_titles);
        assert!(snap.sinks.is_empty() && snap.sources.is_empty() && snap.app_rows.is_empty());
        assert!(!snap.show_sinks && !snap.show_sources && !snap.show_apps);
        assert!(snap.modal.is_none());
        assert!(!snap.palette.open);
    }

    #[test]
    fn one_of_each_kind_projects_rows_and_routes() {
        let mut a = app();
        let mut sink = stream(1, StreamKind::Sink);
        sink.form = Some(DeviceForm::Output(SinkForm::Speaker));
        sink.is_default = true;
        a.streams.insert(1, sink);
        a.streams.insert(2, stream(2, StreamKind::Source));
        let mut app_s = stream(3, StreamKind::Application);
        app_s.app_id = Some("com.example.App".into());
        a.streams.insert(3, app_s);

        let snap = build_snapshot(&a, no_titles);
        assert_eq!(snap.sinks.len(), 1);
        assert_eq!(snap.sources.len(), 1);
        assert_eq!(snap.app_rows.len(), 1);
        assert!(snap.sinks[0].is_default);
        assert_eq!(snap.sinks[0].id, RowId::Sink(1));
        // Meter routes: each node feeds its own row(s).
        assert_eq!(snap.meter_routes[&1], vec![RowId::Sink(1)]);
        assert_eq!(snap.meter_routes[&2], vec![RowId::Source(2)]);
        assert_eq!(
            snap.meter_routes[&3],
            vec![RowId::AppGroup("app:com.example.App".into())]
        );
        // One sink option, offered to the app target picker.
        assert_eq!(snap.sink_options.len(), 1);
    }

    #[test]
    fn warning_flag_trips_above_the_threshold() {
        let mut a = app();
        let mut loud = stream(1, StreamKind::Sink);
        loud.channel_volumes = vec![MAX_VOLUME, MAX_VOLUME]; // cubic > 1.10
        a.streams.insert(1, loud);
        let snap = build_snapshot(&a, no_titles);
        assert!(snap.sinks[0].warning);
    }

    /// Two streams from one browser, expanded into member sub-columns. Both
    /// carry the app's name, which is what the members must not be called.
    fn expanded_browser(media_names: [Option<&str>; 2]) -> ViewSnapshot {
        let mut a = app();
        for (i, media) in media_names.into_iter().enumerate() {
            let id = 85 + i as u32 * 12;
            let mut s = stream(id, StreamKind::Application);
            s.app_id = Some("net.helium.Browser".into());
            s.media_name = media.map(str::to_string);
            s.xdg = Some(crate::xdg::XdgInfo {
                name: "Helium Browser".into(),
                icon_path: None,
                desktop_path: std::path::PathBuf::from("/dev/null"),
            });
            a.streams.insert(id, s);
        }
        let key = crate::state::app_row_key(&a.streams[&85]);
        a.expanded_groups.insert(key);
        build_snapshot(&a, no_titles)
    }

    #[test]
    fn member_columns_are_named_apart_from_the_app_they_belong_to() {
        let snap = expanded_browser([None, None]);
        let members: Vec<&str> = snap
            .app_rows
            .iter()
            .filter(|r| r.is_member)
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(members, ["Stream 85", "Stream 97"]);

        let group = snap
            .app_rows
            .iter()
            .find(|r| !r.is_member)
            .expect("group row");
        assert_eq!(
            group.label, "Helium Browser",
            "the group keeps the app name"
        );
    }

    #[test]
    fn a_member_column_takes_the_stream_name_when_it_says_something() {
        let snap = expanded_browser([Some("Some Video"), Some("Playback")]);
        let members: Vec<&str> = snap
            .app_rows
            .iter()
            .filter(|r| r.is_member)
            .map(|r| r.label.as_str())
            .collect();
        // "Playback" is what PipeWire calls a stream that named nothing, so it
        // falls back the same as a missing name would.
        assert_eq!(members, ["Some Video", "Stream 97"]);
    }

    #[test]
    fn delete_modal_projects_a_confirming_body() {
        let mut a = app();
        a.modal = Some(Modal::DeleteProfile {
            name: "Gaming".into(),
        });
        let snap = build_snapshot(&a, no_titles);
        let modal = snap.modal.expect("modal");
        assert!(modal.destructive);
        assert!(!modal.input_visible);
        assert!(modal.body.contains("Gaming"));
    }
}
