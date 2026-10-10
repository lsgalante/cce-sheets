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
use ironcalc::base::expressions::parser::ExpectedTokens;
use ironcalc::base::UserModel;

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
    /// Function names offered for what is typed at the caret.
    pub completion: Option<Completion>,
    /// Escape closed the list; it stays closed until the text changes.
    pub completion_dismissed: Option<String>,
}

/// The function-name list under the edited formula.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub names: Vec<String>,
    pub pick: usize,
    /// The typed prefix's bytes in the text: `from..to` is replaced.
    pub from: usize,
    pub to: usize,
}

/// How many names the list shows.
pub const COMPLETIONS: usize = 8;

/// The names starting with `prefix` (any case), shortest first, at most
/// [`COMPLETIONS`] — so `SU` offers `SUM` before `SUMPRODUCT`.
pub fn candidates(names: &[&str], prefix: &str) -> Vec<String> {
    let p = prefix.to_uppercase();
    let mut out: Vec<&str> = names.iter().copied().filter(|n| n.to_uppercase().starts_with(&p)).collect();
    out.sort_by(|a, b| a.len().cmp(&b.len()).then(a.cmp(b)));
    out.dedup();
    out.into_iter().take(COMPLETIONS).map(str::to_string).collect()
}

/// What to offer for a formula being edited with the caret at byte
/// `cursor`: the names a function prefix there could be, when the engine
/// says a function name may go there. Not for plain values, an empty
/// prefix, or a name already typed out in full.
pub fn completion(model: &mut UserModel, sheet: u32, row: i32, col: i32, text: &str, cursor: usize, names: &[&str]) -> Option<Completion> {
    if !text.starts_with('=') || !text.is_char_boundary(cursor) {
        return None;
    }
    let chars = text[..cursor].chars().count();
    let ctx = model.formula_completion(sheet, row, col, text, chars).ok()?;
    let prefix = ctx.expecting.iter().find_map(|e| match e {
        ExpectedTokens::FunctionName(p) if !p.is_empty() => Some(p.clone()),
        _ => None,
    })?;
    // The prefix is what sits right before the caret.
    let from = cursor.checked_sub(prefix.len()).filter(|&f| text.is_char_boundary(f) && text[f..cursor].eq_ignore_ascii_case(&prefix))?;
    let names = candidates(names, &prefix);
    if names.is_empty() || (names.len() == 1 && names[0].eq_ignore_ascii_case(&prefix)) {
        return None;
    }
    Some(Completion { names, pick: 0, from, to: cursor })
}

/// `text` with the completion's prefix replaced by `name(`, and the byte the
/// caret goes to: inside the parenthesis. An existing `(` after the caret is
/// kept rather than doubled.
pub fn accept(text: &str, c: &Completion, name: &str) -> (String, usize) {
    let rest = &text[c.to..];
    let paren = if rest.starts_with('(') { "" } else { "(" };
    let out = format!("{}{name}{paren}{rest}", &text[..c.from]);
    let caret = c.from + name.len() + 1;
    (out, caret)
}

impl CellEdit {
    /// An edit of the cell's current content, caret at the end (F2, the bar).
    pub fn of_content(sheet: u32, row: i32, col: i32, content: String, in_bar: bool) -> CellEdit {
        CellEdit { edit: LineEdit::with_text(content), sheet, row, col, typed_over: false, in_bar, cell_shift: 0.0, bar_shift: 0.0, completion: None, completion_dismissed: None }
    }

    /// An empty edit that replaces the cell (typing over it).
    pub fn typed_over(sheet: u32, row: i32, col: i32) -> CellEdit {
        CellEdit { edit: LineEdit::default(), sheet, row, col, typed_over: true, in_bar: false, cell_shift: 0.0, bar_shift: 0.0, completion: None, completion_dismissed: None }
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

    /// The x of byte `byte` of the shown text (text-origin relative).
    pub fn x_of(&self, byte: usize) -> f32 {
        x_at(&self.offsets, byte)
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
    fn completions_offer_functions_for_a_typed_prefix() {
        let lang = ironcalc::base::language::get_language("en").unwrap();
        let names = crate::functions::names(&lang.functions);
        let mut m = UserModel::new_empty("t", "en", "UTC", "en").unwrap();
        let text = "=A1+su";
        let c = completion(&mut m, 0, 1, 1, text, text.len(), &names).unwrap();
        assert_eq!(c.names[0], "SUM");
        assert!(c.names.len() <= COMPLETIONS);
        assert_eq!(&text[c.from..c.to], "su");
        let (out, caret) = accept(text, &c, "SUM");
        assert_eq!(out, "=A1+SUM(");
        assert_eq!(caret, out.len());
        // Not for a plain value, nor right after a complete name's "(".
        assert_eq!(completion(&mut m, 0, 1, 1, "su", 2, &names), None);
        assert_eq!(completion(&mut m, 0, 1, 1, "=SUM(", 5, &names), None);
    }

    #[test]
    fn accepting_keeps_an_existing_parenthesis() {
        let c = Completion { names: vec!["SUM".into()], pick: 0, from: 1, to: 3 };
        assert_eq!(accept("=su(A1)", &c, "SUM"), ("=SUM(A1)".to_string(), 5));
    }

    #[test]
    fn shorter_names_come_first() {
        assert_eq!(candidates(&["SUMPRODUCT", "SUM", "SUMIF", "AVERAGE"], "su"), vec!["SUM", "SUMIF", "SUMPRODUCT"]);
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
