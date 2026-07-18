//! Normalized input values and the mapping from an event to state.
//!
//! Wayland and GTK each translate their native events into these values, so the
//! mapping from an event and the hovered hit target to a state message or a
//! transient UI change is written once and shared. Nothing here is toolkit- or
//! protocol-specific; it is the vocabulary both shells speak.

use crate::render::primitives::Rect;
use crate::render::text::Font;
use crate::state::Message;
use crate::ui::layout::{HitTarget, Layout, ResizeEdge, RowId, cubic_for_y};
use crate::ui::{Drag, Focus, UiState};
use crate::view::snapshot::ViewSnapshot;

/// The modifier keys held during an event.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Modifiers {
    /// No modifier held (a bare key press).
    pub fn is_none(self) -> bool {
        !self.ctrl && !self.shift && !self.alt
    }

    /// Exactly Ctrl (no Shift or Alt), the common shortcut form.
    pub fn is_ctrl(self) -> bool {
        self.ctrl && !self.shift && !self.alt
    }
}

/// A key, normalized. Text-producing keys arrive as Char; the rest are named
/// so the mapping can act on them without decoding keysyms.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Char(char),
    Enter,
    Escape,
    Backspace,
    Delete,
    Tab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

impl Key {
    /// The named key an X11 keysym stands for, or None when the keysym is a
    /// character or something the UI does not act on.
    ///
    /// Both shells need this table. Wayland delivers keysyms through xkb and
    /// GDK's keyvals are the same numbers, so the naming is written once here
    /// and each shell brings its own way of turning the rest into characters.
    pub fn from_keysym(sym: u32) -> Option<Self> {
        Some(match sym {
            keysym::RETURN | keysym::KP_ENTER | keysym::ISO_ENTER => Self::Enter,
            keysym::ESCAPE => Self::Escape,
            keysym::BACKSPACE => Self::Backspace,
            keysym::DELETE | keysym::KP_DELETE => Self::Delete,
            keysym::TAB | keysym::ISO_LEFT_TAB => Self::Tab,
            keysym::LEFT | keysym::KP_LEFT => Self::Left,
            keysym::RIGHT | keysym::KP_RIGHT => Self::Right,
            keysym::UP | keysym::KP_UP => Self::Up,
            keysym::DOWN | keysym::KP_DOWN => Self::Down,
            keysym::HOME | keysym::KP_HOME => Self::Home,
            keysym::END | keysym::KP_END => Self::End,
            _ => return None,
        })
    }
}

/// X11 keysym values for the keys the UI acts on. Wayland and GDK both speak
/// these numbers.
pub mod keysym {
    pub const BACKSPACE: u32 = 0xff08;
    pub const TAB: u32 = 0xff09;
    pub const RETURN: u32 = 0xff0d;
    pub const ESCAPE: u32 = 0xff1b;
    pub const HOME: u32 = 0xff50;
    pub const LEFT: u32 = 0xff51;
    pub const UP: u32 = 0xff52;
    pub const RIGHT: u32 = 0xff53;
    pub const DOWN: u32 = 0xff54;
    pub const END: u32 = 0xff57;
    pub const KP_ENTER: u32 = 0xff8d;
    pub const KP_HOME: u32 = 0xff95;
    pub const KP_LEFT: u32 = 0xff96;
    pub const KP_UP: u32 = 0xff97;
    pub const KP_RIGHT: u32 = 0xff98;
    pub const KP_DOWN: u32 = 0xff99;
    pub const KP_END: u32 = 0xff9c;
    pub const KP_DELETE: u32 = 0xff9f;
    pub const ISO_LEFT_TAB: u32 = 0xfe20;
    pub const ISO_ENTER: u32 = 0xfe34;
    pub const DELETE: u32 = 0xffff;
}

/// A key press with its modifiers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyEvent {
    pub key: Key,
    pub mods: Modifiers,
}

/// A pointer button.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// What a pointer event did.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PointerAction {
    Motion,
    Press(MouseButton),
    Release(MouseButton),
    /// Wheel or touchpad scroll, in logical pixels. Positive dy scrolls down.
    Scroll {
        dx: f32,
        dy: f32,
    },
}

/// A pointer event at a logical position within the content.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PointerEvent {
    pub x: f64,
    pub y: f64,
    pub action: PointerAction,
}

impl PointerEvent {
    pub fn position(self) -> (f64, f64) {
        (self.x, self.y)
    }
}

/// A clipboard operation a key press asked for. The shells perform the actual
/// transfer, which is platform-specific, but agree on what the keys mean.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClipboardAction {
    Copy,
    Cut,
    Paste,
}

/// A window-management request. Moving, resizing, and closing a window are the
/// shell's business, not the mixer's, so these never become state messages; the
/// shared code only names what the press asked for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WindowAction {
    Move,
    Resize(ResizeEdge),
    Minimize,
    ToggleMaximize,
    Close,
}

/// The window action a left press on `target` asks for, or None. Only reachable
/// when the shell draws its own chrome; layout emits no such targets otherwise.
pub fn window_action(target: &HitTarget) -> Option<WindowAction> {
    Some(match target {
        HitTarget::TitlebarDrag => WindowAction::Move,
        HitTarget::ResizeEdge(edge) => WindowAction::Resize(*edge),
        HitTarget::WindowMinimize => WindowAction::Minimize,
        HitTarget::WindowMaximize => WindowAction::ToggleMaximize,
        HitTarget::WindowClose => WindowAction::Close,
        _ => return None,
    })
}

/// Whether a key press asks for a screenshot (Ctrl+Shift+S).
pub fn is_screenshot_key(event: KeyEvent) -> bool {
    event.mods.ctrl
        && event.mods.shift
        && !event.mods.alt
        && matches!(event.key, Key::Char('s') | Key::Char('S'))
}

/// The clipboard action a key press means, or None. Only an overlay editor
/// takes these; the mixer body has no text to copy.
pub fn clipboard_action(ui: &UiState, event: KeyEvent) -> Option<ClipboardAction> {
    if !ui.overlay_focused() || !event.mods.is_ctrl() {
        return None;
    }
    match event.key {
        Key::Char('c') | Key::Char('C') => Some(ClipboardAction::Copy),
        Key::Char('x') | Key::Char('X') => Some(ClipboardAction::Cut),
        Key::Char('v') | Key::Char('V') => Some(ClipboardAction::Paste),
        _ => None,
    }
}

