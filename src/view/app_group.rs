//! App-stream grouping: collapse the streams of one process into a single
//! column, order the columns, and flatten each group into the rows the view
//! renders. Pure data transforms over [`crate::domain::Stream`]; no widgets,
//! so the aggregation rules can be unit-tested in isolation.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::domain::{Stream as AudioStream, linear_to_cubic};
use crate::state;
use crate::view::rows::AppRowInfo;

/// Precomputed meter routing for one app-stream node: which rows its peak
/// feeds. Built by render_plan so applying a peak is a pure lookup.
pub(crate) struct AppPeakRoute {
    pub(crate) group_key: Box<str>,
    /// Whether the node also has a sub-row of its own, which is true only while
    /// its group is expanded.
    pub(crate) has_member_row: bool,
}

/// One rendered app column: a collapsed group row or a per-member sub-row of
/// an expanded group. render_plan flattens groups into this sequence so the
/// diff/order code stays one path.
pub(crate) enum RenderedAppRow<'a> {
    /// Collapsed-aggregate row. expanded is true when sub-rows are also
    /// rendered; the picker's expand toggle reads it to flip its glyph.
    Group {
        group: &'a AppRowGroup<'a>,
        expanded: bool,
    },
    /// Sub-row for one stream in an expanded group.
    Member { stream: &'a AudioStream },
}

/// One collapsed app row: every stream sharing a [`state::app_row_key`] lands
/// in the same group.
pub(crate) struct AppRowGroup<'a> {
    pub(crate) key: String,
    pub(crate) members: Vec<&'a AudioStream>,
}

impl<'a> AppRowGroup<'a> {
    fn new(key: String) -> Self {
        Self {
            key,
            members: Vec::new(),
        }
    }

    /// Earliest first-seen seq among members, driving the column sort so the
    /// group stays put across pause/reconnect (the reconnect inherits the seq
    /// via the tombstone path). min anchors the slot to a stable member.
    fn sort_key(&self, app_order: &BTreeMap<u32, u64>) -> (u64, u32) {
        let min_seq = self
            .members
            .iter()
            .map(|s| app_order.get(&s.id).copied().unwrap_or(u64::MAX))
            .min()
            .unwrap_or(u64::MAX);
        let min_id = self.members.iter().map(|s| s.id).min().unwrap_or(u32::MAX);
        (min_seq, min_id)
    }

    /// Collapse member state into the aggregate handed to [`update_app_row`].
    /// Master cubic is max-of-members; all_muted requires every member muted;
    /// effective_target resolves only when every member shares one pin.
    /// Single-member groups get an MPRIS-enriched name via resolve_title,
    /// multi-member ones keep the app name (one title can't represent N tabs).
    pub(crate) fn to_info(
        &self,
        tombstoned: &HashSet<u32>,
        is_expanded: bool,
        resolve_title: impl Fn(&AudioStream) -> Option<String>,
    ) -> AppRowInfo<'_> {
        let master_cubic = self
            .members
            .iter()
            .map(|s| linear_to_cubic(s.average_volume()))
            .fold(0.0_f32, f32::max);
        let all_muted = !self.members.is_empty() && self.members.iter().all(|s| s.muted);
        let all_tombstoned =
            !self.members.is_empty() && self.members.iter().all(|s| tombstoned.contains(&s.id));
        // Common target across all members, if any.
        let mut targets = self.members.iter().map(|s| s.target_sink_name.as_deref());
        let effective_target = match targets.next() {
            Some(first) if targets.all(|t| t == first) => first,
            _ => None,
        };
        // Stable representative (lowest id) sources the name + xdg so the label
        // doesn't shuffle on registry reorders.
        let rep = self.members.iter().copied().min_by_key(|s| s.id);
        // MPRIS enrichment only for single-stream groups (a multi-tab group
        // would mislabel with one tab's title). The title replaces the app name
        // outright since the icon already identifies the app.
        let display_name = match rep {
            Some(rep) if self.members.len() == 1 => {
                resolve_title(rep).unwrap_or_else(|| rep.display_name().to_string())
            }
            Some(rep) => rep.display_name().to_string(),
            None => String::new(),
        };
        AppRowInfo {
            key: &self.key,
            display_name,
            icon_path: rep.and_then(|s| s.icon_path.as_deref()),
            master_cubic,
            all_muted,
            effective_target,
            all_tombstoned,
            member_count: self.members.len(),
            is_expanded,
        }
    }
}

