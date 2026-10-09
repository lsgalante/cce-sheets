//! Editing a cell. One [`LineEdit`] holds the text while it is edited; the
//! in-cell editor and the formula bar both show it, and the caret is in
//! whichever the edit was begun in or last pressed.
//!
//! Two modes, as in Excel: typing over a cell (`typed_over`) is *enter* mode,
//! where Left/Right leave the cell — commit, then move — so a run of entries
//! is typed without reaching for Enter; F2, a double-click or a press in the
//! bar is *edit* mode, where Left/Right move the caret. Enter, Tab, Up and
//! Down commit and move in both; Escape puts the cell back.
//!
//! Caret, selection and click positions come from the shaped run the text is
//! drawn with (`engine::shaped_cluster_offsets`), never from a measured
//! width: the measurer reports ink, and the drift grows with every glyph.

use cce_ui::cosmic_text::FontSystem;
use cce_ui::scene::layout::Rect;
use cce_ui::widget::{Key, KeyEvent, LineEdit, NamedKey};

pub struct CellEdit {
    pub edit: LineEdit,
    pub sheet: u32,
    pub row: i32,
    pub col: i32,
    /// Begun by typing over the cell: Left/Right commit and move.
    pub typed_over: bool,
    /// The caret is in the formula bar, not the cell.
    pub in_bar: bool,
    /// How far each field's text is scrolled left to keep the caret in view.
    pub cell_shift: f32,
    pub bar_shift: f32,
}

impl CellEdit {
    /// An edit of the cell's current content, caret at the end (F2, the bar).
    pub fn of_content(sheet: u32, row: i32, col: i32, content: String, in_bar: bool) -> CellEdit {
        CellEdit { edit: LineEdit::with_text(content), sheet, row, col, typed_over: false, in_bar, cell_shift: 0.0, bar_shift: 0.0 }
    }

    /// An empty edit that replaces the cell (typing over it).
    pub fn typed_over(sheet: u32, row: i32, col: i32) -> CellEdit {
        CellEdit { edit: LineEdit::default(), sheet, row, col, typed_over: true, in_bar: false, cell_shift: 0.0, bar_shift: 0.0 }
    }
}

/// What a key does while a cell is being edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKey {
    /// Write the text into the cell, then move the selection as the key does.
    CommitAndMove,
    /// Put the cell back as it was.
    Cancel,
    /// The field's: typing, the caret, the selection.
    Field,
}

pub fn classify(ed: &CellEdit, ev: &KeyEvent) -> EditKey {
    match &ev.logical_key {
        Key::Named(NamedKey::Escape) => EditKey::Cancel,
        Key::Named(NamedKey::Enter | NamedKey::Tab | NamedKey::ArrowUp | NamedKey::ArrowDown) => EditKey::CommitAndMove,
        Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight) if ed.typed_over && !ed.in_bar && !ev.shift && !ev.ctrl => {
            EditKey::CommitAndMove
        }
        _ => EditKey::Field,
    }
}

/// Whether a key, pressed over a selected cell that is not being edited,
/// types into it: printable text with no Ctrl or Alt.
pub fn types(ev: &KeyEvent) -> bool {
    !ev.ctrl && !ev.alt && ev.text.as_deref().is_some_and(|t| !t.is_empty() && !t.chars().any(char::is_control))
}

/// A field's text as drawn, with every boundary's x offset (text-origin
/// relative) and the caret, selection and composition on that run.
pub struct Marks {
    pub shown: String,
    offsets: Vec<(usize, f32)>,
    pub caret: f32,
    pub selection: Option<(f32, f32)>,
    pub composition: Option<(f32, f32)>,
}

impl Marks {
    pub fn of(fs: &mut FontSystem, edit: &LineEdit, size: f32, family: &str) -> Marks {
        let shown = edit.display();
        let offsets = cce_ui::engine::shaped_cluster_offsets(fs, &shown, size, Some(family));
        let x = |b: usize| x_at(&offsets, b);
        let caret = x(edit.display_index(edit.cursor));
        let selection = edit.selection.filter(|&(a, b)| a < b).map(|(a, b)| (x(edit.display_index(a)), x(edit.display_index(b))));
        let composition = edit.composition_range().map(|(a, b)| (x(a), x(b)));
        Marks { shown, offsets, caret, selection, composition }
    }

    /// The whole run's advance.
    pub fn width(&self) -> f32 {
        self.offsets.last().map_or(0.0, |&(_, x)| x)
    }

    /// The byte of the shown text nearest `rel_x` (text-origin relative).
    pub fn byte_at(&self, rel_x: f32) -> usize {
        byte_at(&self.offsets, rel_x)
    }
}

/// The x of byte `byte`: the last boundary at or before it.
pub fn x_at(offsets: &[(usize, f32)], byte: usize) -> f32 {
    offsets.iter().rev().find(|&&(b, _)| b <= byte).map_or(0.0, |&(_, x)| x)
}

