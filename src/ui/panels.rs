use std::cell::RefCell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;

use crate::bus::Sender;
use crate::state::Message;
use crate::ui::theme::Palette;

/// The Ctrl+K command palette overlay: the widgets the refresh drives plus the
/// small bit of frame-to-frame state it tracks.
pub(super) struct PalettePanel {
    pub(super) root: gtk::Box,
    pub(super) entry: gtk::Entry,
    pub(super) list: gtk::ListBox,
    /// List scroller, kept so keyboard nav can scroll the selected row into view.
    pub(super) scroll: gtk::ScrolledWindow,
    /// Labels currently rendered. refresh_palette rebuilds rows only when this
    /// changes; a selection-only change (Up/Down) reuses the laid-out rows so
    /// scroll-into-view runs synchronously instead of racing a relayout.
    pub(super) rendered_labels: Vec<String>,
    /// Parallel-to-row vector: row.index() indexes the Message to dispatch on
    /// activation. Shared with the row/enter closures.
    pub(super) messages: Rc<RefCell<Vec<Message>>>,
    /// Previous frame's open state, so we grab focus only on the closed->open edge.
    pub(super) was_open: bool,
}

/// The profile-management modal overlay: persistent widgets plus the open-edge
/// flag the refresh uses to seed entry text once per opening.
pub(super) struct ModalPanel {
    pub(super) root: gtk::Box,
    pub(super) title: gtk::Label,
    pub(super) body: gtk::Label,
    pub(super) entry: gtk::Entry,
    pub(super) error: gtk::Label,
    pub(super) confirm: gtk::Button,
    pub(super) was_open: bool,
}

/// Construct the dim-backdrop + centered panel + entry + scrolled list,
/// wire up search/Esc/Enter/Up/Down/click, and return the parts the
/// caller needs to drive on every refresh.
pub(super) fn build_palette_panel(tx: &Sender<Message>) -> PalettePanel {
    let messages: Rc<RefCell<Vec<Message>>> = Rc::new(RefCell::new(Vec::new()));
    // Dim backdrop that fills the overlay. A press only closes the palette
    // when it lands on the backdrop itself, not on the panel's widgets.
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .vexpand(true)
        .halign(gtk::Align::Fill)
        .valign(gtk::Align::Fill)
        .build();
    root.add_css_class("bnk-palette-backdrop");

    // Pick the widget under the pointer instead of a "swallow" gesture on
    // the panel: that would cancel the rows' click gestures (resolved on
    // release), making rows unclickable.
    let click = gtk::GestureClick::new();
    let tx_bd = tx.clone();
    let root_for_pick = root.clone();
    click.connect_pressed(move |_, _, x, y| {
        if root_for_pick
            .pick(x, y, gtk::PickFlags::DEFAULT)
            .is_none_or(|w| &w == root_for_pick.upcast_ref::<gtk::Widget>())
        {
            let _ = tx_bd.send(Message::TogglePalette);
        }
    });
    root.add_controller(click);

    // Centered, fixed-width panel near the top.
    let panel = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(0)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Start)
        .margin_top(80)
        .width_request(520)
        .build();
    panel.add_css_class("bnk-palette-panel");

    // Plain entry (no search/clear icons) so the field matches the modal's
    // name entry.
    let entry = gtk::Entry::builder()
        .placeholder_text("Search commands...")
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(8)
        .margin_end(8)
        .build();

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .build();

    // Live filter as the user types.
    let tx_q = tx.clone();
    entry.connect_changed(move |e| {
        let _ = tx_q.send(Message::PaletteQueryChanged(e.text().into()));
    });

    // Up/Down move the selection and Escape closes the palette while focus
    // stays in the entry. Capture phase so we fire before the entry's own
    // text handling.
    let key_ctrl = gtk::EventControllerKey::new();
    key_ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
    let tx_keys = tx.clone();
    key_ctrl.connect_key_pressed(move |_, key, _, _| match key {
        gtk::gdk::Key::Up => {
            let _ = tx_keys.send(Message::PaletteSelectPrev);
            glib::Propagation::Stop
        }
        gtk::gdk::Key::Down => {
            let _ = tx_keys.send(Message::PaletteSelectNext);
            glib::Propagation::Stop
        }
        gtk::gdk::Key::Escape => {
            let _ = tx_keys.send(Message::TogglePalette);
            glib::Propagation::Stop
        }
        _ => glib::Propagation::Proceed,
    });
    entry.add_controller(key_ctrl);

    // Enter on the entry executes whichever row is currently selected.
    let tx_enter = tx.clone();
    let msgs_enter = messages.clone();
    let list_enter = list.clone();
    entry.connect_activate(move |_| {
        if let Some(row) = list_enter.selected_row() {
            let idx = row.index() as usize;
            if let Some(msg) = msgs_enter.borrow().get(idx).cloned() {
                let _ = tx_enter.send(Message::TogglePalette);
                let _ = tx_enter.send(msg);
            }
        }
    });

    // Clicking a row executes it directly.
    let tx_row = tx.clone();
    let msgs_row = messages.clone();
    list.connect_row_activated(move |_, row| {
        let idx = row.index() as usize;
        if let Some(msg) = msgs_row.borrow().get(idx).cloned() {
            let _ = tx_row.send(Message::TogglePalette);
            let _ = tx_row.send(msg);
        }
    });

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .max_content_height(420)
        .propagate_natural_height(true)
        .build();
    scrolled.set_child(Some(&list));

    panel.append(&entry);
    panel.append(&scrolled);

    root.append(&panel);

    PalettePanel {
        root,
        entry,
        list,
        scroll: scrolled,
        rendered_labels: Vec::new(),
        messages,
        was_open: false,
    }
}

