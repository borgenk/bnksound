//! The text-editing model: the text, the caret, and the selection anchor.
//!
//! Pure logic. Nothing here knows about the renderer, the clipboard, or the
//! shell; the shell turns key presses and clicks into these calls and takes the
//! answers back. That is what makes the editing rules testable, and they need
//! to be: cursor and anchor interact in ways that are easy to get subtly wrong.
//!
//! Positions are char offsets, not byte offsets. Every public boundary speaks
//! chars, because that is what a caret is; bytes appear only where the String
//! is actually indexed.

/// Byte capacity of an editable field (palette query, profile name).
pub const MAX_LEN: usize = 1024;

/// The text being edited, the caret, and where a selection started.
///
/// An anchor equal to the cursor is not a selection: Shift+Left then
/// Shift+Right lands exactly there, and treating it as one would select a
/// character the user never asked for.
#[derive(Default)]
pub struct Editor {
    text: String,
    cursor: usize,
    anchor: Option<usize>,
}

impl Editor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// How many chars the text holds. The caret can sit at any of them, or one
    /// past the last.
    pub fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The selection as (start, end) char offsets, or None when there is none.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        if anchor == self.cursor {
            return None;
        }
        Some((anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    /// The selected text, if any.
    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection()?;
        Some(self.text[self.byte(start)..self.byte(end)].to_string())
    }

    /// Replace the whole text and drop the caret at the end, no selection. Used
    /// to seed the field when an overlay opens with an existing value.
    pub fn set_text(&mut self, s: &str) {
        self.text.clear();
        self.text.push_str(s);
        self.cursor = self.len();
        self.anchor = None;
    }

    /// Clear everything.
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.anchor = None;
    }

    /// Type a character, replacing the selection. Ignored when the field is full:
    /// advancing the caret past text that was never inserted desyncs it, and
    /// every later edit lands in the wrong place.
    pub fn insert(&mut self, ch: char) -> bool {
        self.delete_selection();
        if self.text.len() + ch.len_utf8() > MAX_LEN {
            self.anchor = None;
            return false;
        }
        let at = self.byte(self.cursor);
        self.text.insert(at, ch);
        self.cursor += 1;
        self.anchor = None;
        true
    }

    /// Replace the selection with s, or insert it at the caret. All or nothing:
    /// deleting the selection first and then failing to fit would leave neither
    /// the old text nor the pasted text.
    pub fn paste(&mut self, s: &str) -> bool {
        let selected_bytes = match self.selection() {
            Some((start, end)) => self.byte(end) - self.byte(start),
            None => 0,
        };
        if self.text.len() - selected_bytes + s.len() > MAX_LEN {
            return false;
        }
        self.delete_selection();
        let at = self.byte(self.cursor);
        self.text.insert_str(at, s);
        self.cursor += s.chars().count();
        self.anchor = None;
        true
    }

    /// Delete the selection, or the char before the caret.
    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        // An anchor that took no part in this edit would, once the caret moves
        // off it, read as a one-character selection the next keystroke eats.
        self.anchor = None;
        if self.cursor > 0 {
            self.cursor -= 1;
            let at = self.byte(self.cursor);
            self.text.remove(at);
        }
    }

    /// Delete the selection, or the char after the caret.
    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        self.anchor = None;
        if self.cursor < self.len() {
            let at = self.byte(self.cursor);
            self.text.remove(at);
        }
    }

    /// Delete the selection and put the caret where it started. False when there
    /// was nothing selected.
    pub fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection() else {
            return false;
        };
        self.text
            .replace_range(self.byte(start)..self.byte(end), "");
        self.cursor = start;
        self.anchor = None;
        true
    }

    /// Move the caret one char left. With shift, extend the selection; without
    /// it, a selection collapses to its start rather than moving the caret.
    pub fn left(&mut self, shift: bool) {
        if shift {
            self.anchor.get_or_insert(self.cursor);
        } else if let Some((start, _)) = self.selection() {
            self.cursor = start;
            self.anchor = None;
            return;
        } else {
            self.anchor = None;
        }
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Move the caret one char right, or collapse a selection to its end.
    pub fn right(&mut self, shift: bool) {
        if shift {
            self.anchor.get_or_insert(self.cursor);
        } else if let Some((_, end)) = self.selection() {
            self.cursor = end;
            self.anchor = None;
            return;
        } else {
            self.anchor = None;
        }
        if self.cursor < self.len() {
            self.cursor += 1;
        }
    }

    pub fn home(&mut self, shift: bool) {
        if shift {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = 0;
    }

    pub fn end(&mut self, shift: bool) {
        if shift {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = self.len();
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.len();
    }

    /// A click at a char offset. One click places the caret, two select the word
    /// under it, three or more select everything.
    pub fn click(&mut self, at: usize, count: u32) {
        match count {
            2 => {
                let (start, end) = self.word_at(at);
                self.anchor = Some(start);
                self.cursor = end;
            }
            n if n >= 3 => self.select_all(),
            _ => {
                self.cursor = at.min(self.len());
                self.anchor = None;
            }
        }
    }

    /// A drag to a char offset, extending from wherever the drag began.
    pub fn drag(&mut self, to: usize) {
        self.anchor.get_or_insert(self.cursor);
        self.cursor = to.min(self.len());
    }

    /// The word around a char offset, as (start, end) char offsets. A click in
    /// the run of separators between two words selects that run, matching every
    /// other text field.
    fn word_at(&self, at: usize) -> (usize, usize) {
        let chars: Vec<char> = self.text.chars().collect();
        let len = chars.len();
        if len == 0 {
            return (0, 0);
        }
        let pos = at.min(len - 1);
        let is_word = |c: char| c.is_alphanumeric() || c == '_';

        // Grow both ways over chars of the same kind as the one clicked.
        let want = is_word(chars[pos]);
        let mut start = pos;
        while start > 0 && is_word(chars[start - 1]) == want {
            start -= 1;
        }
        let mut end = pos;
        while end < len && is_word(chars[end]) == want {
            end += 1;
        }
        (start, end)
    }

    /// The byte offset of a char offset. Past the end maps to the byte length,
    /// so a caret one past the last char indexes the end.
    fn byte(&self, char_idx: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_idx)
            .map(|(i, _)| i)
            .unwrap_or(self.text.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(text: &str) -> Editor {
        let mut e = Editor::new();
        for c in text.chars() {
            assert!(e.insert(c), "insert {c}");
        }
        e
    }

    #[test]
    fn typing_appends_and_moves_the_caret() {
        let e = editor("abc");
        assert_eq!(e.text(), "abc");
        assert_eq!(e.cursor(), 3);
        assert_eq!(e.selection(), None);
    }

    #[test]
    fn set_text_seeds_the_field_with_caret_at_end() {
        let mut e = editor("old");
        e.set_text("New Name");
        assert_eq!(e.text(), "New Name");
        assert_eq!(e.cursor(), 8);
        assert_eq!(e.selection(), None);
    }

    #[test]
    fn a_full_field_refuses_the_character_and_leaves_the_caret_alone() {
        let mut e = Editor::new();
        for _ in 0..MAX_LEN {
            assert!(e.insert('x'));
        }
        assert!(!e.insert('y'), "the field is full");
        assert_eq!(e.cursor(), MAX_LEN, "the caret did not run past the text");
        assert_eq!(e.len(), MAX_LEN);
        e.backspace();
        assert_eq!(e.len(), MAX_LEN - 1);
    }

    /// Shift+Left then Shift+Right leaves the anchor sitting on the cursor,
    /// which is not a selection. If backspace left it standing, the caret it
    /// then moves would read as a one-character selection nobody made.
    #[test]
    fn backspace_clears_an_anchor_that_is_not_a_selection() {
        let mut e = editor("abcd");
        e.left(true);
        e.right(true);
        assert_eq!(
            e.selection(),
            None,
            "an anchor on the cursor is no selection"
        );
        e.backspace();
        assert_eq!(e.text(), "abc");
        assert_eq!(e.selection(), None, "no phantom selection is left behind");
        e.insert('x');
        assert_eq!(e.text(), "abcx", "typing did not eat a second character");
    }

    #[test]
    fn backspace_and_delete_take_the_selection_when_there_is_one() {
        let mut e = editor("abcd");
        e.home(false);
        e.right(true);
        e.right(true);
        e.backspace();
        assert_eq!(e.text(), "cd");
        assert_eq!(e.cursor(), 0);

        let mut e = editor("abcd");
        e.select_all();
        e.delete();
        assert!(e.text().is_empty());
    }

    #[test]
    fn a_multibyte_caret_moves_by_character_not_by_byte() {
        let mut e = editor("aéb");
        assert_eq!(e.cursor(), 3);
        e.left(false);
        e.backspace(); // deletes 'é', which is two bytes
        assert_eq!(e.text(), "ab");
        assert_eq!(e.cursor(), 1);
        e.insert('é');
        assert_eq!(e.text(), "aéb");
    }

    #[test]
    fn selection_extends_and_collapses() {
        let mut e = editor("hello");
        e.home(false);
        e.right(true);
        e.right(true);
        assert_eq!(e.selection(), Some((0, 2)));
        assert_eq!(e.selected_text().as_deref(), Some("he"));

        e.left(false);
        assert_eq!(e.selection(), None);
        assert_eq!(e.cursor(), 0);

        e.end(true);
        assert_eq!(e.selection(), Some((0, 5)));
        e.right(false);
        assert_eq!(e.cursor(), 5);
        assert_eq!(e.selection(), None);
    }

    #[test]
    fn typing_over_a_selection_replaces_it() {
        let mut e = editor("abcd");
        e.select_all();
        e.insert('x');
        assert_eq!(e.text(), "x");
        assert_eq!(e.cursor(), 1);
    }

    #[test]
    fn paste_replaces_the_selection() {
        let mut e = editor("abcd");
        e.home(false);
        e.right(true);
        e.right(true);
        assert!(e.paste("XY"));
        assert_eq!(e.text(), "XYcd");
        assert_eq!(e.cursor(), 2);
    }

    #[test]
    fn a_paste_that_does_not_fit_changes_nothing() {
        let mut e = editor("abc");
        e.select_all();
        let huge = "x".repeat(MAX_LEN + 1);
        assert!(!e.paste(&huge));
        assert_eq!(e.text(), "abc", "the selection survives");
        assert_eq!(e.selection(), Some((0, 3)), "and is still selected");
    }

    #[test]
    fn a_double_click_selects_the_word_under_it() {
        let mut e = editor("hello wide world");
        e.click(7, 2);
        assert_eq!(e.selection(), Some((6, 10)));
        assert_eq!(e.selected_text().as_deref(), Some("wide"));
    }

    #[test]
    fn a_triple_click_selects_everything() {
        let mut e = editor("hello world");
        e.click(3, 3);
        assert_eq!(e.selection(), Some((0, 11)));
    }

    #[test]
    fn a_single_click_places_the_caret_and_clears_the_selection() {
        let mut e = editor("hello");
        e.select_all();
        e.click(2, 1);
        assert_eq!(e.cursor(), 2);
        assert_eq!(e.selection(), None);
        e.click(99, 1);
        assert_eq!(e.cursor(), 5);
    }

    #[test]
    fn a_drag_extends_from_where_it_began() {
        let mut e = editor("hello");
        e.click(1, 1);
        e.drag(4);
        assert_eq!(e.selection(), Some((1, 4)));
        assert_eq!(e.selected_text().as_deref(), Some("ell"));
        e.drag(0);
        assert_eq!(e.selection(), Some((0, 1)));
    }

    #[test]
    fn clear_resets_everything() {
        let mut e = editor("abc");
        e.select_all();
        e.clear();
        assert!(e.text().is_empty());
        assert_eq!(e.cursor(), 0);
        assert_eq!(e.selection(), None);
    }
}
