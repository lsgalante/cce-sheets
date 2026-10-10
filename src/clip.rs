//! Copy, cut and paste of cells.
//!
//! IronCalc copies a selection as two things: tab-separated text of the
//! values as shown, and the cells themselves (content and style), which it
//! can paste back with references shifted the way a spreadsheet shifts them.
//! The text goes on the system clipboard, for other apps; the cells stay
//! here. A paste whose clipboard text is still the text this app put there
//! pastes the cells; anything else — text copied from another app, or from
//! another window — is pasted as tab-separated values typed in.
//!
//! IronCalc's `Clipboard` keeps its fields to itself but serializes them,
//! which is how its own web frontend carries them across, and how they are
//! read here.

use ironcalc::base::{ClipboardData, UserModel};
use ironcalc::base::expressions::types::Area;

/// Cells copied (or cut) in this window.
pub struct Copied {
    pub sheet: u32,
    /// `(row1, col1, row2, col2)`.
    pub range: (i32, i32, i32, i32),
    /// The text put on the system clipboard.
    pub text: String,
    data: ClipboardData,
    pub cut: bool,
}

/// Copy the model's selected range.
pub fn copy(model: &UserModel, cut: bool) -> Result<Copied, String> {
    let clipboard = model.copy_to_clipboard()?;
    let mut v = serde_json::to_value(&clipboard).map_err(|e| e.to_string())?;
    let field = |v: &mut serde_json::Value, k: &str| v.get_mut(k).map(serde_json::Value::take).ok_or(format!("clipboard has no {k}"));
    let text: String = serde_json::from_value(field(&mut v, "csv")?).map_err(|e| e.to_string())?;
    let sheet: u32 = serde_json::from_value(field(&mut v, "sheet")?).map_err(|e| e.to_string())?;
    let range: (i32, i32, i32, i32) = serde_json::from_value(field(&mut v, "range")?).map_err(|e| e.to_string())?;
    let data: ClipboardData = serde_json::from_value(field(&mut v, "data")?).map_err(|e| e.to_string())?;
    Ok(Copied { sheet, range, text, data, cut })
}

/// What a paste will do.
#[derive(Debug, PartialEq, Eq)]
pub enum Source {
    /// The cells this window copied.
    Cells,
    /// Text from elsewhere.
    Text(String),
    Nothing,
}

/// Which paste: the copied cells while the system clipboard still holds
/// their text (or cannot be read at all), else the clipboard's text.
pub fn source(copied: Option<&Copied>, system: Option<String>) -> Source {
    let same = |t: &str, c: &Copied| normalize(t) == normalize(&c.text);
    match (copied, system) {
        (Some(c), Some(t)) if same(&t, c) => Source::Cells,
        (_, Some(t)) if !t.is_empty() => Source::Text(t),
        (Some(_), None) => Source::Cells,
        _ => Source::Nothing,
    }
}

/// Clipboard text as compared: line endings unified, trailing newlines off
/// (`wl-copy` / `wl-paste` add or keep them by their own rules).
fn normalize(t: &str) -> String {
    t.replace("\r\n", "\n").trim_end_matches(['\n', '\r']).to_string()
}

/// Paste the copied cells at the model's selected cell; the pasted range is
/// selected after.
pub fn paste_cells(model: &mut UserModel, c: &Copied) -> Result<(), String> {
    model.paste_from_clipboard(c.sheet, c.range, &c.data, c.cut)
}

/// Paste tab-separated text at the selected cell, each field typed in.
pub fn paste_text(model: &mut UserModel, text: &str) -> Result<(), String> {
    let view = model.get_selected_view();
    let area = Area { sheet: view.sheet, row: view.row, column: view.column, width: 1, height: 1 };
    model.paste_csv_string(&area, &rectangular(&normalize(text))?)
}