/// Mirror the focused editor's text into state after a clipboard edit.
pub fn editor_text_message(ui: &UiState) -> Option<Message> {
    match ui.focus {
        Focus::Palette => Some(Message::PaletteQueryChanged(ui.editor.text().to_string())),
        Focus::Modal => Some(Message::ModalNameChanged(ui.editor.text().to_string())),
        Focus::Body => None,
    }
}

/// The state message a pressed row slider or row produces for a cubic volume.
fn volume_msg(id: &RowId, cubic: f32) -> Message {
    match id {
        RowId::Sink(i) | RowId::Source(i) | RowId::AppMember(i) => {
            Message::VolumeChanged(*i, cubic)
        }
        RowId::AppGroup(key) => Message::GroupVolumeChanged {
            key: key.clone(),
            cubic,
        },
    }
}

/// The slider track rectangle of a column, for drag mapping.
fn slider_track(layout: &Layout, id: &RowId) -> Option<Rect> {
    layout
        .columns
        .iter()
        .find(|c| &c.id == id)
        .map(|c| c.slider.track)
}

/// The state message a button hit target produces. None for targets handled by
/// the caller (sliders, the profile selector, overlay inputs).
fn button_message(target: &HitTarget) -> Option<Message> {
    use HitTarget::*;
    Some(match target {
        SectionFilter(sec) => Message::ToggleSection(*sec),
        MuteAll => Message::MuteAllToggled,
        ResetTargets => Message::ResetAllStreamTargets,
        DeviceDefault(RowId::Sink(id)) => Message::MakeDefault(*id),
        DeviceDefault(RowId::Source(id)) => Message::MakeDefaultSource(*id),
        RowMute(RowId::Sink(id) | RowId::Source(id) | RowId::AppMember(id)) => {
            Message::MuteToggled(*id)
        }
        RowMute(RowId::AppGroup(key)) => Message::GroupMuteToggled(key.clone()),
        AppExpand(RowId::AppGroup(key)) => Message::GroupToggleExpanded(key.clone()),
        AppTarget {
            row: RowId::AppGroup(key),
            sink: Some(id),
        } => Message::GroupSetStreamTarget {
            key: key.clone(),
            sink_id: *id,
        },
        AppTarget {
            row: RowId::AppGroup(key),
            sink: None,
        } => Message::GroupClearStreamTarget(key.clone()),
        AppTarget {
            row: RowId::AppMember(id),
            sink: Some(sink),
        } => Message::SetStreamTarget {
            app_id: *id,
            sink_id: *sink,
        },
        AppTarget {
            row: RowId::AppMember(id),
            sink: None,
        } => Message::ClearStreamTarget(*id),
        ProfileCreate => Message::OpenCreateProfileModal,
        _ => return None,
    })
}

/// The char offset in `text` that pointer x lands on within a text field.
/// Measured from the same origin the field draws its first glyph at.
fn caret_offset(font: &Font, field: Rect, text: &str, x: f64) -> usize {
    use crate::ui::layout::metrics::{FIELD_PAD, FIELD_TEXT_SIZE};
    let rel = (x - f64::from(field.x + FIELD_PAD)) as f32;
    font.char_offset_at_x(text, rel.max(0.0), FIELD_TEXT_SIZE)
}

/// What releasing a profile drag means: a reorder when it landed on a different
/// row, an apply when it never left the row it started on. Releasing off the
/// menu cancels.
fn profile_drop(layout: &Layout, name: &str, x: i32, y: i32) -> Option<Message> {
    let menu = layout.profile_menu.as_ref()?;
    let row = menu.rows.iter().find(|r| r.rect.contains(x, y))?;
    if row.name == name {
        return Some(Message::ApplyProfile(name.to_string()));
    }
    Some(Message::ReorderProfile {
        name: name.to_string(),
        target: row.name.clone(),
        // Which half of the row it was dropped on decides the side.
        before: y < row.rect.y + row.rect.h / 2,
    })
}

