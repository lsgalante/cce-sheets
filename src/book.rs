//! The open workbook: IronCalc's `UserModel` plus where it came from.
//!
//! IronCalc reads and writes .xlsx itself. Delimited text (.csv, .tsv) is
//! read here: each field goes through the engine's own input parsing, as if
//! typed, so `3.5` is a number, `TRUE` a boolean and `=A1*2` a formula.
//!
//! Saving writes the format the target's extension names. Every save goes to
//! a temporary file beside the target and is renamed over it, so a failed
//! save never leaves a truncated workbook. Delimited text holds one sheet's
//! values: saving to it writes the selected sheet as shown, and says so when
//! that leaves formulas or other sheets behind.

use std::fs::File;
use std::io::{BufWriter, Write};
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

    /// The format a save to `path` writes: its extension's, except that a
    /// delimited file saved back over itself keeps the separator it was read
    /// with (a semicolon file stays one).
    pub fn kind_for(&self, path: &Path) -> Result<FileKind, String> {
        if let (FileKind::Delimited(d), Some(own)) = (self.kind, &self.path) {
            if own == path {
                return Ok(FileKind::Delimited(d));
            }
        }
        let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
        match ext.as_str() {
            "xlsx" => Ok(FileKind::Xlsx),
            "csv" => Ok(FileKind::Delimited(b',')),
            "tsv" | "tab" => Ok(FileKind::Delimited(b'\t')),
            _ => Err("save as .xlsx, .csv or .tsv".to_string()),
        }
    }

    /// Write the workbook to `path`, which becomes its file. `Ok(Some(note))`
    /// when the format kept less than the workbook holds.
    pub fn save_to(&mut self, path: &Path) -> Result<Option<String>, String> {
        let kind = self.kind_for(path)?;
        let model = self.model.get_model();
        let note = match kind {
            FileKind::Xlsx | FileKind::Untitled => {
                write_atomic(path, |w| {
                    ironcalc::export::save_xlsx_to_writer(model, w).map(|_| ()).map_err(|e| e.user_message())
                })?;
                None
            }
            FileKind::Delimited(delim) => {
                let sheet = self.model.get_selected_sheet();
                write_atomic(path, |w| write_delimited(model, sheet, delim, w))?;
                delimited_losses(model, sheet)
            }
        };
        self.path = Some(path.to_path_buf());
        self.kind = kind;
        Ok(note)
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

/// One sheet as delimited text: every row and column up to the last used
/// one, each cell as it is shown.
pub fn write_delimited<W: Write>(model: &Model, sheet: u32, delim: u8, w: W) -> Result<(), String> {
    let ws = model.workbook.worksheets.get(sheet as usize).ok_or("no such sheet")?;
    let mut out = csv::WriterBuilder::new().delimiter(delim).flexible(true).from_writer(w);
    if !ws.sheet_data.is_empty() {
        let d = ws.dimension();
        for row in 1..=d.max_row {
            let record: Vec<String> = (1..=d.max_column).map(|col| model.get_formatted_cell_value(sheet, row, col).unwrap_or_default()).collect();
            out.write_record(&record).map_err(|e| e.to_string())?;
        }
    }
    out.flush().map_err(|e| e.to_string())
}

/// What saving `sheet` as delimited text leaves behind, said plainly.
fn delimited_losses(model: &Model, sheet: u32) -> Option<String> {
    let sheets = model.workbook.worksheets.len();
    let formulas = model.workbook.worksheets[sheet as usize].sheet_data.values().flat_map(|r| r.values()).any(|c| c.has_formula());
    match (formulas, sheets > 1) {
        (false, false) => None,
        (true, false) => Some("saved values only: formulas are not kept in this format".to_string()),
        (false, true) => Some("saved this sheet only: the format holds one".to_string()),
        (true, true) => Some("saved this sheet's values only: formulas and other sheets are not kept".to_string()),
    }
}

/// Write `path` through a temporary file beside it, synced and renamed over
/// the target, so the old file stands until the new one is whole.
fn write_atomic(path: &Path, write: impl FnOnce(&mut BufWriter<File>) -> Result<(), String>) -> Result<(), String> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name().and_then(|n| n.to_str()).ok_or("no file name")?;
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let file = File::create(&tmp).map_err(|e| e.to_string())?;
        let mut w = BufWriter::new(file);
        write(&mut w)?;
        let file = w.into_inner().map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Where a workbook with unsaved changes is kept when the window closes:
/// `$XDG_STATE_HOME/cce-sheets/recovered/<name>-<unix seconds>.xlsx`.
pub fn recovery_path(name: &str, now_secs: u64) -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    let stem = Path::new(name).file_stem().and_then(|s| s.to_str()).unwrap_or("Untitled");
    Some(state.join("cce-sheets/recovered").join(format!("{stem}-{now_secs}.xlsx")))
}

/// Save the workbook to a recovery file; where it went, or why not.
pub fn save_recovery(book: &Book) -> Result<PathBuf, String> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let path = recovery_path(&book.display_name(), now).ok_or("no state directory")?;
    std::fs::create_dir_all(path.parent().ok_or("no parent")?).map_err(|e| e.to_string())?;
    write_atomic(&path, |w| ironcalc::export::save_xlsx_to_writer(book.model.get_model(), w).map(|_| ()).map_err(|e| e.user_message()))?;
    Ok(path)
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

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cce-sheets-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn xlsx_saves_over_itself_and_round_trips() {
        let path = scratch("round.xlsx");
        let mut book = Book::untitled();
        book.model.set_user_input(0, 1, 1, "7").unwrap();
        book.model.set_user_input(0, 2, 1, "=A1*6").unwrap();
        assert_eq!(book.save_to(&path).unwrap(), None);
        // Again, over the existing file (IronCalc's own save refuses that).
        book.model.set_user_input(0, 1, 1, "8").unwrap();
        book.save_to(&path).unwrap();
        let back = Book::open(&path).unwrap();
        assert_eq!(back.model.get_formatted_cell_value(0, 2, 1).unwrap(), "48");
        assert_eq!(back.model.get_cell_content(0, 2, 1).unwrap(), "=A1*6");
        assert_eq!(book.kind, FileKind::Xlsx);
        let dir = path.parent().unwrap();
        assert!(std::fs::read_dir(dir).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".tmp")));
    }

    #[test]
    fn delimited_saves_keep_the_separator_and_say_what_they_drop() {
        let path = scratch("semi.csv");
        std::fs::write(&path, "a;b\n1;2\n").unwrap();
        let mut book = Book::open(&path).unwrap();
        assert_eq!(book.kind, FileKind::Delimited(b';'));
        book.model.set_user_input(0, 3, 1, "=A2+B2").unwrap();
        let note = book.save_to(&path).unwrap();
        assert!(note.unwrap().contains("formulas"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a;b\n1;2\n3;\n");
        // A different .csv path is comma-separated.
        assert_eq!(book.kind_for(&scratch("other.csv")).unwrap(), FileKind::Delimited(b','));
        assert!(book.kind_for(&scratch("x.ods")).is_err());
    }

    #[test]
    fn recovery_lands_under_the_state_dir() {
        let p = recovery_path("budget.xlsx", 42).unwrap();
        assert!(p.ends_with("cce-sheets/recovered/budget-42.xlsx"));
    }

    #[test]
    fn sheet_names_are_made_legal() {
        assert_eq!(sheet_name_from("Q3 [draft]: sales/eu"), "Q3 draft saleseu");
        assert_eq!(sheet_name_from("[]"), "Sheet1");
        assert_eq!(sheet_name_from(&"x".repeat(40)).len(), 31);
    }
}