/// The boundary nearest `x`.
pub fn byte_at(offsets: &[(usize, f32)], x: f32) -> usize {
    offsets.iter().min_by(|a, b| (a.1 - x).abs().total_cmp(&(b.1 - x).abs())).map_or(0, |&(b, _)| b)
}

/// The scroll that keeps `caret` inside a field `avail` wide, starting from
/// `shift`: unchanged while the caret is in view, else just enough to bring
/// it back; never past the text's end, never negative.
pub fn keep_caret(shift: f32, caret: f32, text_w: f32, avail: f32) -> f32 {
    let avail = avail.max(1.0);
    let mut s = shift;
    if caret - s > avail {
        s = caret - avail;
    }
    if caret < s {
        s = caret;
    }
    s.min((text_w - avail).max(0.0)).max(0.0)
}

/// The in-cell editor's box: the cell, widened to the right to fit the text
/// (as Excel's grows), but no further than `max_right`.
pub fn editor_rect(cell: Rect, text_w: f32, pad: f32, max_right: f32) -> Rect {
    let want = (text_w + 2.0 * pad + 2.0).max(cell.width);
    let width = want.min((max_right - cell.x).max(cell.width));
    Rect { width, ..cell }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cce_ui::widget::ElementState;

    fn key(k: Key, text: Option<&str>) -> KeyEvent {
        KeyEvent { logical_key: k, text: text.map(str::to_string), state: ElementState::Pressed, repeat: false, ctrl: false, shift: false, alt: false }
    }

    #[test]
    fn enter_mode_arrows_leave_the_cell_edit_mode_arrows_move_the_caret() {
        let typed = CellEdit::typed_over(0, 1, 1);
        let f2 = CellEdit::of_content(0, 1, 1, "abc".into(), false);
        let left = key(Key::Named(NamedKey::ArrowLeft), None);
        assert_eq!(classify(&typed, &left), EditKey::CommitAndMove);
        assert_eq!(classify(&f2, &left), EditKey::Field);
        for k in [NamedKey::Enter, NamedKey::Tab, NamedKey::ArrowUp, NamedKey::ArrowDown] {
            assert_eq!(classify(&f2, &key(Key::Named(k), None)), EditKey::CommitAndMove);
        }
        assert_eq!(classify(&typed, &key(Key::Named(NamedKey::Escape), None)), EditKey::Cancel);
        assert_eq!(classify(&typed, &key(Key::Character("x".into()), Some("x"))), EditKey::Field);
        // Shift+Left selects in the field even in enter mode.
        let mut shift_left = left.clone();
        shift_left.shift = true;
        assert_eq!(classify(&typed, &shift_left), EditKey::Field);
    }

    #[test]
    fn only_plain_printable_keys_type_into_a_cell() {
        assert!(types(&key(Key::Character("a".into()), Some("a"))));
        assert!(types(&key(Key::Character("=".into()), Some("="))));
        assert!(types(&key(Key::Named(NamedKey::Space), Some(" "))));
        let mut ctrl_c = key(Key::Character("c".into()), Some("c"));
        ctrl_c.ctrl = true;
        assert!(!types(&ctrl_c));
        assert!(!types(&key(Key::Named(NamedKey::Tab), Some("\t"))));
        assert!(!types(&key(Key::Named(NamedKey::ArrowDown), None)));
    }

    #[test]
    fn offsets_map_both_ways() {
        let offs = [(0, 0.0), (1, 8.0), (2, 16.0), (3, 24.0)];
        assert_eq!(x_at(&offs, 2), 16.0);
        assert_eq!(x_at(&offs, 9), 24.0);
        assert_eq!(byte_at(&offs, 11.0), 1);
        assert_eq!(byte_at(&offs, 13.0), 2);
        assert_eq!(byte_at(&offs, -5.0), 0);
        assert_eq!(byte_at(&offs, 99.0), 3);
    }

    #[test]
    fn the_caret_stays_in_view() {
        // In view: no scroll.
        assert_eq!(keep_caret(0.0, 50.0, 80.0, 100.0), 0.0);
        // Past the right edge: scroll just enough.
        assert_eq!(keep_caret(0.0, 150.0, 200.0, 100.0), 50.0);
        // Back left of the scrolled view: scroll back to it.
        assert_eq!(keep_caret(50.0, 20.0, 200.0, 100.0), 20.0);
        // Text shrank: no scroll past its end.
        assert_eq!(keep_caret(80.0, 60.0, 60.0, 100.0), 0.0);
    }

    #[test]
    fn the_editor_grows_right_to_fit() {
        let cell = Rect { x: 10.0, y: 0.0, width: 90.0, height: 25.0 };
        assert_eq!(editor_rect(cell, 20.0, 3.0, 500.0).width, 90.0);
        assert_eq!(editor_rect(cell, 200.0, 3.0, 500.0).width, 208.0);
        assert_eq!(editor_rect(cell, 900.0, 3.0, 500.0).width, 490.0);
        // A cell wider than the room left keeps its own width.
        assert_eq!(editor_rect(cell, 900.0, 3.0, 50.0).width, 90.0);
    }
}