/// Handle a pointer event: update transient state and emit any messages.
/// `now_ms` stamps clicks for multi-click detection; `font` measures text so a
/// click in a field lands on the character under it.
pub fn on_pointer(
    ui: &mut UiState,
    layout: &Layout,
    snapshot: &ViewSnapshot,
    event: PointerEvent,
    now_ms: u64,
    font: &Font,
) -> Vec<Message> {
    let mut msgs = Vec::new();
    ui.pointer = (event.x, event.y);
    let (xi, yi) = (event.x as i32, event.y as i32);

    match event.action {
        PointerAction::Motion => {
            let hover = layout.hit(xi, yi).cloned();
            // The ring belongs to the knob alone, while the click belongs to
            // the whole track, so the two are hit-tested separately. Going
            // through `hover` first keeps a knob under an overlay unlit.
            let knob_hover = match &hover {
                Some(HitTarget::RowSlider(row)) => layout
                    .columns
                    .iter()
                    .find(|c| &c.id == row)
                    .filter(|c| c.slider.thumb.contains(xi, yi))
                    .map(|c| c.id.clone()),
                _ => None,
            };
            if hover != ui.hover {
                ui.hover = hover;
                ui.dirty.mark_full();
            }
            if knob_hover != ui.knob_hover {
                ui.knob_hover = knob_hover;
                ui.dirty.mark_full();
            }
            match ui.drag.clone() {
                Some(Drag::Slider(id)) => {
                    if let Some(track) = slider_track(layout, &id) {
                        msgs.push(volume_msg(&id, cubic_for_y(track, yi)));
                    }
                }
                // A selection drag keeps tracking the field it began in, even
                // once the pointer has left it.
                Some(Drag::TextSelect) => {
                    if let Some(field) = layout.focused_field(ui.focus) {
                        let at = caret_offset(font, field, ui.editor.text(), event.x);
                        ui.editor.drag(at);
                        ui.dirty.mark_full();
                    }
                }
                Some(Drag::StripScroll { grab }) => {
                    if let Some(next) = scroll_for_slider(layout, xi - grab)
                        && next != ui.scroll_x
                    {
                        ui.scroll_x = next;
                        ui.dirty.mark_full();
                    }
                }
                Some(Drag::ProfileReorder(_)) | None => {}
            }
        }
        PointerAction::Press(MouseButton::Left) => {
            let target = layout.hit(xi, yi).cloned();
            ui.pressed = target.clone();
            let clicks = ui.click.press(now_ms, event.x, event.y);
            match target {
                Some(HitTarget::RowSlider(id)) => {
                    if let Some(track) = slider_track(layout, &id) {
                        msgs.push(volume_msg(&id, cubic_for_y(track, yi)));
                    }
                    ui.drag = Some(Drag::Slider(id));
                }
                Some(HitTarget::ProfileSelector) => {
                    ui.profile_menu_open = !ui.profile_menu_open;
                }
                // On the slider, drag from where it was grabbed. Beside it, the
                // slider jumps to the pointer and the drag carries on from its
                // middle, so one press both pages and grabs.
                Some(HitTarget::StripScrollbar) => {
                    if let Some(bar) = &layout.strip_scrollbar {
                        let grab = if bar.slider.contains(xi, yi) {
                            xi - bar.slider.x
                        } else {
                            let half = bar.slider.w / 2;
                            if let Some(next) = scroll_for_slider(layout, xi - half) {
                                ui.scroll_x = next;
                            }
                            half
                        };
                        ui.drag = Some(Drag::StripScroll { grab });
                    }
                }
                // A press on a profile may turn into a reorder, so nothing is
                // applied until release tells the two apart.
                Some(HitTarget::ProfileApply(name)) => {
                    ui.drag = Some(Drag::ProfileReorder(name));
                }
                Some(t @ (HitTarget::PaletteInput | HitTarget::ModalInput)) => {
                    ui.focus = if t == HitTarget::PaletteInput {
                        Focus::Palette
                    } else {
                        Focus::Modal
                    };
                    if let Some(field) = layout.focused_field(ui.focus) {
                        let at = caret_offset(font, field, ui.editor.text(), event.x);
                        ui.editor.click(at, clicks);
                        ui.drag = Some(Drag::TextSelect);
                    }
                }
                Some(HitTarget::PaletteRow(i)) => {
                    if let Some(m) = snapshot.palette.messages.get(i) {
                        msgs.push(m.clone());
                        msgs.push(Message::TogglePalette);
                        ui.focus = Focus::Body;
                    }
                }
                Some(HitTarget::ModalConfirm) => msgs.push(Message::ModalConfirm),
                Some(HitTarget::ModalCancel) => msgs.push(Message::ModalDismiss),
                // Pressing the dimmed area around an overlay dismisses it.
                Some(HitTarget::Backdrop) => {
                    if snapshot.modal.is_some() {
                        msgs.push(Message::ModalDismiss);
                    } else if snapshot.palette.open {
                        msgs.push(Message::TogglePalette);
                    }
                    ui.focus = Focus::Body;
                }
                Some(t) => {
                    if let Some(m) = button_message(&t) {
                        msgs.push(m);
                    }
                }
                None => ui.profile_menu_open = false,
            }
            ui.dirty.mark_full();
        }
        PointerAction::Release(_) => {
            if let Some(Drag::ProfileReorder(name)) = ui.drag.take()
                && let Some(m) = profile_drop(layout, &name, xi, yi)
            {
                msgs.push(m);
                ui.profile_menu_open = false;
            }
            ui.drag = None;
            ui.pressed = None;
            ui.dirty.mark_full();
        }
        PointerAction::Scroll { dx, dy } => {
            // The columns are one row, so both axes push it sideways: a wheel
            // has only the vertical one, and a touchpad's sideways swipe means
            // the same thing here. Sub-pixel remainders carry to the next
            // event rather than truncating to nothing.
            // An open palette takes the wheel: it is what the pointer is over,
            // and the mixer behind it is not going anywhere. It moves by whole
            // rows, so the pixels carry in their own remainder.
            if let Some(palette) = &layout.palette {
                let row_h = crate::ui::layout::metrics::PALETTE_ROW_H as f32;
                let delta = ui.palette_wheel + dx + dy;
                let rows = (delta / row_h).trunc();
                ui.palette_wheel = delta - rows * row_h;
                // Scrolling starts from the window on screen and stops where
                // the list ends, both of which only the projection knows.
                let last = snapshot.palette.rows.len() - palette.rows.len();
                let next = (palette.first_visible as i64 + rows as i64).clamp(0, last as i64);
                if next as usize != palette.first_visible {
                    msgs.push(Message::PaletteScrollTo(next as usize));
                }
            } else {
                let delta = ui.scroll_residual + dx + dy;
                let whole = delta.trunc();
                ui.scroll_residual = delta - whole;
                let next = (ui.scroll_x + whole as i32).clamp(0, layout.scroll_max_x);
                if next != ui.scroll_x {
                    ui.scroll_x = next;
                    ui.dirty.mark_full();
                }
            }
        }
        PointerAction::Press(_) => {}
    }
    msgs
}

/// The scroll offset that puts the strip's scrollbar slider at `slider_x`,
/// clamped to the run it can travel. `None` when nothing is scrollable.
fn scroll_for_slider(layout: &Layout, slider_x: i32) -> Option<i32> {
    let bar = layout.strip_scrollbar.as_ref()?;
    let travel = bar.track.w - bar.slider.w;
    if travel <= 0 {
        return Some(0);
    }
    let progress = (slider_x - bar.track.x) as f32 / travel as f32;
    let at = (layout.scroll_max_x as f32 * progress).round() as i32;
    Some(at.clamp(0, layout.scroll_max_x))
}

/// The row the pointer is over, if any (for the bare-m mute shortcut).
fn hovered_row(ui: &UiState) -> Option<&RowId> {
    match ui.hover.as_ref()? {
        HitTarget::RowSlider(id) | HitTarget::RowMute(id) | HitTarget::AppExpand(id) => Some(id),
        _ => None,
    }
}

/// Handle a key event: route to the focused overlay's editor or the body
/// shortcuts, and emit any messages.
pub fn on_key(ui: &mut UiState, snapshot: &ViewSnapshot, event: KeyEvent) -> Vec<Message> {
    let mut msgs = Vec::new();
    match ui.focus {
        Focus::Palette => palette_key(ui, snapshot, event, &mut msgs),
        Focus::Modal => modal_key(ui, event, &mut msgs),
        Focus::Body => body_key(ui, event, &mut msgs),
    }
    ui.dirty.mark_full();
    msgs
}