/// Bucket application streams into columns keyed by [`state::app_row_key`],
/// then order the columns by earliest first-seen seq among members so the
/// strip stays put across pause/reconnect (seqless groups sink to the right).
pub(crate) fn group_app_streams<'a>(
    app_streams: &[&'a AudioStream],
    app_order: &BTreeMap<u32, u64>,
) -> Vec<AppRowGroup<'a>> {
    // BTreeMap keeps per-group member order deterministic before the sort.
    let mut group_map: BTreeMap<String, AppRowGroup<'a>> = BTreeMap::new();
    for s in app_streams.iter().copied() {
        let key = state::app_row_key(s);
        group_map
            .entry(key.clone())
            .or_insert_with(|| AppRowGroup::new(key))
            .members
            .push(s);
    }
    let mut groups: Vec<AppRowGroup<'a>> = group_map.into_values().collect();
    groups.sort_by_key(|g| g.sort_key(app_order));
    groups
}

/// Flatten ordered groups into the render sequence plus the per-node peak
/// routes. The group row is always emitted (the proportional master); member
/// sub-rows are added only when the group is expanded with more than one
/// stream.
pub(crate) fn render_plan<'a>(
    groups: &'a [AppRowGroup<'a>],
    expanded_groups: &HashSet<String>,
) -> (Vec<RenderedAppRow<'a>>, HashMap<u32, AppPeakRoute>) {
    let mut rendered: Vec<RenderedAppRow<'a>> = Vec::new();
    let mut routes: HashMap<u32, AppPeakRoute> = HashMap::new();
    for g in groups {
        let expanded = expanded_groups.contains(&g.key) && g.members.len() > 1;
        rendered.push(RenderedAppRow::Group { group: g, expanded });
        for s in &g.members {
            // Every member feeds the group row; the member sub-row only exists
            // while the group is expanded.
            routes.insert(
                s.id,
                AppPeakRoute {
                    group_key: g.key.as_str().into(),
                    has_member_row: expanded,
                },
            );
            if expanded {
                rendered.push(RenderedAppRow::Member { stream: s });
            }
        }
    }
    (rendered, routes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::StreamKind;

    fn app_stream(id: u32) -> AudioStream {
        AudioStream {
            name: format!("stream-{id}"),
            ..crate::domain::sample_stream(id, StreamKind::Application)
        }
    }

    fn no_titles(_stream: &AudioStream) -> Option<String> {
        None
    }

    #[test]
    fn groups_streams_by_identity_and_keeps_singletons_apart() {
        let mut a = app_stream(1);
        a.app_id = Some("com.spotify.Client".into());
        let mut b = app_stream(2);
        b.app_id = Some("com.spotify.Client".into());
        let c = app_stream(3); // no identity -> node:3

        let streams = [&a, &b, &c];
        let groups = group_app_streams(&streams, &BTreeMap::new());

        assert_eq!(groups.len(), 2);
        let spotify = groups
            .iter()
            .find(|g| g.key == "app:com.spotify.Client")
            .expect("spotify group");
        assert_eq!(spotify.members.len(), 2);
        assert!(groups.iter().any(|g| g.key == "node:3"));
    }

    #[test]
    fn columns_sort_by_earliest_member_seq() {
        // Two singleton groups; app_order assigns the later id the earlier seq,
        // so it must sort ahead of the lower-id group.
        let mut early = app_stream(10);
        early.app_id = Some("app:early".into());
        let mut late = app_stream(5);
        late.app_id = Some("app:late".into());

        let mut order = BTreeMap::new();
        order.insert(10, 1u64); // seen first
        order.insert(5, 2u64); // seen second

        let streams = [&late, &early];
        let groups = group_app_streams(&streams, &order);
        let keys: Vec<&str> = groups.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(keys, vec!["app:app:early", "app:app:late"]);
    }

    #[test]
    fn render_plan_emits_member_rows_only_when_expanded_multi_member() {
        let mut a = app_stream(1);
        a.app_id = Some("app:x".into());
        let mut b = app_stream(2);
        b.app_id = Some("app:x".into());
        let streams = [&a, &b];
        let groups = group_app_streams(&streams, &BTreeMap::new());
        let key = groups[0].key.clone();

        // Collapsed: one group row, no members.
        let (rows, routes) = render_plan(&groups, &HashSet::new());
        assert_eq!(rows.len(), 1);
        assert!(matches!(
            rows[0],
            RenderedAppRow::Group {
                expanded: false,
                ..
            }
        ));
        // Both nodes route to the group, no member sub-row.
        assert_eq!(routes[&1].group_key.as_ref(), key.as_str());
        assert!(!routes[&1].has_member_row);
        assert!(!routes[&2].has_member_row);

        // Expanded: group row + one member row per stream, in member order.
        let expanded: HashSet<String> = [key.clone()].into_iter().collect();
        let (rows, routes) = render_plan(&groups, &expanded);
        assert_eq!(rows.len(), 3);
        assert!(
            matches!(&rows[0], RenderedAppRow::Group { group, expanded: true } if group.key == key)
        );
        assert!(matches!(&rows[1], RenderedAppRow::Member { stream } if stream.id == 1));
        assert!(matches!(&rows[2], RenderedAppRow::Member { stream } if stream.id == 2));
        assert!(routes[&1].has_member_row);
        assert!(routes[&2].has_member_row);
    }

    #[test]
    fn render_plan_never_expands_single_member_group() {
        let mut a = app_stream(1);
        a.app_id = Some("app:solo".into());
        let streams = [&a];
        let groups = group_app_streams(&streams, &BTreeMap::new());
        // Even with the key marked expanded, a 1-member group stays collapsed.
        let expanded: HashSet<String> = [groups[0].key.clone()].into_iter().collect();
        let (rows, routes) = render_plan(&groups, &expanded);
        assert_eq!(rows.len(), 1);
        assert!(!routes[&1].has_member_row);
    }

    #[test]
    fn to_info_master_cubic_is_max_of_members() {
        let mut quiet = app_stream(1);
        quiet.app_id = Some("app:x".into());
        quiet.channel_volumes = vec![0.25, 0.25];
        let mut loud = app_stream(2);
        loud.app_id = Some("app:x".into());
        loud.channel_volumes = vec![0.75, 0.75];

        let streams = [&quiet, &loud];
        let groups = group_app_streams(&streams, &BTreeMap::new());
        let info = groups[0].to_info(&HashSet::new(), false, no_titles);
        assert_eq!(info.master_cubic, linear_to_cubic(0.75));
        assert_eq!(info.member_count, 2);
    }

    #[test]
    fn to_info_all_muted_requires_every_member() {
        let mut a = app_stream(1);
        a.app_id = Some("app:x".into());
        a.muted = true;
        let mut b = app_stream(2);
        b.app_id = Some("app:x".into());
        b.muted = false;

        let streams = [&a, &b];
        let groups = group_app_streams(&streams, &BTreeMap::new());
        assert!(
            !groups[0]
                .to_info(&HashSet::new(), false, no_titles)
                .all_muted
        );

        b.muted = true;
        let streams = [&a, &b];
        let groups = group_app_streams(&streams, &BTreeMap::new());
        assert!(
            groups[0]
                .to_info(&HashSet::new(), false, no_titles)
                .all_muted
        );
    }

    #[test]
    fn to_info_effective_target_only_when_members_share_a_pin() {
        let mut a = app_stream(1);
        a.app_id = Some("app:x".into());
        a.target_sink_name = Some("hdmi".into());
        let mut b = app_stream(2);
        b.app_id = Some("app:x".into());
        b.target_sink_name = Some("hdmi".into());

        let groups = group_app_streams(&[&a, &b], &BTreeMap::new());
        assert_eq!(
            groups[0]
                .to_info(&HashSet::new(), false, no_titles)
                .effective_target,
            Some("hdmi")
        );

        // Diverging pins collapse to None (autoroute stays active).
        b.target_sink_name = Some("usb".into());
        let groups = group_app_streams(&[&a, &b], &BTreeMap::new());
        assert_eq!(
            groups[0]
                .to_info(&HashSet::new(), false, no_titles)
                .effective_target,
            None
        );
    }

    #[test]
    fn to_info_enriches_single_member_with_resolved_title() {
        let mut solo = app_stream(1);
        solo.app_id = Some("app:player".into());
        solo.pid = Some("4242".into());

        let groups = group_app_streams(&[&solo], &BTreeMap::new());
        let info = groups[0].to_info(&HashSet::new(), false, |stream| {
            (stream.pid.as_deref() == Some("4242")).then(|| "Spotify · Track".to_string())
        });
        assert_eq!(info.display_name, "Spotify · Track");
    }

    #[test]
    fn to_info_ignores_title_for_multi_member_groups() {
        let mut a = app_stream(1);
        a.app_id = Some("app:browser".into());
        a.pid = Some("10".into());
        a.name = "Firefox".into();
        let mut b = app_stream(2);
        b.app_id = Some("app:browser".into());
        b.pid = Some("11".into());

        let groups = group_app_streams(&[&a, &b], &BTreeMap::new());
        // A title would mislabel a 2-tab group, so it's never consulted.
        let info = groups[0].to_info(&HashSet::new(), false, |_| Some("One Tab".to_string()));
        assert_eq!(info.display_name, "Firefox");
    }
}
