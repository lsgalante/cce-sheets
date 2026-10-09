//! The open workbook: IronCalc's `UserModel` plus where it came from.
//!
//! IronCalc reads and writes .xlsx itself. Delimited text (.csv, .tsv) is
//! read here: each field goes through the engine's own input parsing, as if
//! typed, so `3.5` is a number, `TRUE` a boolean and `=A1*2` a formula.

use std::path::{Path, PathBuf};

use ironcalc::base::{Model, UserModel};

/// Locale, timezone and formula language every workbook is opened with.
pub const LOCALE: &str = "en";
pub const TIMEZONE: &str = "UTC";
pub const LANGUAGE: &str = "en";

/// Extensions the app opens, for the file dialog.
pub const SHEET_EXTS: &[&str] = &["xlsx", "csv", "tsv", "tab"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// Not from a file.
    Untitled,
    Xlsx,
    /// Delimited text with this field separator.
    Delimited(u8),
}

pub struct Book {
    pub model: UserModel<'static>,
    pub path: Option<PathBuf>,
    /// What Save writes back (milestone 2).
    #[allow(dead_code)]
    pub kind: FileKind,
}

impl Book {
    pub fn untitled() -> Book {
        let model = UserModel::new_empty("Untitled", LOCALE, TIMEZONE, LANGUAGE).expect("an empty workbook always builds");
        Book { model, path: None, kind: FileKind::Untitled }
    }

    pub fn open(path: &Path) -> Result<Book, String> {
        let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
        let (model, kind) = match ext.as_str() {
            "xlsx" => {
                let p = path.to_str().ok_or("path is not UTF-8")?;
                let m = ironcalc::import::load_from_xlsx(p, LOCALE, TIMEZONE, LANGUAGE).map_err(|e| e.user_message())?;
                (m, FileKind::Xlsx)
            }
            _ => {
                let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
                let delim = match ext.as_str() {
                    "tsv" | "tab" => b'\t',
                    _ => sniff_delimiter(&bytes),
                };
                let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Sheet1");
                (load_delimited(&bytes, delim, name)?, FileKind::Delimited(delim))
            }
        };
        Ok(Book { model: UserModel::from_model(model), path: Some(path.to_path_buf()), kind })
    }

    /// The file name for the title bar.
    pub fn display_name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| "Untitled".to_string())
    }
}

/// The separator a delimited file most likely uses: whichever of comma, tab
/// and semicolon appears most often, outside quotes, on the first line.
pub fn sniff_delimiter(bytes: &[u8]) -> u8 {
    let mut counts = [(b',', 0usize), (b'\t', 0), (b';', 0)];
    let mut quoted = false;
    for &b in bytes.iter().take(64 * 1024) {
        match b {
            b'"' => quoted = !quoted,
            b'\n' if !quoted => break,
            _ if !quoted => {
                if let Some(c) = counts.iter_mut().find(|(d, _)| *d == b) {
                    c.1 += 1;
                }
            }
            _ => {}
        }
    }
    // Ties go to the comma: it is first, and max_by_key keeps the last max.
    counts.iter().rev().max_by_key(|(_, n)| *n).filter(|(_, n)| *n > 0).map_or(b',', |(d, _)| *d)
}

/// A one-sheet workbook from delimited text, the sheet named after `name`.
pub fn load_delimited(bytes: &[u8], delim: u8, name: &str) -> Result<Model<'static>, String> {
    // The engine borrows its constructor's strings for the model's life, so
    // build from 'static ones and name the workbook after.
    let mut model = Model::new_empty("Untitled", LOCALE, TIMEZONE, LANGUAGE)?;
    model.workbook.name = name.to_string();
    let sheet_name = sheet_name_from(name);
    if sheet_name != "Sheet1" {
        model.rename_sheet_by_index(0, &sheet_name)?;
    }
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delim)
        .has_headers(false)
        .flexible(true)
        .from_reader(bytes);
    let mut record = csv::ByteRecord::new();
    let mut row = 0i32;
    while reader.read_byte_record(&mut record).map_err(|e| e.to_string())? {
        row += 1;
        for (i, field) in record.iter().enumerate() {
            if field.is_empty() {
                continue;
            }
            let value = String::from_utf8_lossy(field).into_owned();
            model.set_user_input(0, row, i as i32 + 1, value)?;
        }
    }
    model.evaluate();
    Ok(model)
}

/// A legal sheet name from a file stem: at most 31 characters, none of
/// `[]:*?/\`, not empty.
fn sheet_name_from(stem: &str) -> String {
    let cleaned: String = stem.chars().filter(|c| !"[]:*?/\\".contains(*c)).take(31).collect();
    let cleaned = cleaned.trim().trim_matches('\'').to_string();
    if cleaned.is_empty() {
        "Sheet1".to_string()
    } else {
        cleaned
    }
}

/// `1` → `A`, `27` → `AA`, `16384` → `XFD`.
pub fn column_name(mut col: i32) -> String {
    let mut out = Vec::new();
    while col > 0 {
        let rem = (col - 1) % 26;
        out.push(b'A' + rem as u8);
        col = (col - 1) / 26;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_names() {
        assert_eq!(column_name(1), "A");
        assert_eq!(column_name(26), "Z");
        assert_eq!(column_name(27), "AA");
        assert_eq!(column_name(52), "AZ");
        assert_eq!(column_name(703), "AAA");
        assert_eq!(column_name(16384), "XFD");
    }

    #[test]
    fn sniffs_the_separator_from_the_first_line() {
        assert_eq!(sniff_delimiter(b"a,b,c\n1;2;3;4;5\n"), b',');
        assert_eq!(sniff_delimiter(b"a;b;c\n"), b';');
        assert_eq!(sniff_delimiter(b"a\tb\n"), b'\t');
        // Separators inside quotes do not count.
        assert_eq!(sniff_delimiter(b"\"x;y;z\",b\n"), b',');
        assert_eq!(sniff_delimiter(b"single\n"), b',');
    }

    #[test]
    fn delimited_fields_are_typed_by_the_engine() {
        let m = load_delimited(b"name,qty,price\nfoo,3,2.5\nbar,4,=B3*2\n", b',', "stock").unwrap();
        let v = |r, c| m.get_formatted_cell_value(0, r, c).unwrap();
        assert_eq!(v(1, 1), "name");
        assert_eq!(v(2, 2), "3");
        assert_eq!(v(3, 3), "8");
        assert_eq!(m.get_worksheets_properties()[0].name, "stock");
    }

    #[test]
    fn quoted_fields_keep_their_separators_and_newlines() {
        let m = load_delimited(b"\"a, b\",\"two\nlines\"\n", b',', "q").unwrap();
        assert_eq!(m.get_formatted_cell_value(0, 1, 1).unwrap(), "a, b");
        assert_eq!(m.get_formatted_cell_value(0, 1, 2).unwrap(), "two\nlines");
    }

    #[test]
    fn sheet_names_are_made_legal() {
        assert_eq!(sheet_name_from("Q3 [draft]: sales/eu"), "Q3 draft saleseu");
        assert_eq!(sheet_name_from("[]"), "Sheet1");
        assert_eq!(sheet_name_from(&"x".repeat(40)).len(), 31);
    }
}