fn body_key(ui: &mut UiState, event: KeyEvent, msgs: &mut Vec<Message>) {
    match event.key {
        Key::Char('k') | Key::Char('K') if event.mods.is_ctrl() => {
            ui.focus = Focus::Palette;
            ui.editor.clear();
            msgs.push(Message::TogglePalette);
        }
        Key::Char('m') | Key::Char('M') if event.mods.is_none() => {
            if let Some(id) = hovered_row(ui) {
                let m = match id {
                    RowId::AppGroup(key) => Message::GroupMuteToggled(key.clone()),
                    RowId::Sink(i) | RowId::Source(i) | RowId::AppMember(i) => {
                        Message::MuteToggled(*i)
                    }
                };
                msgs.push(m);
            }
        }
        _ => {}
    }
}

fn palette_key(
    ui: &mut UiState,
    snapshot: &ViewSnapshot,
    event: KeyEvent,
    msgs: &mut Vec<Message>,
) {
    match event.key {
        Key::Escape => {
            ui.focus = Focus::Body;
            msgs.push(Message::TogglePalette);
        }
        Key::Enter => {
            if let Some(m) = snapshot.palette.messages.get(snapshot.palette.selected) {
                msgs.push(m.clone());
            }
            ui.focus = Focus::Body;
            msgs.push(Message::TogglePalette);
        }
        Key::Up => msgs.push(Message::PaletteSelectPrev),
        Key::Down => msgs.push(Message::PaletteSelectNext),
        _ => {
            if edit_key(&mut ui.editor, event) {
                msgs.push(Message::PaletteQueryChanged(ui.editor.text().to_string()));
            }
        }
    }
}

fn modal_key(ui: &mut UiState, event: KeyEvent, msgs: &mut Vec<Message>) {
    match event.key {
        Key::Escape => {
            ui.focus = Focus::Body;
            msgs.push(Message::ModalDismiss);
        }
        Key::Enter => msgs.push(Message::ModalConfirm),
        _ => {
            if edit_key(&mut ui.editor, event) {
                msgs.push(Message::ModalNameChanged(ui.editor.text().to_string()));
            }
        }
    }
}