/// Rows of values as the tab-separated text IronCalc's paste reads: quoted
/// where a value holds a tab, quote or newline, and every row padded to the
/// widest. The paste's reader is strict about record lengths and silently
/// SKIPS a row shorter than the first.
pub fn as_tsv(rows: &[Vec<String>]) -> Result<String, String> {
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut w = csv::WriterBuilder::new().delimiter(b'\t').from_writer(vec![]);
    for r in rows {
        let padded = r.iter().cloned().chain(std::iter::repeat_n(String::new(), width - r.len()));
        w.write_record(padded).map_err(|e| e.to_string())?;
    }
    String::from_utf8(w.into_inner().map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// Tab-separated text from anywhere, ragged rows and all, made rectangular
/// for the paste (see [`as_tsv`]).
fn rectangular(text: &str) -> Result<String, String> {
    let mut r = csv::ReaderBuilder::new().delimiter(b'\t').has_headers(false).flexible(true).from_reader(text.as_bytes());
    let rows: Vec<Vec<String>> = r.records().map(|rec| rec.map(|rec| rec.iter().map(str::to_string).collect())).collect::<Result<_, _>>().map_err(|e| e.to_string())?;
    as_tsv(&rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> UserModel<'static> {
        let mut m = UserModel::new_empty("t", "en", "UTC", "en").unwrap();
        m.set_user_input(0, 1, 1, "1").unwrap();
        m.set_user_input(0, 2, 1, "=A1*10").unwrap();
        m
    }

    #[test]
    fn copied_cells_paste_with_shifted_references() {
        let mut m = model();
        m.set_selected_cell(1, 1).unwrap();
        m.set_selected_range(1, 1, 2, 1).unwrap();
        let c = copy(&m, false).unwrap();
        assert_eq!(c.range, (1, 1, 2, 1));
        assert_eq!(c.text, "1\n10");
        m.set_selected_cell(1, 3).unwrap();
        assert_eq!(source(Some(&c), Some("1\n10\n".into())), Source::Cells);
        paste_cells(&mut m, &c).unwrap();
        assert_eq!(m.get_cell_content(0, 2, 3).unwrap(), "=C1*10");
        assert_eq!(m.get_formatted_cell_value(0, 2, 3).unwrap(), "10");
        // A copy leaves its source.
        assert_eq!(m.get_formatted_cell_value(0, 2, 1).unwrap(), "10");
    }

    #[test]
    fn a_cut_moves_the_cells() {
        let mut m = model();
        m.set_selected_cell(1, 1).unwrap();
        m.set_selected_range(1, 1, 2, 1).unwrap();
        let c = copy(&m, true).unwrap();
        m.set_selected_cell(5, 2).unwrap();
        paste_cells(&mut m, &c).unwrap();
        assert_eq!(m.get_formatted_cell_value(0, 1, 1).unwrap(), "");
        assert_eq!(m.get_formatted_cell_value(0, 5, 2).unwrap(), "1");
        assert_eq!(m.get_formatted_cell_value(0, 6, 2).unwrap(), "10");
    }

    #[test]
    fn text_from_elsewhere_is_typed_in() {
        let mut m = model();
        m.set_selected_cell(4, 2).unwrap();
        assert_eq!(source(None, Some("x\t2\r\n3\t=B5+C4\r\n".into())), Source::Text("x\t2\r\n3\t=B5+C4\r\n".into()));
        paste_text(&mut m, "x\t2\r\n3\t=B5+C4\r\n").unwrap();
        assert_eq!(m.get_formatted_cell_value(0, 4, 2).unwrap(), "x");
        assert_eq!(m.get_formatted_cell_value(0, 5, 3).unwrap(), "5");
    }

    #[test]
    fn ragged_text_keeps_every_row() {
        let mut m = model();
        m.set_selected_cell(10, 1).unwrap();
        // Lines with different numbers of fields, as text copied from a
        // document has: none may be dropped.
        paste_text(&mut m, "a\tb\tc\nd\ne\tf\n").unwrap();
        assert_eq!(m.get_formatted_cell_value(0, 11, 1).unwrap(), "d");
        assert_eq!(m.get_formatted_cell_value(0, 12, 2).unwrap(), "f");
    }

    #[test]
    fn the_source_follows_the_system_clipboard() {
        let m = model();
        let c = copy(&m, false).unwrap();
        // Another app copied since: its text wins.
        assert_eq!(source(Some(&c), Some("other".into())), Source::Text("other".into()));
        // The clipboard could not be read: the cells are still ours to paste.
        assert_eq!(source(Some(&c), None), Source::Cells);
        assert_eq!(source(None, None), Source::Nothing);
        assert_eq!(source(None, Some(String::new())), Source::Nothing);
    }
}
