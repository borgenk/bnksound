//! The profile selector, as GTK widgets.
//!
//! The native shell paints a chip into the mixer surface. Here the selector is
//! a menu button that rides in the header, assembled from stock widgets so the
//! desktop's theme styles it like any other app's. Both send the same messages.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;

use crate::bus::Sender;
use crate::gtk_shell::style;
use crate::state::Message;
use crate::view::snapshot::{ProfileRowView, ViewSnapshot};

/// The label shown when no profile is active.
const NO_ACTIVE_PROFILE: &str = "Profiles";

/// Padding around a menu row's label, in logical pixels.
const ROW_PAD_X: i32 = 6;
const ROW_PAD_Y: i32 = 3;

/// Inset from the popover's edge, so a rounded row has somewhere to round into.
const ROW_INSET: i32 = 4;

/// What the popover was last built from. Rebuilding the list every frame would
/// pull rows out from under the pointer mid-click, so a snapshot that moves
/// neither the names nor the active one is skipped.
#[derive(PartialEq, Eq)]
struct Shown {
    names: Vec<String>,
    active: Option<String>,
}

/// A button naming the active profile, and a popover listing the rest.
pub struct ProfileSelector {
    button: gtk::MenuButton,
    list: gtk::ListBox,
    /// The names behind the list's rows, in row order, so an activated row maps
    /// back to a profile.
    names: Rc<RefCell<Vec<String>>>,
    msg_tx: Sender<Message>,
    shown: RefCell<Option<Shown>>,
}

