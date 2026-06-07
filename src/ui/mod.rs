use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;

use crate::bus::Sender;
use crate::command_palette::{self, MAX_VISIBLE};
use crate::domain::{DeviceForm, Section, Stream as AudioStream, StreamKind};
use crate::geometry::Geometry;
use crate::mpris::Mpris;
use crate::state::{App, Message, Modal};

mod app_group;
mod css;
pub(crate) mod meter;
mod panels;
mod row;
mod theme;

use app_group::{AppPeakRoute, RenderedAppRow};
use panels::{ModalPanel, PalettePanel, build_modal_panel, build_palette_panel, build_palette_row};
use row::{
    HoverTarget, RowWidgets, build_app_column, build_sink_column, build_source_column,
    make_col_separator, update_app_member_row, update_app_row, update_sink_row, update_source_row,
};

pub struct Widgets {
    pub window: gtk::ApplicationWindow,
    /// Titlebar button that opens the profile popover; label swapped to the
    /// active-profile name on refresh.
    profile_menu_btn: gtk::MenuButton,
    /// Row container inside the profile popover. Rebuilt per refresh; the
    /// separator and "+ New profile" footer are appended once and stay put.
    profile_popover_list: gtk::Box,
    sink_section: gtk::Box,
    sink_list: gtk::Box,
    /// Inputs|outputs separator. Shown only when both neighbours are visible.
    sep_before_outputs: gtk::Separator,
    source_section: gtk::Box,
    source_list: gtk::Box,
    /// Outputs|apps separator. Shown when apps is visible and at least one
    /// device section precedes it (so it still divides inputs|apps when
    /// outputs are hidden).
    sep_before_apps: gtk::Separator,
    app_section: gtk::Box,
    app_list: gtk::Box,
    status_label: gtk::Label,
    /// IN/OUT/APP filter buttons, painted active/inactive in `refresh_lists`.
    filter_buttons: Vec<(Section, gtk::Button)>,
    /// "M" mute-all button, painted active when every output sink is muted.
    mute_all_btn: gtk::Button,
    sink_rows: HashMap<u32, RowWidgets>,
    source_rows: HashMap<u32, RowWidgets>,
    // Keyed by app-group key (see `crate::state::app_row_key`) so
    // multiple streams from one process collapse into one row. Streams
    // without identifying props fall back to a synthetic `node:<id>` key.
    app_rows: HashMap<String, RowWidgets>,
    // Per-node meter routing, rebuilt each refresh by app_group::render_plan.
    // Precomputed so the meter tick never formats a key or scans for group
    // membership.
    app_peak_routes: HashMap<u32, AppPeakRoute>,
    /// Ctrl+K command palette overlay (widgets + render state).
    palette: PalettePanel,
    /// Profile-management modal overlay. Same overlay pattern as the palette:
    /// persistent widgets, entry text seeded only on the closed->open edge so
    /// typing doesn't reset the cursor.
    modal: ModalPanel,
    /// Column the pointer is over, or `None`. Written by per-column motion
    /// controllers, read by the key controller to route the `m` mute shortcut:
    /// sinks use per-stream `MuteToggled`, app groups use `GroupMuteToggled`.
    hovered_column: Rc<RefCell<Option<HoverTarget>>>,
    /// `%`-placement settings, captured at build (settings are read-only at
    /// runtime). Applied to rows in [`Self::apply_percent_visibility`].
    percent_above: bool,
    percent_on_slider: bool,
    /// Active colour palette, threaded into the per-row cairo draws (meter
    /// bars, unity notch) so they match the styled chrome.
    colors: theme::Palette,
    /// MPRIS metadata source for app-row title enrichment. Owned here so it
    /// lives as long as the UI; queried in `to_info`.
    mpris: Mpris,
}