/// Construct the profile-management modal: dimmed backdrop, centered
/// panel, title, optional body label (delete confirm copy), name entry
/// (create / rename), inline error label, and Confirm / Cancel buttons.
/// Visibility and contents are driven by [`Widgets::refresh_modal`]
/// based on the current `state.modal`.
pub(super) fn build_modal_panel(tx: &Sender<Message>) -> ModalPanel {
    // Backdrop fills the overlay. A press only dismisses the modal when it
    // lands on the backdrop itself, not on the centered panel.
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .vexpand(true)
        .halign(gtk::Align::Fill)
        .valign(gtk::Align::Fill)
        .build();
    root.add_css_class("bnk-palette-backdrop");

    // Pick the widget under the pointer instead of a "swallow" gesture on
    // the panel: that would cancel the buttons' click gestures (resolved on
    // release), so Cancel/Confirm would never fire.
    let backdrop_click = gtk::GestureClick::new();
    let tx_bd = tx.clone();
    let root_for_pick = root.clone();
    backdrop_click.connect_pressed(move |_, _, x, y| {
        if root_for_pick
            .pick(x, y, gtk::PickFlags::DEFAULT)
            .is_none_or(|w| &w == root_for_pick.upcast_ref::<gtk::Widget>())
        {
            let _ = tx_bd.send(Message::ModalDismiss);
        }
    });
    root.add_controller(backdrop_click);

    let panel = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .width_request(360)
        .build();
    panel.add_css_class("bnk-modal-panel");

    let title = gtk::Label::builder().xalign(0.0).build();
    title.add_css_class("bnk-modal-title");

    let body = gtk::Label::builder().xalign(0.0).wrap(true).build();
    body.add_css_class("bnk-modal-body");

    let entry = gtk::Entry::builder()
        .placeholder_text("profile name")
        .hexpand(true)
        .build();

    let tx_change = tx.clone();
    entry.connect_changed(move |e| {
        let _ = tx_change.send(Message::ModalNameChanged(e.text().into()));
    });
    let tx_activate = tx.clone();
    entry.connect_activate(move |_| {
        let _ = tx_activate.send(Message::ModalConfirm);
    });

    let error = gtk::Label::builder().xalign(0.0).wrap(true).build();
    error.add_css_class("bnk-modal-error");

    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();

    let cancel = gtk::Button::with_label("Cancel");
    let tx_cancel = tx.clone();
    cancel.connect_clicked(move |_| {
        let _ = tx_cancel.send(Message::ModalDismiss);
    });

    let confirm = gtk::Button::with_label("Confirm");
    confirm.add_css_class("suggested-action");
    let tx_confirm = tx.clone();
    confirm.connect_clicked(move |_| {
        let _ = tx_confirm.send(Message::ModalConfirm);
    });

    buttons.append(&cancel);
    buttons.append(&confirm);

    // Escape on the panel dismisses the modal. Capture phase so the entry
    // doesn't swallow it first.
    let key_ctrl = gtk::EventControllerKey::new();
    key_ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
    let tx_esc = tx.clone();
    key_ctrl.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            let _ = tx_esc.send(Message::ModalDismiss);
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    panel.add_controller(key_ctrl);

    panel.append(&title);
    panel.append(&body);
    panel.append(&entry);
    panel.append(&error);
    panel.append(&buttons);

    root.append(&panel);

    ModalPanel {
        root,
        title,
        body,
        entry,
        error,
        confirm,
        was_open: false,
    }
}

/// Build a single ListBoxRow for the palette: muted "namespace: " prefix
/// followed by the action.
pub(super) fn build_palette_row(label: &str, palette: Palette) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("bnk-palette-row");

    let h = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();

    let label_widget = gtk::Label::new(None);
    label_widget.set_xalign(0.0);
    label_widget.set_hexpand(true);
    label_widget.set_ellipsize(gtk4::pango::EllipsizeMode::End);

    let escaped = glib::markup_escape_text(label);
    let markup = match label.find(": ") {
        Some(i) => {
            let ns = glib::markup_escape_text(&label[..i + 2]);
            let action = glib::markup_escape_text(&label[i + 2..]);
            format!(
                "<span color=\"{}\">{ns}</span>{action}",
                palette.text_muted.css()
            )
        }
        None => escaped.to_string(),
    };
    label_widget.set_markup(&markup);
    h.append(&label_widget);

    row.set_child(Some(&h));
    row
}