impl ProfileSelector {
    pub fn new(msg_tx: Sender<Message>) -> Self {
        let names: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));

        // Profiles, a rule, then the row that makes one: the same three parts
        // the painted menu draws, and one list so they are styled alike.
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        list.add_css_class(style::MENU_CLASS);
        list.set_margin_start(ROW_INSET);
        list.set_margin_end(ROW_INSET);
        {
            let names = Rc::clone(&names);
            let msg_tx = msg_tx.clone();
            list.connect_row_activated(move |list, row| {
                let index = usize::try_from(row.index()).unwrap_or(usize::MAX);
                let names = names.borrow();
                // The rule sits between the two, and never activates.
                let message = match index {
                    i if i < names.len() => Message::ApplyProfile(names[i].clone()),
                    i if i == names.len() + 1 => Message::OpenCreateProfileModal,
                    _ => return,
                };
                popdown(list);
                let _ = msg_tx.send(message);
            });
        }

        let popover = gtk::Popover::new();
        popover.set_child(Some(&list));
        // A dropdown rather than a speech bubble: no tail, and its leading edge
        // under the button's rather than centred on it.
        popover.set_has_arrow(false);
        popover.set_halign(gtk::Align::Start);

        // A labelled menu button carries its own dropdown arrow, which is what
        // keeps it reading as a control rather than a title.
        let button = gtk::MenuButton::new();
        button.set_popover(Some(&popover));
        button.add_css_class(style::BUTTON_CLASS);
        // Opening the menu should not pull keyboard focus off whatever in the
        // mixer had it. GTK still focuses the button when the popover closes,
        // so this is about where focus goes, not about the ring that used to
        // follow it; the stylesheet handles that.
        button.set_focus_on_click(false);
        // Frameless, the way header controls usually are. This asks for the
        // theme's own flat button rather than overriding what a frame looks
        // like, so the hover and pressed states stay the theme's.
        button.set_has_frame(false);

        ProfileSelector {
            button,
            list,
            names,
            msg_tx,
            shown: RefCell::new(None),
        }
    }

    /// The widget the header hosts.
    pub fn widget(&self) -> &gtk::MenuButton {
        &self.button
    }

    /// Bring the button's label and the popover's rows in line with the
    /// snapshot.
    pub fn sync(&self, snapshot: &ViewSnapshot) {
        let next = Shown {
            names: snapshot
                .profile
                .rows
                .iter()
                .map(|row| row.name.clone())
                .collect(),
            active: snapshot.profile.active.clone(),
        };
        if self.shown.borrow().as_ref() == Some(&next) {
            return;
        }

        self.button
            .set_label(next.active.as_deref().unwrap_or(NO_ACTIVE_PROFILE));

        while let Some(row) = self.list.first_child() {
            self.list.remove(&row);
        }
        for row in &snapshot.profile.rows {
            self.list.append(&self.row(row));
        }
        self.list.append(&rule());
        self.list.append(&action_row("New profile"));

        self.names.borrow_mut().clone_from(&next.names);
        *self.shown.borrow_mut() = Some(next);
    }

    /// One profile: its name, and a check on the active one.
    fn row(&self, view: &ProfileRowView) -> gtk::ListBoxRow {
        let label = row_label(&view.name);

        let check = gtk::Image::from_icon_name("object-select-symbolic");
        check.set_visible(view.active);

        // Menu rows want breathing room the bare list does not give them.
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        content.set_margin_start(ROW_PAD_X);
        content.set_margin_end(ROW_PAD_X);
        content.set_margin_top(ROW_PAD_Y);
        content.set_margin_bottom(ROW_PAD_Y);
        content.append(&label);
        content.append(&check);

        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&content));
        if view.active {
            row.add_css_class(style::ACTIVE_CLASS);
        }
        self.attach_reorder(&row, &view.name);
        row
    }

    /// Dragging one row onto another reorders the profiles, the gesture the
    /// painted menu also answers to. Which half of the target it lands on
    /// decides the side.
    fn attach_reorder(&self, row: &gtk::ListBoxRow, name: &str) {
        let source = gtk::DragSource::new();
        source.set_actions(gdk::DragAction::MOVE);
        {
            let name = name.to_string();
            source.connect_prepare(move |_, _, _| {
                Some(gdk::ContentProvider::for_value(&name.to_value()))
            });
        }
        row.add_controller(source);

        let target = gtk::DropTarget::new(glib::types::Type::STRING, gdk::DragAction::MOVE);
        {
            let onto = name.to_string();
            let msg_tx = self.msg_tx.clone();
            target.connect_drop(move |target, value, _, y| {
                let Ok(dragged) = value.get::<String>() else {
                    return false;
                };
                if dragged == onto {
                    return false;
                }
                let height = target.widget().map_or(0, |w| w.height());
                let _ = msg_tx.send(Message::ReorderProfile {
                    name: dragged,
                    target: onto.clone(),
                    before: y < f64::from(height) / 2.0,
                });
                true
            });
        }
        row.add_controller(target);
    }
}

/// A row's label: left-aligned, and taking the width so anything beside it is
/// pushed to the far edge.
fn row_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label
}

/// A row carrying an action rather than a profile, padded to match the ones
/// above it.
fn action_row(text: &str) -> gtk::ListBoxRow {
    let label = row_label(text);
    label.set_margin_start(ROW_PAD_X);
    label.set_margin_end(ROW_PAD_X);
    label.set_margin_top(ROW_PAD_Y);
    label.set_margin_bottom(ROW_PAD_Y);

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&label));
    row
}

/// The rule between picking a profile and making one. It sits in the list to
/// stay aligned with the rows, and takes neither focus nor activation so it is
/// never something a keyboard can land on.
fn rule() -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&gtk::Separator::new(gtk::Orientation::Horizontal)));
    row.set_activatable(false);
    row.set_selectable(false);
    row.set_focusable(false);
    row
}

/// Close the popover a widget sits in. Reaching for it through the widget tree
/// keeps the rows from holding a reference back to their own popover.
fn popdown(widget: &impl IsA<gtk::Widget>) {
    if let Some(popover) = widget
        .as_ref()
        .ancestor(gtk::Popover::static_type())
        .and_downcast::<gtk::Popover>()
    {
        popover.popdown();
    }
}