impl Widgets {
    pub fn build(
        app: &gtk::Application,
        tx: Sender<Message>,
        geometry: Geometry,
        settings: &crate::settings::Settings,
        mpris: Mpris,
    ) -> Self {
        // Single source of truth shared by the CSS (via a generated
        // `@define-color` header) and the cairo-drawn meter / slider notch.
        let colors = theme::Palette::dark();
        css::install_css(&colors);

        // Seed the window with the last persisted size. On Wayland this is a
        // request the compositor may override.
        let width = i32::try_from(geometry.width).unwrap_or(i32::MAX);
        let height = i32::try_from(geometry.height).unwrap_or(i32::MAX);
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("BNK Sound")
            .default_width(width)
            .default_height(height)
            .build();
        if geometry.maximized {
            window.maximize();
        }
        if !settings.show_window_border {
            window.add_css_class("bnk-no-window-border");
        }

        // Capture the final session size once, on close. Watching
        // `notify::default-width` would also catch every compositor-driven
        // configure during startup, spamming the save with sizes the user
        // never picked.
        let geom_tx = tx.clone();
        window.connect_close_request(move |w| {
            let _ = geom_tx.send(Message::GeometryChanged {
                width: w.default_width().max(0) as u32,
                height: w.default_height().max(0) as u32,
                maximized: w.is_maximized(),
            });
            let _ = geom_tx.send(Message::AutoSaveTick);
            glib::Propagation::Proceed
        });

        let outer = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            // 2 px base inset so content never butts the bare window edge. The
            // top is bumped per titlebar mode below to clear the headerbar /
            // strip; the bottom stays 2 px (nothing sits below it).
            .margin_top(2)
            .margin_bottom(2)
            .margin_start(0)
            .margin_end(0)
            .build();

        let (profile_menu_btn, profile_popover_list) = build_profile_selector(&tx);

        let scrolled = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hexpand(true)
            .build();

        // Sinks and apps share one horizontal column strip, pavucontrol style,
        // each delimited by a vertical separator. 0 px spacing keeps the section
        // separator flush rather than opening a wider trench than the per-column ones.
        let scroll_inner = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(0)
            .halign(gtk::Align::Start)
            .hexpand(false)
            .hexpand_set(true)
            .build();

        // Inputs (capture devices): a vertical wrapper around a horizontal
        // strip of per-device columns. Renders first.
        let source_section = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .halign(gtk::Align::Start)
            .build();
        let source_list = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            // 0 inter-column spacing: each column carries its own right-side
            // padding and the separator widgets are the visible divider.
            // Anything > 0 widens the gap between a column and its separator.
            .spacing(0)
            .halign(gtk::Align::Start)
            .build();
        source_section.append(&source_list);

        let sep_before_outputs = make_col_separator();

        // Outputs (playback devices). Mirrors the source section.
        let sink_section = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .halign(gtk::Align::Start)
            .build();
        let sink_list = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            // See `source_list` for why this is 0.
            .spacing(0)
            .halign(gtk::Align::Start)
            .build();
        sink_section.append(&sink_list);

        let sep_before_apps = make_col_separator();

        let app_section = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .halign(gtk::Align::Start)
            .build();
        let app_list = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            // See `source_list` for why this is 0.
            .spacing(0)
            .halign(gtk::Align::Start)
            .build();
        app_section.append(&app_list);

        // Left-to-right: Inputs | Outputs | Apps, each preceded by the
        // separator that divides it from whatever section sits before it.
        scroll_inner.append(&source_section);
        scroll_inner.append(&sep_before_outputs);
        scroll_inner.append(&sink_section);
        scroll_inner.append(&sep_before_apps);
        scroll_inner.append(&app_section);
        scrolled.set_child(Some(&scroll_inner));

        let status_label = gtk::Label::builder().xalign(0.0).visible(false).build();
        status_label.add_css_class("bnk-subtle");

        // Two ways to host the profile selector at the top (see
        // `settings::TitlebarMode`): the HeaderBar (default, CSD) or an
        // in-window strip on an undecorated window (tiling / borderless).
        // `GTK_CSD=0` forces the strip regardless of the setting.
        let use_headerbar = settings.titlebar == crate::settings::TitlebarMode::HeaderBar
            && std::env::var("GTK_CSD").as_deref() != Ok("0");
        let titlebar_strip = if use_headerbar {
            let header = gtk::HeaderBar::new();
            header.pack_start(&profile_menu_btn);
            // Empty title widget suppresses the default title label; the window
            // title stays set so task switchers still get a readable name.
            header.set_title_widget(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));
            window.set_titlebar(Some(&header));
            // Top inset to clear the headerbar with a small gap.
            outer.set_margin_top(12);
            None
        } else {
            // Undecorated so GTK draws no titlebar of its own; the strip below
            // is the only top chrome.
            window.set_decorated(false);
            // Full-width strip carrying the profile selector, wrapped in a
            // WindowHandle so it still drags / double-click-maximizes on a
            // floating compositor (a no-op under a tiling WM). Borderless: no
            // min/max/close.
            let strip = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .build();
            strip.add_css_class("bnk-titlebar-strip");
            // Center vertically so the button doesn't stretch to the full bar
            // height. The HeaderBar path centers on its own.
            profile_menu_btn.set_valign(gtk::Align::Center);
            strip.append(&profile_menu_btn);
            let handle = gtk::WindowHandle::new();
            handle.set_child(Some(&strip));
            outer.set_margin_top(12);
            Some(handle)
        };
        outer.append(&scrolled);
        outer.append(&status_label);
        // The action bar takes a fixed left strip; `outer` takes the rest.
        outer.set_hexpand(true);

        // Left action bar: narrow vertical strip of shortcut buttons.
        let (action_bar, filter_buttons, mute_all_btn) = build_action_bar(&tx, settings);
        action_bar.set_visible(settings.show_sidebar);

        let content_root = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(0)
            .build();
        content_root.append(&action_bar);
        content_root.append(&outer);