/// Apply an editing key to the editor. Returns whether the text changed, so the
/// caller mirrors it into state; cursor-only moves return false.
fn edit_key(editor: &mut crate::ui::editor::Editor, event: KeyEvent) -> bool {
    let shift = event.mods.shift;
    match event.key {
        Key::Char('a') | Key::Char('A') if event.mods.is_ctrl() => {
            editor.select_all();
            false
        }
        Key::Char(c) if !event.mods.ctrl && !event.mods.alt => editor.insert(c),
        Key::Backspace => {
            editor.backspace();
            true
        }
        Key::Delete => {
            editor.delete();
            true
        }
        Key::Left => {
            editor.left(shift);
            false
        }
        Key::Right => {
            editor.right(shift);
            false
        }
        Key::Home => {
            editor.home(shift);
            false
        }
        Key::End => {
            editor.end(shift);
            false
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Section, Stream, StreamKind};
    use crate::state;
    use crate::view::snapshot::build_snapshot;

    #[test]
    fn keysyms_name_the_keys_the_ui_acts_on() {
        assert_eq!(Key::from_keysym(keysym::RETURN), Some(Key::Enter));
        assert_eq!(Key::from_keysym(keysym::ESCAPE), Some(Key::Escape));
        assert_eq!(Key::from_keysym(keysym::BACKSPACE), Some(Key::Backspace));
        assert_eq!(Key::from_keysym(keysym::DELETE), Some(Key::Delete));
        assert_eq!(Key::from_keysym(keysym::TAB), Some(Key::Tab));
        assert_eq!(Key::from_keysym(keysym::LEFT), Some(Key::Left));
        assert_eq!(Key::from_keysym(keysym::RIGHT), Some(Key::Right));
        assert_eq!(Key::from_keysym(keysym::UP), Some(Key::Up));
        assert_eq!(Key::from_keysym(keysym::DOWN), Some(Key::Down));
        assert_eq!(Key::from_keysym(keysym::HOME), Some(Key::Home));
        assert_eq!(Key::from_keysym(keysym::END), Some(Key::End));
    }

    /// The keypad and ISO variants reach the same keys. Both shells share this
    /// table now, so a keypad Enter works in either.
    #[test]
    fn keypad_and_iso_variants_reach_the_same_keys() {
        assert_eq!(Key::from_keysym(keysym::KP_ENTER), Some(Key::Enter));
        assert_eq!(Key::from_keysym(keysym::ISO_ENTER), Some(Key::Enter));
        assert_eq!(Key::from_keysym(keysym::ISO_LEFT_TAB), Some(Key::Tab));
        assert_eq!(Key::from_keysym(keysym::KP_DELETE), Some(Key::Delete));
        assert_eq!(Key::from_keysym(keysym::KP_LEFT), Some(Key::Left));
        assert_eq!(Key::from_keysym(keysym::KP_RIGHT), Some(Key::Right));
        assert_eq!(Key::from_keysym(keysym::KP_UP), Some(Key::Up));
        assert_eq!(Key::from_keysym(keysym::KP_DOWN), Some(Key::Down));
        assert_eq!(Key::from_keysym(keysym::KP_HOME), Some(Key::Home));
        assert_eq!(Key::from_keysym(keysym::KP_END), Some(Key::End));
    }

    /// Characters are not named keys: each shell turns those into Key::Char
    /// with its own keysym-to-character conversion.
    #[test]
    fn character_keysyms_are_left_to_the_shell() {
        assert_eq!(Key::from_keysym(0x6b), None, "the keysym for k");
        assert_eq!(Key::from_keysym(0x20), None, "space");
        assert_eq!(Key::from_keysym(0), None, "no symbol at all");
    }

    #[test]
    fn clipboard_keys_only_apply_to_a_focused_editor() {
        let ctrl = Modifiers {
            ctrl: true,
            ..Default::default()
        };
        let mut ui = UiState::new();
        let ev = |k| KeyEvent { key: k, mods: ctrl };

        // The mixer body has no text, so Ctrl+C there is not a clipboard action.
        assert_eq!(clipboard_action(&ui, ev(Key::Char('c'))), None);

        ui.focus = Focus::Palette;
        assert_eq!(
            clipboard_action(&ui, ev(Key::Char('c'))),
            Some(ClipboardAction::Copy)
        );
        assert_eq!(
            clipboard_action(&ui, ev(Key::Char('x'))),
            Some(ClipboardAction::Cut)
        );
        assert_eq!(
            clipboard_action(&ui, ev(Key::Char('v'))),
            Some(ClipboardAction::Paste)
        );
        // A bare letter is typing, not a shortcut.
        assert_eq!(
            clipboard_action(
                &ui,
                KeyEvent {
                    key: Key::Char('v'),
                    mods: Modifiers::default()
                }
            ),
            None
        );
    }

    #[test]
    fn modifier_predicates() {
        assert!(Modifiers::default().is_none());
        let ctrl = Modifiers {
            ctrl: true,
            ..Default::default()
        };
        assert!(ctrl.is_ctrl());
        assert!(!ctrl.is_none());
        let ctrl_shift = Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
        };
        assert!(!ctrl_shift.is_ctrl(), "ctrl+shift is not bare ctrl");
    }

    #[test]
    fn pointer_position_reads_back() {
        let e = PointerEvent {
            x: 12.5,
            y: 7.0,
            action: PointerAction::Press(MouseButton::Left),
        };
        assert_eq!(e.position(), (12.5, 7.0));
    }

    fn font() -> Font {
        Font::from_path(std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/test-font.ttf"
        )))
        .expect("fixture font")
    }

    fn scene() -> (UiState, Layout, ViewSnapshot) {
        let mut app = state::empty();
        app.streams.insert(
            1,
            Stream {
                id: 1,
                kind: StreamKind::Sink,
                name: "Speaker".into(),
                app_id: None,
                binary: None,
                pid: None,
                node_name: Some("node.1".into()),
                media_name: None,
                media_role: None,
                channel_volumes: vec![0.5, 0.5],
                muted: false,
                xdg: None,
                form: None,
                is_default: false,
                target_sink_name: None,
            },
        );
        let snap = build_snapshot(&app, |_| None);
        let layout = crate::ui::layout::project(&snap, &UiState::new(), Rect::new(0, 0, 560, 720));
        (UiState::new(), layout, snap)
    }

    fn press_at(
        ui: &mut UiState,
        layout: &Layout,
        snap: &ViewSnapshot,
        x: i32,
        y: i32,
    ) -> Vec<Message> {
        pointer_at(
            ui,
            layout,
            snap,
            PointerAction::Press(MouseButton::Left),
            x,
            y,
            0,
        )
    }

    fn pointer_at(
        ui: &mut UiState,
        layout: &Layout,
        snap: &ViewSnapshot,
        action: PointerAction,
        x: i32,
        y: i32,
        ms: u64,
    ) -> Vec<Message> {
        on_pointer(
            ui,
            layout,
            snap,
            PointerEvent {
                x: x as f64,
                y: y as f64,
                action,
            },
            ms,
            &font(),
        )
    }

    /// A strip holding more columns than the window fits, so there is somewhere
    /// to scroll to.
    fn wide_scene() -> (UiState, Layout, ViewSnapshot) {
        let mut app = state::empty();
        for id in 1..=8 {
            app.streams.insert(
                id,
                Stream {
                    id,
                    kind: StreamKind::Sink,
                    name: format!("Speaker {id}"),
                    app_id: None,
                    binary: None,
                    pid: None,
                    node_name: Some(format!("node.{id}")),
                    media_name: None,
                    media_role: None,
                    channel_volumes: vec![0.5, 0.5],
                    muted: false,
                    xdg: None,
                    form: None,
                    is_default: false,
                    target_sink_name: None,
                },
            );
        }
        let snap = build_snapshot(&app, |_| None);
        let ui = UiState::new();
        let layout = crate::ui::layout::project(&snap, &ui, Rect::new(0, 0, 560, 720));
        (ui, layout, snap)
    }

    fn scroll_by(
        ui: &mut UiState,
        layout: &Layout,
        snap: &ViewSnapshot,
        dx: f32,
        dy: f32,
    ) -> Vec<Message> {
        pointer_at(ui, layout, snap, PointerAction::Scroll { dx, dy }, 0, 0, 0)
    }

    /// A touchpad swiped sideways reports its delta on the horizontal axis, and
    /// the strip it is over runs sideways, so it has to move.
    #[test]
    fn a_sideways_swipe_scrolls_the_strip() {
        let (mut ui, layout, snap) = wide_scene();
        assert!(
            layout.scroll_max_x > 0,
            "the scene is wider than the window"
        );

        scroll_by(&mut ui, &layout, &snap, 30.0, 0.0);
        assert_eq!(ui.scroll_x, 30, "a horizontal swipe moves the strip");

        // A wheel notch, which only ever reports the vertical axis, moves the
        // same strip the same way.
        scroll_by(&mut ui, &layout, &snap, 0.0, 10.0);
        assert_eq!(ui.scroll_x, 40);

        // Scrolling back past the start stops at it.
        scroll_by(&mut ui, &layout, &snap, -500.0, 0.0);
        assert_eq!(ui.scroll_x, 0);
    }

    /// Dragging the scrollbar takes the strip with it, and the slider keeps
    /// the part of itself that was grabbed under the pointer.
    #[test]
    fn dragging_the_scrollbar_scrolls_the_strip() {
        let (mut ui, layout, snap) = wide_scene();
        let bar = layout.strip_scrollbar.as_ref().expect("scrollbar");
        let y = bar.track.y + bar.track.h / 2;
        let grab = 3;

        press_at(&mut ui, &layout, &snap, bar.slider.x + grab, y);
        assert!(matches!(ui.drag, Some(Drag::StripScroll { grab: 3 })));
        assert_eq!(ui.scroll_x, 0, "the press alone moves nothing");

        // Drag to the far end of the track, which is the end of the strip.
        pointer_at(
            &mut ui,
            &layout,
            &snap,
            PointerAction::Motion,
            bar.track.right(),
            y,
            0,
        );
        assert_eq!(ui.scroll_x, layout.scroll_max_x);

        pointer_at(&mut ui, &layout, &snap, PointerAction::Motion, 0, y, 0);
        assert_eq!(ui.scroll_x, 0, "and back to the first column");
    }

    /// A press beside the slider is how a scrollbar pages: the slider comes to
    /// the pointer instead of waiting to be dragged there.
    #[test]
    fn pressing_the_track_brings_the_slider_to_the_pointer() {
        let (mut ui, layout, snap) = wide_scene();
        let bar = layout.strip_scrollbar.as_ref().expect("scrollbar");
        let y = bar.track.y + bar.track.h / 2;

        press_at(&mut ui, &layout, &snap, bar.track.right() - 1, y);
        assert_eq!(ui.scroll_x, layout.scroll_max_x);
        assert!(
            matches!(ui.drag, Some(Drag::StripScroll { .. })),
            "the same press keeps hold of the slider"
        );
    }

    #[test]
    fn a_strip_that_fits_has_no_scrollbar_to_press() {
        let (_, layout, _) = scene();
        assert_eq!(layout.scroll_max_x, 0);
        assert!(layout.strip_scrollbar.is_none());
    }

    /// A touchpad reports fractions of a pixel. Truncating each one on its own
    /// leaves a slow drag moving nothing at all, however far it goes.
    #[test]
    fn sub_pixel_scrolls_accumulate_instead_of_vanishing() {
        let (mut ui, layout, snap) = wide_scene();
        for _ in 0..3 {
            scroll_by(&mut ui, &layout, &snap, 0.0, 0.3);
            assert_eq!(ui.scroll_x, 0, "under a pixel, so nothing has moved yet");
        }
        scroll_by(&mut ui, &layout, &snap, 0.0, 0.3);
        assert_eq!(ui.scroll_x, 1, "four tenths-of-a-pixel steps add up to one");
    }

    /// A palette holding more commands than one window of rows can show.
    fn scrolling_palette_scene() -> (UiState, Layout, ViewSnapshot) {
        let mut app = state::empty();
        app.profiles.profiles.clear();
        for i in 0..crate::command_palette::VISIBLE_ROWS + 6 {
            app.profiles.profiles.push(crate::profile::Profile {
                name: format!("p{i}"),
                ..Default::default()
            });
        }
        app.palette_open = true;
        let snap = build_snapshot(&app, |_| None);
        let mut ui = UiState::new();
        ui.focus = Focus::Palette;
        let layout = crate::ui::layout::project(&snap, &ui, Rect::new(0, 0, 560, 720));
        (ui, layout, snap)
    }

    /// With the palette open the wheel belongs to its list, and it counts in
    /// rows rather than pixels.
    #[test]
    fn the_wheel_moves_the_palette_list_and_leaves_the_strip_alone() {
        let (mut ui, layout, snap) = scrolling_palette_scene();
        let row_h = crate::ui::layout::metrics::PALETTE_ROW_H as f32;

        let msgs = scroll_by(&mut ui, &layout, &snap, 0.0, row_h * 2.0);
        assert!(matches!(msgs.as_slice(), [Message::PaletteScrollTo(2)]));
        assert_eq!(ui.scroll_x, 0, "the mixer behind it stays put");

        // Half a row moves nothing yet; the other half gets there.
        let msgs = scroll_by(&mut ui, &layout, &snap, 0.0, row_h / 2.0);
        assert!(msgs.is_empty(), "under a row, so the list has not moved");
        let msgs = scroll_by(&mut ui, &layout, &snap, 0.0, row_h / 2.0);
        assert!(
            matches!(msgs.as_slice(), [Message::PaletteScrollTo(1)]),
            "the offset is counted from the window on screen, not the gesture"
        );
    }

    #[test]
    fn the_wheel_stops_at_both_ends_of_the_list() {
        let (mut ui, layout, snap) = scrolling_palette_scene();
        let row_h = crate::ui::layout::metrics::PALETTE_ROW_H as f32;
        let palette = layout.palette.as_ref().expect("palette");
        let last = snap.palette.rows.len() - palette.rows.len();

        let msgs = scroll_by(&mut ui, &layout, &snap, 0.0, row_h * 100.0);
        assert!(
            matches!(msgs.as_slice(), [Message::PaletteScrollTo(row)] if *row == last),
            "the end of the list is as far as it goes"
        );
        // Already at the top, so scrolling up asks for nothing at all.
        let msgs = scroll_by(&mut ui, &layout, &snap, 0.0, -row_h * 100.0);
        assert!(msgs.is_empty());
    }

    #[test]
    fn only_the_knob_lights_the_ring_though_the_whole_track_takes_a_click() {
        let (mut ui, layout, snap) = scene();
        let col = layout
            .columns
            .iter()
            .find(|c| c.id == RowId::Sink(1))
            .expect("sink column");
        let thumb = col.slider.thumb;
        let track = col.slider.track;

        // Over the knob: the ring is lit.
        pointer_at(
            &mut ui,
            &layout,
            &snap,
            PointerAction::Motion,
            thumb.x + thumb.w / 2,
            thumb.y + thumb.h / 2,
            0,
        );
        assert_eq!(ui.lit_knob(), Some(RowId::Sink(1)), "the knob lights it");

        // Further up the same track, well clear of the knob: still the
        // slider's hit target, but no ring.
        let above = thumb.y - thumb.h * 2;
        assert!(
            above > track.y,
            "the scene has track above the knob to aim at"
        );
        pointer_at(
            &mut ui,
            &layout,
            &snap,
            PointerAction::Motion,
            track.x + track.w / 2,
            above,
            1,
        );
        assert!(
            matches!(ui.hover, Some(HitTarget::RowSlider(_))),
            "the track is still what a click would land on",
        );
        assert_eq!(ui.lit_knob(), None, "but the ring stays dark");
    }

    /// Both shells report the pointer leaving the window as a motion to a point
    /// outside it, so that one move has to put every hover-driven thing back to
    /// rest. Without it the last thing hovered keeps its highlight and its ring
    /// after the pointer is gone.
    #[test]
    fn a_motion_outside_the_window_clears_every_hover() {
        let (mut ui, layout, snap) = scene();
        let col = layout
            .columns
            .iter()
            .find(|c| c.id == RowId::Sink(1))
            .expect("sink column");
        let thumb = col.slider.thumb;

        pointer_at(
            &mut ui,
            &layout,
            &snap,
            PointerAction::Motion,
            thumb.x + thumb.w / 2,
            thumb.y + thumb.h / 2,
            0,
        );
        assert!(ui.hover.is_some());
        assert!(ui.knob_hover.is_some());

        pointer_at(&mut ui, &layout, &snap, PointerAction::Motion, -1, -1, 1);
        assert_eq!(ui.hover, None, "nothing is hovered once the pointer is out");
        assert_eq!(ui.knob_hover, None, "and no knob keeps its ring");
        assert_eq!(ui.lit_knob(), None);
    }

    #[test]
    fn a_drag_keeps_the_ring_lit_after_the_pointer_leaves_the_knob() {
        let (mut ui, layout, snap) = scene();
        let col = layout
            .columns
            .iter()
            .find(|c| c.id == RowId::Sink(1))
            .expect("sink column");
        let thumb = col.slider.thumb;

        press_at(
            &mut ui,
            &layout,
            &snap,
            thumb.x + thumb.w / 2,
            thumb.y + thumb.h / 2,
        );
        // Dragging past the knob keeps hold of it, so the ring stays.
        pointer_at(
            &mut ui,
            &layout,
            &snap,
            PointerAction::Motion,
            thumb.x + thumb.w / 2,
            col.slider.track.y,
            1,
        );
        assert_eq!(
            ui.lit_knob(),
            Some(RowId::Sink(1)),
            "the grab holds the ring even off the knob",
        );
    }

    #[test]
    fn pressing_a_mute_button_emits_mute_toggled() {
        let (mut ui, layout, snap) = scene();
        let col = layout
            .columns
            .iter()
            .find(|c| c.id == RowId::Sink(1))
            .expect("sink column");
        let msgs = press_at(
            &mut ui,
            &layout,
            &snap,
            col.mute.x + col.mute.w / 2,
            col.mute.y + col.mute.h / 2,
        );
        assert!(matches!(msgs.as_slice(), [Message::MuteToggled(1)]));
    }

    #[test]
    fn pressing_a_slider_starts_a_drag_and_sets_volume() {
        let (mut ui, layout, snap) = scene();
        let col = layout
            .columns
            .iter()
            .find(|c| c.id == RowId::Sink(1))
            .expect("sink column");
        // Press at the very top of the track: near maximum volume.
        let msgs = press_at(
            &mut ui,
            &layout,
            &snap,
            col.slider.track.x + col.slider.track.w / 2,
            col.slider.track.y,
        );
        assert!(matches!(ui.drag, Some(Drag::Slider(RowId::Sink(1)))));
        match msgs.as_slice() {
            [Message::VolumeChanged(1, v)] => assert!(*v > 1.4, "top is near max, got {v}"),
            other => panic!("expected VolumeChanged, got {other:?}"),
        }
    }

    #[test]
    fn ctrl_k_toggles_the_palette_and_takes_focus() {
        let (mut ui, _layout, snap) = scene();
        let msgs = on_key(
            &mut ui,
            &snap,
            KeyEvent {
                key: Key::Char('k'),
                mods: Modifiers {
                    ctrl: true,
                    ..Default::default()
                },
            },
        );
        assert_eq!(ui.focus, Focus::Palette);
        assert!(matches!(msgs.as_slice(), [Message::TogglePalette]));
    }

    /// A scene with the palette open, so the overlay geometry exists.
    fn palette_scene(query: &str) -> (UiState, Layout, ViewSnapshot) {
        let mut app = state::empty();
        app.palette_open = true;
        app.palette_query = query.to_string();
        let snap = build_snapshot(&app, |_| None);
        let mut ui = UiState::new();
        ui.focus = Focus::Palette;
        ui.editor.set_text(query);
        let layout = crate::ui::layout::project(&snap, &ui, Rect::new(0, 0, 560, 720));
        (ui, layout, snap)
    }

    #[test]
    fn clicking_in_a_text_field_puts_the_caret_under_the_pointer() {
        let (mut ui, layout, snap) = palette_scene("hello world");
        let field = layout.focused_field(Focus::Palette).expect("palette field");
        let f = font();
        // Land halfway through the text and check the caret follows.
        let target = 5;
        let x = field.x
            + crate::ui::layout::metrics::FIELD_PAD
            + f.x_at_char_offset("hello world", target, metrics_size()) as i32;
        press_at(&mut ui, &layout, &snap, x, field.y + field.h / 2);
        assert_eq!(ui.editor.cursor(), target);
        assert_eq!(
            ui.editor.selection(),
            None,
            "a single click selects nothing"
        );
        assert_eq!(
            ui.drag,
            Some(Drag::TextSelect),
            "the press begins a selection"
        );
    }

    #[test]
    fn dragging_in_a_text_field_extends_the_selection() {
        let (mut ui, layout, snap) = palette_scene("hello world");
        let field = layout.focused_field(Focus::Palette).expect("palette field");
        let f = font();
        let at = |n| {
            field.x
                + crate::ui::layout::metrics::FIELD_PAD
                + f.x_at_char_offset("hello world", n, metrics_size()) as i32
        };
        let y = field.y + field.h / 2;
        press_at(&mut ui, &layout, &snap, at(2), y);
        pointer_at(&mut ui, &layout, &snap, PointerAction::Motion, at(7), y, 10);
        assert_eq!(ui.editor.selection(), Some((2, 7)));
        assert_eq!(ui.editor.selected_text().as_deref(), Some("llo w"));

        // The drag keeps tracking the field after the pointer leaves it.
        pointer_at(
            &mut ui,
            &layout,
            &snap,
            PointerAction::Motion,
            at(11) + 400,
            y,
            20,
        );
        assert_eq!(ui.editor.selection(), Some((2, 11)));
    }

    #[test]
    fn a_double_click_in_a_field_selects_the_word() {
        let (mut ui, layout, snap) = palette_scene("hello world");
        let field = layout.focused_field(Focus::Palette).expect("palette field");
        let f = font();
        let x = field.x
            + crate::ui::layout::metrics::FIELD_PAD
            + f.x_at_char_offset("hello world", 8, metrics_size()) as i32;
        let y = field.y + field.h / 2;
        let press = PointerAction::Press(MouseButton::Left);
        pointer_at(&mut ui, &layout, &snap, press, x, y, 0);
        pointer_at(&mut ui, &layout, &snap, press, x, y, 100);
        assert_eq!(ui.editor.selected_text().as_deref(), Some("world"));
    }

    fn metrics_size() -> f32 {
        crate::ui::layout::metrics::FIELD_TEXT_SIZE
    }

    /// The dimmed area is not a hole: a press there dismisses rather than
    /// falling through to whatever the overlay is covering.
    #[test]
    fn pressing_the_backdrop_closes_the_overlay_and_hits_nothing_behind_it() {
        let (mut ui, layout, snap) = palette_scene("");
        // Bottom-left corner: inside the content, well clear of the panel.
        let (x, y) = (4, layout.content.bottom() - 4);
        assert_eq!(layout.hit(x, y), Some(&HitTarget::Backdrop));
        let msgs = press_at(&mut ui, &layout, &snap, x, y);
        assert!(matches!(msgs.as_slice(), [Message::TogglePalette]));
        assert_eq!(ui.focus, Focus::Body);
    }

    #[test]
    fn window_targets_map_to_window_actions_and_nothing_else_does() {
        use crate::ui::layout::ResizeEdge;
        assert_eq!(
            window_action(&HitTarget::TitlebarDrag),
            Some(WindowAction::Move)
        );
        assert_eq!(
            window_action(&HitTarget::WindowClose),
            Some(WindowAction::Close)
        );
        assert_eq!(
            window_action(&HitTarget::WindowMaximize),
            Some(WindowAction::ToggleMaximize)
        );
        assert_eq!(
            window_action(&HitTarget::WindowMinimize),
            Some(WindowAction::Minimize)
        );
        assert_eq!(
            window_action(&HitTarget::ResizeEdge(ResizeEdge::BottomRight)),
            Some(WindowAction::Resize(ResizeEdge::BottomRight))
        );
        // Mixer targets are not the window's business.
        assert_eq!(window_action(&HitTarget::MuteAll), None);
        assert_eq!(window_action(&HitTarget::RowMute(RowId::Sink(1))), None);
    }

    /// A scene with the profile dropdown open over two profiles.
    fn menu_scene() -> (UiState, Layout, ViewSnapshot) {
        let mut app = state::empty();
        app.profiles.profiles.clear();
        for name in ["Gaming", "Music"] {
            app.profiles.profiles.push(crate::profile::Profile {
                name: name.to_string(),
                ..Default::default()
            });
        }
        let snap = build_snapshot(&app, |_| None);
        let mut ui = UiState::new();
        ui.profile_menu_open = true;
        let layout = crate::ui::layout::project(&snap, &ui, Rect::new(0, 0, 560, 720));
        (ui, layout, snap)
    }

    /// Pressing a profile can mean either apply or reorder, so nothing happens
    /// until release says which. Released where it started, it applies.
    #[test]
    fn releasing_a_profile_where_it_was_pressed_applies_it() {
        let (mut ui, layout, snap) = menu_scene();
        let row = &layout.profile_menu.as_ref().expect("menu").rows[0].rect;
        let (x, y) = (row.x + 4, row.y + row.h / 2);

        let msgs = press_at(&mut ui, &layout, &snap, x, y);
        assert!(msgs.is_empty(), "the press alone applies nothing");
        assert_eq!(ui.drag, Some(Drag::ProfileReorder("Gaming".into())));

        let msgs = pointer_at(
            &mut ui,
            &layout,
            &snap,
            PointerAction::Release(MouseButton::Left),
            x,
            y,
            10,
        );
        assert!(matches!(
            msgs.as_slice(),
            [Message::ApplyProfile(n)] if n == "Gaming"
        ));
        assert!(!ui.profile_menu_open, "applying closes the dropdown");
    }

    #[test]
    fn dragging_a_profile_onto_another_row_reorders_it() {
        let (mut ui, layout, snap) = menu_scene();
        let menu = layout.profile_menu.as_ref().expect("menu");
        let (from, onto) = (menu.rows[0].rect, menu.rows[1].rect);
        press_at(&mut ui, &layout, &snap, from.x + 4, from.y + from.h / 2);

        // Released on the top half of the second row: it lands before it.
        let msgs = pointer_at(
            &mut ui,
            &layout,
            &snap,
            PointerAction::Release(MouseButton::Left),
            onto.x + 4,
            onto.y + 1,
            10,
        );
        match msgs.as_slice() {
            [
                Message::ReorderProfile {
                    name,
                    target,
                    before,
                },
            ] => {
                assert_eq!(name, "Gaming");
                assert_eq!(target, "Music");
                assert!(*before, "the top half drops before the target");
            }
            other => panic!("expected a reorder, got {other:?}"),
        }

        // The bottom half drops after it.
        let (mut ui, layout, snap) = menu_scene();
        press_at(&mut ui, &layout, &snap, from.x + 4, from.y + from.h / 2);
        let msgs = pointer_at(
            &mut ui,
            &layout,
            &snap,
            PointerAction::Release(MouseButton::Left),
            onto.x + 4,
            onto.bottom() - 1,
            10,
        );
        assert!(matches!(
            msgs.as_slice(),
            [Message::ReorderProfile { before: false, .. }]
        ));
    }

    #[test]
    fn releasing_a_profile_drag_off_the_menu_cancels_it() {
        let (mut ui, layout, snap) = menu_scene();
        let row = layout.profile_menu.as_ref().expect("menu").rows[0].rect;
        press_at(&mut ui, &layout, &snap, row.x + 4, row.y + row.h / 2);
        let msgs = pointer_at(
            &mut ui,
            &layout,
            &snap,
            PointerAction::Release(MouseButton::Left),
            10,
            700,
            10,
        );
        assert!(msgs.is_empty(), "a drag dropped nowhere does nothing");
        assert_eq!(ui.drag, None);
    }

    #[test]
    fn only_ctrl_shift_s_asks_for_a_screenshot() {
        let key = |key, ctrl, shift| KeyEvent {
            key,
            mods: Modifiers {
                ctrl,
                shift,
                alt: false,
            },
        };
        assert!(is_screenshot_key(key(Key::Char('s'), true, true)));
        assert!(is_screenshot_key(key(Key::Char('S'), true, true)));
        assert!(!is_screenshot_key(key(Key::Char('s'), true, false)));
        assert!(!is_screenshot_key(key(Key::Char('s'), false, true)));
        assert!(!is_screenshot_key(key(Key::Char('k'), true, true)));
    }

    #[test]
    fn a_section_filter_press_toggles_that_section() {
        let (mut ui, layout, snap) = scene();
        let hit = layout
            .hits
            .iter()
            .find(|h| matches!(h.target, HitTarget::SectionFilter(Section::Outputs)))
            .expect("outputs filter");
        let msgs = press_at(
            &mut ui,
            &layout,
            &snap,
            hit.rect.x + hit.rect.w / 2,
            hit.rect.y + hit.rect.h / 2,
        );
        assert!(matches!(
            msgs.as_slice(),
            [Message::ToggleSection(Section::Outputs)]
        ));
    }
}