        // In strip mode the profile bar stacks above the content row; in
        // headerbar mode the content row is the whole body.
        let body: gtk::Widget = match titlebar_strip {
            Some(strip) => {
                let stacked = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(0)
                    .build();
                stacked.append(&strip);
                stacked.append(&content_root);
                stacked.upcast()
            }
            None => content_root.upcast(),
        };

        // Overlay so the palette can float above the content.
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&body));

        let palette = build_palette_panel(&tx);
        palette.root.set_visible(false);
        overlay.add_overlay(&palette.root);

        let modal = build_modal_panel(&tx);
        modal.root.set_visible(false);
        overlay.add_overlay(&modal.root);

        window.set_child(Some(&overlay));

        let hovered_column: Rc<RefCell<Option<HoverTarget>>> = Rc::new(RefCell::new(None));

        install_key_controller(&window, &tx, &hovered_column, &palette.root, &modal.root);

        Widgets {
            window,
            profile_menu_btn,
            profile_popover_list,
            sink_section,
            sink_list,
            sep_before_outputs,
            source_section,
            source_list,
            sep_before_apps,
            app_section,
            app_list,
            status_label,
            filter_buttons,
            mute_all_btn,
            sink_rows: HashMap::new(),
            source_rows: HashMap::new(),
            app_rows: HashMap::new(),
            app_peak_routes: HashMap::new(),
            palette,
            modal,
            hovered_column,
            percent_above: settings.percent_above,
            percent_on_slider: settings.percent_on_slider,
            colors,
            mpris,
        }
    }

    pub fn refresh(&mut self, state: &App, tx: &Sender<Message>) {
        self.refresh_profile_menu(state, tx);
        self.refresh_lists(state, tx);
        // Bring rows freshly created in `refresh_lists` in line with the
        // configured `%` placement rather than defaulting to shown.
        self.apply_percent_visibility();
        self.refresh_status(state);
        self.refresh_palette(state);
        self.refresh_modal(state);
    }

    /// Apply the `%`-placement settings to every row (readout above the slider,
    /// number on the knob). A straight per-row visibility set.
    fn apply_percent_visibility(&self) {
        for row in self
            .sink_rows
            .values()
            .chain(self.source_rows.values())
            .chain(self.app_rows.values())
        {
            row.percent_box.set_visible(self.percent_above);
            row.knob_value.set_visible(self.percent_on_slider);
        }
    }

    fn refresh_palette(&mut self, state: &App) {
        if !state.palette_open {
            // Blank the entry so the search-changed signal doesn't loop us back
            // into Some(query).
            if self.palette.was_open {
                self.palette.entry.set_text("");
                // Clear focus before hiding the entry, else GTK4 drifts focus to
                // the first focusable widget and its focus ring fades in and out.
                gtk::prelude::GtkWindowExt::set_focus(&self.window, None::<&gtk::Widget>);
            }
            self.palette.root.set_visible(false);
            self.palette.was_open = false;
            return;
        }

        self.palette.root.set_visible(true);

        let cmds = command_palette::build_commands(state);
        let filtered = command_palette::filter_commands(&cmds, &state.palette_query);
        let visible: Vec<usize> = filtered.into_iter().take(MAX_VISIBLE).collect();
        let labels: Vec<String> = visible.iter().map(|&i| cmds[i].label.clone()).collect();

        // Rebuild rows only when the visible set changed (or on the open edge).
        // A selection-only change (Up/Down) keeps the laid-out rows so the
        // scroll-into-view below runs synchronously against a settled
        // adjustment. The list is tiny, so the rebuild is a wholesale recreate.
        let needs_rebuild = !self.palette.was_open || self.palette.rendered_labels != labels;
        if needs_rebuild {
            while let Some(child) = self.palette.list.first_child() {
                self.palette.list.remove(&child);
            }
            let mut messages = Vec::with_capacity(visible.len());
            for &cmd_idx in &visible {
                let cmd = &cmds[cmd_idx];
                let row = build_palette_row(&cmd.label, self.colors);
                self.palette.list.append(&row);
                messages.push(cmd.message.clone());
            }
            *self.palette.messages.borrow_mut() = messages;

            if visible.is_empty() {
                let empty = gtk::Label::new(Some("No matching commands"));
                empty.add_css_class("bnk-palette-empty");
                empty.set_xalign(0.0);
                self.palette.list.append(&empty);
            }
            self.palette.rendered_labels = labels;
        }

        if visible.is_empty() {
            self.palette.list.unselect_all();
        } else {
            let idx = state.palette_selected.min(visible.len() - 1);
            if let Some(row) = self.palette.list.row_at_index(idx as i32) {
                self.palette.list.select_row(Some(&row));
                if needs_rebuild {
                    // Fresh rows aren't laid out yet; defer until they are. If a
                    // newer refresh dropped the row, compute_bounds returns None
                    // and this is a no-op.
                    let scrolled = self.palette.scroll.clone();
                    let list = self.palette.list.clone();
                    glib::idle_add_local_once(move || {
                        scroll_row_into_view(&scrolled, &list, &row);
                    });
                } else {
                    // Rows already laid out: scroll now against a settled adjustment.
                    scroll_row_into_view(&self.palette.scroll, &self.palette.list, &row);
                }
            }
        }

        // Seed the entry and grab focus only on the closed->open edge so later
        // keystrokes don't tug the cursor.
        if !self.palette.was_open {
            self.palette.entry.set_text(&state.palette_query);
            self.palette.entry.grab_focus();
        }
        self.palette.was_open = true;
    }

    /// Sync the profile-management modal to `state.modal`. Widgets are
    /// persistent so the entry keeps focus across the refresh roundtrip; entry
    /// text and focus are seeded only on the closed->open edge.
    fn refresh_modal(&mut self, state: &App) {
        let Some(modal) = state.modal.as_ref() else {
            if self.modal.was_open {
                // Drop focus so GTK doesn't drift it once we hide the panel.
                gtk::prelude::GtkWindowExt::set_focus(&self.window, None::<&gtk::Widget>);
            }
            self.modal.root.set_visible(false);
            self.modal.was_open = false;
            return;
        };

        self.modal.root.set_visible(true);

        let (title, body, entry_visible, seed_text, confirm_label, destructive, error) = match modal
        {
            Modal::CreateProfile { name, error } => (
                "Create profile".to_string(),
                String::new(),
                true,
                name.clone(),
                "Create".to_string(),
                false,
                error.clone(),
            ),
            Modal::RenameProfile {
                old_name,
                name,
                error,
            } => (
                format!("Rename profile '{old_name}'"),
                String::new(),
                true,
                name.clone(),
                "Rename".to_string(),
                false,
                error.clone(),
            ),
            Modal::DeleteProfile { name } => (
                format!("Delete profile '{name}'?"),
                "This removes the saved profile. The current live audio settings are not touched."
                    .to_string(),
                false,
                String::new(),
                "Delete".to_string(),
                true,
                None,
            ),
        };

        self.modal.title.set_text(&title);
        self.modal.body.set_text(&body);
        self.modal.body.set_visible(!body.is_empty());
        self.modal.entry.set_visible(entry_visible);
        self.modal.confirm.set_label(&confirm_label);
        set_swap_class(
            &self.modal.confirm,
            destructive,
            "destructive-action",
            "suggested-action",
        );
        match error.as_deref() {
            Some(text) => {
                self.modal.error.set_text(text);
                self.modal.error.set_visible(true);
            }
            None => {
                self.modal.error.set_text("");
                self.modal.error.set_visible(false);
            }
        }

        if !self.modal.was_open {
            if entry_visible {
                self.modal.entry.set_text(&seed_text);
                // Cursor at the end so a pre-filled rename name is edited, not
                // overwritten.
                self.modal.entry.set_position(-1);
                self.modal.entry.grab_focus();
            } else {
                self.modal.confirm.grab_focus();
            }
        }
        self.modal.was_open = true;
    }

    fn refresh_profile_menu(&mut self, state: &App, tx: &Sender<Message>) {
        let label = state.profiles.active.as_deref().unwrap_or("Profiles");
        self.profile_menu_btn.set_label(label);

        // Rebuild the popover row list; the separator + footer stay put.
        while let Some(child) = self.profile_popover_list.first_child() {
            self.profile_popover_list.remove(&child);
        }

        for profile in &state.profiles.profiles {
            let active = state.profiles.active.as_deref() == Some(profile.name.as_str());
            let row = gtk::Button::with_label(&profile.name);
            row.add_css_class("bnk-profile-row");
            if active {
                row.add_css_class("bnk-profile-row-active");
            }
            // Left-align so the rows read as a list, not centered captions.
            if let Some(lbl) = row.child().and_then(|c| c.downcast::<gtk::Label>().ok()) {
                lbl.set_xalign(0.0);
            }
            row.set_hexpand(true);
            let tx_apply = tx.clone();
            let name_apply = profile.name.clone();
            let menu_btn = self.profile_menu_btn.clone();
            row.connect_clicked(move |_| {
                // Dismiss the menu to release the popover's input grab.
                menu_btn.popdown();
                let _ = tx_apply.send(Message::ApplyProfile(name_apply.clone()));
            });

            // Drag source (payload: profile name). GTK negotiates click vs
            // drag; drag only takes over after the built-in threshold.
            let drag = gtk::DragSource::builder()
                .actions(gdk::DragAction::MOVE)
                .build();
            let name_drag = profile.name.clone();
            drag.connect_prepare(move |_, _, _| {
                Some(gdk::ContentProvider::for_value(&name_drag.to_value()))
            });
            row.add_controller(drag);

            // Drop target: split by y (top half = before, bottom half = after).
            let drop_target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
            let target_name = profile.name.clone();
            let tx_drop = tx.clone();
            let row_for_drop = row.clone();
            drop_target.connect_drop(move |_, value, _, y| {
                let Ok(src_name) = value.get::<String>() else {
                    return false;
                };
                let height = row_for_drop.height() as f64;
                let before = y < height / 2.0;
                let _ = tx_drop.send(Message::ReorderProfile {
                    name: src_name,
                    target: target_name.clone(),
                    before,
                });
                true
            });
            row.add_controller(drop_target);

            self.profile_popover_list.append(&row);
        }
    }

    fn refresh_lists(&mut self, state: &App, tx: &Sender<Message>) {
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
        // Sort by (form, id) so the strip order is stable across registry churn.
        sinks.sort_by_key(|s| (s.form.map(DeviceForm::sort_key).unwrap_or(u8::MAX), s.id));
        sources.sort_by_key(|s| (s.form.map(DeviceForm::sort_key).unwrap_or(u8::MAX), s.id));

        let groups = app_group::group_app_streams(&app_streams, &state.app_order);
        let (rendered, routes) = app_group::render_plan(&groups, &state.expanded_groups);
        self.app_peak_routes = routes;

        // Outputs and inputs share one diff/reorder routine, differing only in
        // their build/update fns. Surviving rows are NOT unparented (an in-flight
        // slider drag lives on the widget); only dead rows are dropped.
        sync_device_section(
            &self.sink_list,
            &mut self.sink_rows,
            &sinks,
            tx,
            &self.hovered_column,
            build_sink_column,
            update_sink_row,
            self.colors,
        );
        sync_device_section(
            &self.source_list,
            &mut self.source_rows,
            &sources,
            tx,
            &self.hovered_column,
            build_source_column,
            update_source_row,
            self.colors,
        );

        self.sync_app_section(&rendered, &sinks, state, tx);

        // A section shows when its filter is on AND it has content. The outputs
        // separator needs inputs+outputs both shown; the apps separator needs
        // apps shown plus any device section before it.
        let show_out = state.shows_section(Section::Outputs) && !sinks.is_empty();
        let show_in = state.shows_section(Section::Inputs) && !sources.is_empty();
        let show_app = state.shows_section(Section::Apps) && !rendered.is_empty();
        self.source_section.set_visible(show_in);
        self.sink_section.set_visible(show_out);
        self.app_section.set_visible(show_app);
        self.sep_before_outputs.set_visible(show_in && show_out);
        self.sep_before_apps
            .set_visible(show_app && (show_in || show_out));

        // Paint each filter button lit when its section is enabled (regardless
        // of content).
        for (section, btn) in &self.filter_buttons {
            set_swap_class(
                btn,
                state.shows_section(*section),
                "bnk-active",
                "bnk-inactive",
            );
        }

        // Mute-all lights up once every output is muted, using the brand-pink
        // `.bnk-pick-active` style (distinct from the yellow filter toggles).
        if state.all_outputs_muted() {
            self.mute_all_btn.add_css_class("bnk-pick-active");
        } else {
            self.mute_all_btn.remove_css_class("bnk-pick-active");
        }
    }

    /// Diff the app column strip against the render plan: drop dead rows,
    /// append new ones, reorder, refresh each from its group/member state, and
    /// hide the trailing separator. Surviving rows are NOT unparented, so an
    /// in-flight slider drag survives a worker echo.
    fn sync_app_section(
        &mut self,
        rendered: &[(String, RenderedAppRow<'_>)],
        sinks: &[&AudioStream],
        state: &App,
        tx: &Sender<Message>,
    ) {
        let live_keys: std::collections::HashSet<&str> =
            rendered.iter().map(|(key, _)| key.as_str()).collect();

        let dead_app_keys: Vec<String> = self
            .app_rows
            .keys()
            .filter(|k| !live_keys.contains(k.as_str()))
            .cloned()
            .collect();
        for key in dead_app_keys {
            if let Some(row) = self.app_rows.remove(&key) {
                self.app_list.remove(&row.container);
                self.app_list.remove(&row.separator);
            }
        }

        // Append any newly-live app rows; the separator follows the container
        // so the visual order is [row, line, row, line, ...].
        for (row_key, r) in rendered {
            if self.app_rows.contains_key(row_key) {
                continue;
            }
            let row = match r {
                RenderedAppRow::Group { group, .. } => {
                    let key = group.key.clone();
                    let key_for_mute = key.clone();
                    let key_for_vol = key.clone();
                    build_app_column(
                        tx,
                        &self.hovered_column,
                        HoverTarget::AppGroup(key.clone()),
                        false,
                        move || Message::GroupMuteToggled(key_for_mute.clone()),
                        move |v: f32| Message::GroupVolumeChanged {
                            key: key_for_vol.clone(),
                            cubic: v,
                        },
                        self.colors,
                    )
                }
                RenderedAppRow::Member { stream, .. } => {
                    let id = stream.id;
                    // Sub-rows use per-stream volume/mute messages. Hover target
                    // is `Sink` because the `m` shortcut's payload (single
                    // node-id mute) is identical to a sink's.
                    build_app_column(
                        tx,
                        &self.hovered_column,
                        HoverTarget::Sink(id),
                        true,
                        move || Message::MuteToggled(id),
                        move |v: f32| Message::VolumeChanged(id, v),
                        self.colors,
                    )
                }
            };
            self.app_list.append(&row.container);
            self.app_list.append(&row.separator);
            self.app_rows.insert(row_key.clone(), row);
        }

        // Enforce order via reorder_child_after, which does NOT unparent. Each
        // separator is pinned immediately after its row's container.
        let mpris = &self.mpris;
        let mut prev: Option<gtk::Separator> = None;
        let last_key: Option<&str> = rendered.last().map(|(key, _)| key.as_str());
        for (row_key, r) in rendered {
            let row = self.app_rows.get_mut(row_key).expect("just inserted");
            self.app_list
                .reorder_child_after(&row.container, prev.as_ref());
            self.app_list
                .reorder_child_after(&row.separator, Some(&row.container));
            match r {
                RenderedAppRow::Group { group, expanded } => {
                    let info =
                        group.to_info(&state.tombstoned, *expanded, |pid| mpris.resolve_title(pid));
                    update_app_row(row, info, sinks, tx);
                }
                RenderedAppRow::Member {
                    parent_key,
                    parent_xdg,
                    stream,
                } => {
                    let tombstoned = state.tombstoned.contains(&stream.id);
                    update_app_member_row(
                        row,
                        stream,
                        parent_key,
                        *parent_xdg,
                        tombstoned,
                        sinks,
                        tx,
                    );
                }
            }
            prev = Some(row.separator.clone());
        }
        if let Some(last) = last_key {
            for (row_key, _) in rendered {
                if let Some(row) = self.app_rows.get(row_key) {
                    row.separator.set_visible(row_key.as_str() != last);
                }
            }
        }
    }

    fn refresh_status(&mut self, state: &App) {
        match &state.status {
            Some(msg) => {
                self.status_label.set_text(msg);
                self.status_label.set_visible(true);
            }
            None => self.status_label.set_visible(false),
        }
    }
}

/// Builds a fresh device column. `build_sink_column` / `build_source_column`
/// match this shape so `sync_device_section` can take either.
type BuildRowFn =
    fn(u32, &Sender<Message>, &Rc<RefCell<Option<HoverTarget>>>, theme::Palette) -> RowWidgets;
/// Refreshes an existing device row from its stream. `update_sink_row` /
/// `update_source_row` match this shape.
type UpdateRowFn = fn(&mut RowWidgets, &AudioStream);

/// Diff one device section (outputs or inputs) against the live list: drop
/// dead rows, append new ones, reorder, hide the trailing separator. Surviving
/// rows are NOT unparented, so an in-flight slider drag survives a worker echo.
// Eight args that don't cohere into a struct; a context type would force an
// HRTB fn pointer onto `BuildRowFn`.
#[allow(clippy::too_many_arguments)]
fn sync_device_section(
    list: &gtk::Box,
    rows: &mut HashMap<u32, RowWidgets>,
    devices: &[&AudioStream],
    tx: &Sender<Message>,
    hovered: &Rc<RefCell<Option<HoverTarget>>>,
    build: BuildRowFn,
    update: UpdateRowFn,
    palette: theme::Palette,
) {
    let live: std::collections::HashSet<u32> = devices.iter().map(|s| s.id).collect();
    let dead: Vec<u32> = rows
        .keys()
        .copied()
        .filter(|id| !live.contains(id))
        .collect();
    for id in dead {
        if let Some(row) = rows.remove(&id) {
            list.remove(&row.container);
            list.remove(&row.separator);
        }
    }
    for s in devices {
        rows.entry(s.id).or_insert_with(|| {
            let row = build(s.id, tx, hovered, palette);
            list.append(&row.container);
            list.append(&row.separator);
            row
        });
    }
    let mut prev: Option<gtk::Separator> = None;
    for s in devices {
        let row = rows.get_mut(&s.id).expect("just inserted");
        list.reorder_child_after(&row.container, prev.as_ref());
        list.reorder_child_after(&row.separator, Some(&row.container));
        update(row, s);
        prev = Some(row.separator.clone());
    }
    // Hide the trailing separator so the strip doesn't end on a dangling line.
    if let Some(last) = devices.last() {
        for s in devices {
            if let Some(row) = rows.get(&s.id) {
                row.separator.set_visible(s.id != last.id);
            }
        }
    }
}

/// Scroll the minimum distance so `row` is fully visible: top-align when it's
/// above the viewport, bottom-align when below, leave it when in view. Bounds
/// are in `list`'s coordinate space, the space the vadjustment indexes into.
fn scroll_row_into_view(
    scrolled: &gtk::ScrolledWindow,
    list: &gtk::ListBox,
    row: &gtk::ListBoxRow,
) {
    let Some(bounds) = row.compute_bounds(list) else {
        return;
    };
    let adj = scrolled.vadjustment();
    let row_top = f64::from(bounds.y());
    let row_bottom = row_top + f64::from(bounds.height());
    let view_top = adj.value();
    let view_bottom = view_top + adj.page_size();
    if row_top < view_top {
        adj.set_value(row_top);
    } else if row_bottom > view_bottom {
        adj.set_value(row_bottom - adj.page_size());
    }
}

/// Build the profile dropdown: a MenuButton whose label tracks the active
/// profile, opening a popover with a per-profile row list and a "+ New profile"
/// footer. Returns the button plus the popover's row list, which the refresh
/// rebuilds while the separator and footer stay put.
fn build_profile_selector(tx: &Sender<Message>) -> (gtk::MenuButton, gtk::Box) {
    let profile_popover_list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(0)
        .build();

    let profile_popover_sep = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .build();
    profile_popover_sep.add_css_class("bnk-profile-popover-sep");

    let profile_popover_new = gtk::Button::with_label("+ New profile");
    profile_popover_new.add_css_class("bnk-profile-row");
    profile_popover_new.add_css_class("bnk-profile-row-new");

    let profile_popover_root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(0)
        .build();
    profile_popover_root.append(&profile_popover_list);
    profile_popover_root.append(&profile_popover_sep);
    profile_popover_root.append(&profile_popover_new);

    let profile_popover = gtk::Popover::builder()
        .has_arrow(false)
        .position(gtk::PositionType::Bottom)
        .child(&profile_popover_root)
        .build();
    profile_popover.add_css_class("bnk-profile-popover");

    // Close the menu before opening the create-profile modal: an open
    // autohide popover keeps its input grab, so the modal's grab_focus
    // would be ignored.
    {
        let tx = tx.clone();
        let popover = profile_popover.clone();
        profile_popover_new.connect_clicked(move |_| {
            popover.popdown();
            let _ = tx.send(Message::OpenCreateProfileModal);
        });
    }

    let profile_menu_btn = gtk::MenuButton::builder()
        .popover(&profile_popover)
        .label("Profiles")
        .build();
    profile_menu_btn.add_css_class("bnk-profile-menu-btn");

    // Left-align the popover with the button's start edge (GTK centers a
    // Bottom popover by default) and nudge it down a few px. measure()
    // gives natural sizes pre-display so the offset is in place before show.
    const POPOVER_GAP_Y: i32 = 4;
    let popover_for_align = profile_popover.clone();
    let btn_for_align = profile_menu_btn.clone();
    profile_popover.connect_show(move |_| {
        let (_, btn_w, _, _) = btn_for_align.measure(gtk::Orientation::Horizontal, -1);
        let (_, pop_w, _, _) = popover_for_align.measure(gtk::Orientation::Horizontal, -1);
        let offset_x = (pop_w - btn_w) / 2;
        popover_for_align.set_offset(offset_x, POPOVER_GAP_Y);
    });

    // Clicking the MenuButton while the popover is open closes it via
    // autohide, then the same click hits the toggle handler and reopens
    // it (net: a no-op). On close, raise a one-shot suppress flag for a
    // short window; a capture-phase click controller swallows clicks during
    // it. 30 ms is short enough not to block a deliberate close-then-reopen.
    let suppress_next_click = Rc::new(Cell::new(false));
    let suppress_for_close = suppress_next_click.clone();
    profile_popover.connect_closed(move |_| {
        suppress_for_close.set(true);
        let suppress_for_clear = suppress_for_close.clone();
        glib::timeout_add_local_once(Duration::from_millis(30), move || {
            suppress_for_clear.set(false);
        });
    });
    let click = gtk::GestureClick::new();
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    let suppress_for_click = suppress_next_click.clone();
    click.connect_pressed(move |g, _, _, _| {
        if suppress_for_click.get() {
            suppress_for_click.set(false);
            g.set_state(gtk::EventSequenceState::Claimed);
        }
    });
    profile_menu_btn.add_controller(click);

    (profile_menu_btn, profile_popover_list)
}

/// Install the window-level key controller (capture phase so a focused entry
/// can't swallow the shortcuts): Ctrl+K toggles the palette, Ctrl+Shift+S saves
/// a PNG, and a bare m mutes the hovered column (gated on no overlay being open
/// so typing m into an entry doesn't mute).
fn install_key_controller(
    window: &gtk::ApplicationWindow,
    tx: &Sender<Message>,
    hovered: &Rc<RefCell<Option<HoverTarget>>>,
    palette_root: &gtk::Box,
    modal_root: &gtk::Box,
) {
    let key_ctrl = gtk::EventControllerKey::new();
    key_ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
    let tx_keys = tx.clone();
    let hov_for_key = hovered.clone();
    let palette_for_key = palette_root.clone();
    let modal_for_key = modal_root.clone();
    let window_for_key = window.clone();
    key_ctrl.connect_key_pressed(move |_, key, _, mods| {
        if mods.contains(gtk::gdk::ModifierType::CONTROL_MASK) && key == gtk::gdk::Key::k {
            let _ = tx_keys.send(Message::TogglePalette);
            return glib::Propagation::Stop;
        }
        if mods.contains(gtk::gdk::ModifierType::CONTROL_MASK)
            && mods.contains(gtk::gdk::ModifierType::SHIFT_MASK)
            && matches!(key, gtk::gdk::Key::s | gtk::gdk::Key::S)
        {
            crate::screenshot::capture_window(&window_for_key);
            return glib::Propagation::Stop;
        }
        // Bare 'm' (no modifiers) on a hovered column toggles mute; filter
        // out modifier combos so Ctrl+M etc stay free and never double-fire.
        let bare = !mods.intersects(
            gtk::gdk::ModifierType::CONTROL_MASK
                | gtk::gdk::ModifierType::ALT_MASK
                | gtk::gdk::ModifierType::SUPER_MASK
                | gtk::gdk::ModifierType::SHIFT_MASK,
        );
        if bare
            && key == gtk::gdk::Key::m
            && !palette_for_key.is_visible()
            && !modal_for_key.is_visible()
        {
            // Clone out of the RefCell so the borrow isn't held across send().
            let target = hov_for_key.borrow().clone();
            if let Some(target) = target {
                let msg = match target {
                    HoverTarget::Sink(id) => Message::MuteToggled(id),
                    HoverTarget::AppGroup(key) => Message::GroupMuteToggled(key),
                };
                let _ = tx_keys.send(msg);
                return glib::Propagation::Stop;
            }
        }
        glib::Propagation::Proceed
    });
    window.add_controller(key_ctrl);
}

/// Build the left action bar: stacked `.bnk-pick` letter buttons (IN/OUT/APP
/// section filters, M mute-all, R reset-all). Per-button visibility follows
/// `settings`. Returns the bar plus the shown filter buttons (paired with their
/// `Section`) and the mute-all button so `refresh_lists` can sync their active
/// state. Mute-all is always constructed (handle must be valid) but only added
/// when `show_mute_button` is set.
fn build_action_bar(
    tx: &Sender<Message>,
    settings: &crate::settings::Settings,
) -> (gtk::Box, Vec<(Section, gtk::Button)>, gtk::Button) {
    let bar = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .width_request(48)
        .build();
    bar.add_css_class("bnk-action-bar");

    // Uniform width so the strip reads as one aligned column; the widest label
    // (3 chars) drives the floor.
    const ACTION_BTN_WIDTH: i32 = 32;

    // Builder for one action-bar pick: centered label, `.bnk-pick` style,
    // uniform width, click that dispatches `msg`.
    let make_pick = |label: &str, msg: Message| {
        let lbl = gtk::Label::new(Some(label));
        let btn = gtk::Button::builder()
            .child(&lbl)
            .halign(gtk::Align::Center)
            .build();
        btn.add_css_class("bnk-pick");
        btn.set_size_request(ACTION_BTN_WIDTH, -1);
        let tx_btn = tx.clone();
        btn.connect_clicked(move |_| {
            let _ = tx_btn.send(msg.clone());
        });
        btn
    };

    let mut filter_buttons: Vec<(Section, gtk::Button)> = Vec::with_capacity(3);
    for (label, section, enabled) in [
        ("IN", Section::Inputs, settings.show_input_button),
        ("OUT", Section::Outputs, settings.show_output_button),
        ("APP", Section::Apps, settings.show_apps_button),
    ] {
        if !enabled {
            continue;
        }
        let btn = make_pick(label, Message::ToggleSection(section));
        bar.append(&btn);
        filter_buttons.push((section, btn));
    }

    // Separator between the filter and action clusters, only when both have a
    // visible button.
    let any_action = settings.show_mute_button || settings.show_reset_button;
    if !filter_buttons.is_empty() && any_action {
        bar.append(&gtk::Box::builder().height_request(8).build());
    }

    // Mute-all: toggles mute on every output sink. Always built (handle must
    // be valid), appended only when enabled.
    let mute_all_btn = make_pick("M", Message::MuteAllToggled);
    if settings.show_mute_button {
        bar.append(&mute_all_btn);
    }

    // Reset-all: clears every per-app routing pin. A no-op when nothing is pinned.
    if settings.show_reset_button {
        let reset_btn = make_pick("R", Message::ResetAllStreamTargets);
        bar.append(&reset_btn);
    }

    (bar, filter_buttons, mute_all_btn)
}

/// Toggle a widget between two mutually-exclusive CSS classes: `when_true` when
/// `on`, `when_false` otherwise.
fn set_swap_class<W: IsA<gtk::Widget>>(widget: &W, on: bool, when_true: &str, when_false: &str) {
    if on {
        widget.add_css_class(when_true);
        widget.remove_css_class(when_false);
    } else {
        widget.add_css_class(when_false);
        widget.remove_css_class(when_true);
    }
}
