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
//!
//! A Markdown note opens as its tables, one sheet each (`md.rs` finds them),
//! and saving writes each table back over the lines it came from, leaving the
//! rest of the note as it stands on disk. A cell nobody changed is written
//! with its original text, so a `3.10` the engine reads as 3.1 stays `3.10`.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use std::collections::HashMap;

use ironcalc::base::types::{Alignment, HorizontalAlignment};
use ironcalc::base::{Model, UserModel};

use crate::md::{self, MdAlign, MdTable};

/// Locale, timezone and formula language every workbook is opened with.
pub const LOCALE: &str = "en";
pub const TIMEZONE: &str = "UTC";
pub const LANGUAGE: &str = "en";

/// Extensions the app opens, for the file dialog: workbooks, delimited
/// text, and Markdown notes (their tables).
pub const SHEET_EXTS: &[&str] = &["xlsx", "csv", "tsv", "tab", "md"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// Not from a file.
    Untitled,
    Xlsx,
    /// Delimited text with this field separator.
    Delimited(u8),
    /// A Markdown note's tables.
    Markdown,
}

/// What a note's tables were when opened, to write them back.
pub struct MdSource {
    pub tables: Vec<LoadedTable>,
}

pub struct LoadedTable {
    pub table: MdTable,
    pub sheet: u32,
    /// Each cell's text in the note and what the engine made of it, by
    /// (row, column): a cell whose content is still that is written back as
    /// the note had it.
    original: HashMap<(i32, i32), (String, String)>,
}

pub struct Book {
    pub model: UserModel<'static>,
    pub path: Option<PathBuf>,
    pub kind: FileKind,
    /// The tables of a Markdown note, when that is what this is.
    pub md: Option<MdSource>,
}

impl Book {
    pub fn untitled() -> Book {
        let model = UserModel::new_empty("Untitled", LOCALE, TIMEZONE, LANGUAGE).expect("an empty workbook always builds");
        Book { model, path: None, kind: FileKind::Untitled, md: None }
    }

    pub fn open(path: &Path) -> Result<Book, String> {
        let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
        if ext == "md" {
            let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
            let (model, md) = load_markdown(&text)?;
            return Ok(Book { model, path: Some(path.to_path_buf()), kind: FileKind::Markdown, md: Some(md) });
        }
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
        Ok(Book { model: UserModel::from_model(model), path: Some(path.to_path_buf()), kind, md: None })
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
        if self.kind == FileKind::Markdown && self.path.as_deref() == Some(path) {
            return Ok(FileKind::Markdown);
        }
        let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
        match ext.as_str() {
            "xlsx" => Ok(FileKind::Xlsx),
            "csv" => Ok(FileKind::Delimited(b',')),
            "tsv" | "tab" => Ok(FileKind::Delimited(b'\t')),
            "md" => Err("tables are written back only to the note they came from; save as .xlsx, .csv or .tsv".to_string()),
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
            FileKind::Markdown => return self.save_markdown(path),
        };
        self.path = Some(path.to_path_buf());
        self.kind = kind;
        // Saved as a workbook, it is no longer the note's tables.
        self.md = None;
        Ok(note)
    }

    /// Write each table back into the note, as the note stands now.
    fn save_markdown(&mut self, path: &Path) -> Result<Option<String>, String> {
        let src = self.md.as_ref().ok_or("not a note's tables")?;
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let eol = md::eol_of(&text);
        let mut written = Vec::new();
        let mut replacements = Vec::new();
        for t in &src.tables {
            let (rows, align) = table_rows(&self.model, t);
            // A table nobody changed goes back exactly as it was written,
            // not re-padded.
            let new = if rows == t.table.rows && align == t.table.align { t.table.raw.clone() } else { md::render(&rows, &align, eol) };
            written.push((rows, new.clone()));
            replacements.push((t.table.clone(), new));
        }
        let out = md::splice(&text, &replacements)?;
        write_atomic(path, |w| w.write_all(out.as_bytes()).map_err(|e| e.to_string()))?;
        // What the note now holds is what the next save is checked against.
        let now = md::tables(&out);
        let src = self.md.as_mut().expect("checked above");
        for (t, (rows, new)) in src.tables.iter_mut().zip(written) {
            if let Some(nt) = now.iter().find(|x| x.raw == new) {
                t.table = nt.clone();
            }
            t.original = originals(&self.model, t.sheet, &rows);
        }
        Ok(markdown_losses(&self.model, src))
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

/// A note's tables as a workbook, one sheet per table — named after the
/// heading above it — with the header row bold and frozen and each column
/// aligned as its delimiter says. Built on the `Model`, before there is any
/// undo history for this to show up in.
fn load_markdown(text: &str) -> Result<(UserModel<'static>, MdSource), String> {
    let found = md::tables(text);
    if found.is_empty() {
        return Err("this note has no tables".to_string());
    }
    let mut model = Model::new_empty("Untitled", LOCALE, TIMEZONE, LANGUAGE)?;
    let mut names: Vec<String> = Vec::new();
    let mut loaded = Vec::new();
    for (i, t) in found.into_iter().enumerate() {
        let sheet = if i == 0 { 0 } else { model.new_sheet().1 };
        let base = sheet_name_from(t.heading.as_deref().unwrap_or(""));
        let base = if base == "Sheet1" { format!("Table {}", i + 1) } else { base };
        let mut name = base.clone();
        let mut n = 2;
        while names.iter().any(|x| x.eq_ignore_ascii_case(&name)) {
            name = format!("{} {n}", base.chars().take(28).collect::<String>());
            n += 1;
        }
        model.rename_sheet_by_index(sheet, &name)?;
        names.push(name);
        for (r, row) in t.rows.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                if !cell.is_empty() {
                    model.set_user_input(sheet, r as i32 + 1, c as i32 + 1, cell.clone())?;
                }
            }
        }
        for c in 0..t.rows.first().map_or(0, Vec::len) {
            let horizontal = match t.align.get(c).copied().unwrap_or_default() {
                MdAlign::Left => Some(HorizontalAlignment::Left),
                MdAlign::Center => Some(HorizontalAlignment::Center),
                MdAlign::Right => Some(HorizontalAlignment::Right),
                MdAlign::None => None,
            };
            for r in 0..t.rows.len() {
                let mut style = model.get_style_for_cell(sheet, r as i32 + 1, c as i32 + 1).unwrap_or_default();
                if r == 0 {
                    style.font.b = true;
                }
                if let Some(h) = horizontal.clone() {
                    style.alignment = Some(Alignment { horizontal: h, ..Default::default() });
                }
                if r == 0 || horizontal.is_some() {
                    model.set_cell_style(sheet, r as i32 + 1, c as i32 + 1, &style)?;
                }
            }
        }
        model.set_frozen_rows(sheet, 1)?;
        loaded.push((t, sheet));
    }
    model.evaluate();
    let mut user = UserModel::from_model(model);
    user.set_selected_sheet(0)?;
    let tables = loaded
        .into_iter()
        .map(|(table, sheet)| {
            let original = originals(&user, sheet, &table.rows);
            LoadedTable { table, sheet, original }
        })
        .collect();
    Ok((user, MdSource { tables }))
}

/// Each non-empty cell's note text, with the content the engine holds for it.
fn originals(model: &UserModel, sheet: u32, rows: &[Vec<String>]) -> HashMap<(i32, i32), (String, String)> {
    let mut out = HashMap::new();
    for (r, row) in rows.iter().enumerate() {
        for (c, text) in row.iter().enumerate() {
            let (r, c) = (r as i32 + 1, c as i32 + 1);
            if !text.is_empty() {
                out.insert((r, c), (text.clone(), model.get_cell_content(sheet, r, c).unwrap_or_default()));
            }
        }
    }
    out
}

/// A loaded table's sheet as Markdown rows and alignments: the used range,
/// at least the header; unchanged cells as the note had them, the rest as
/// shown; each column's alignment from its header cell.
fn table_rows(model: &UserModel, t: &LoadedTable) -> (Vec<Vec<String>>, Vec<MdAlign>) {
    let ws = &model.get_model().workbook.worksheets[t.sheet as usize];
    let (rows, cols) = if ws.sheet_data.is_empty() {
        (1, t.table.rows.first().map_or(1, Vec::len) as i32)
    } else {
        let d = ws.dimension();
        (d.max_row.max(1), d.max_column.max(1))
    };
    let cell = |r: i32, c: i32| -> String {
        let content = model.get_cell_content(t.sheet, r, c).unwrap_or_default();
        match t.original.get(&(r, c)) {
            Some((text, was)) if *was == content => text.clone(),
            _ => model.get_formatted_cell_value(t.sheet, r, c).unwrap_or_default(),
        }
    };
    let out = (1..=rows).map(|r| (1..=cols).map(|c| cell(r, c)).collect()).collect();
    let align = (1..=cols)
        .map(|c| match model.get_cell_style(t.sheet, 1, c).ok().and_then(|s| s.alignment).map(|a| a.horizontal) {
            Some(HorizontalAlignment::Left) => MdAlign::Left,
            Some(HorizontalAlignment::Center | HorizontalAlignment::CenterContinuous) => MdAlign::Center,
            Some(HorizontalAlignment::Right) => MdAlign::Right,
            _ => MdAlign::None,
        })
        .collect();
    (out, align)
}

/// What writing back to Markdown left out, said plainly.
fn markdown_losses(model: &UserModel, src: &MdSource) -> Option<String> {
    let m = model.get_model();
    let formulas = src.tables.iter().any(|t| m.workbook.worksheets[t.sheet as usize].sheet_data.values().flat_map(|r| r.values()).any(|c| c.has_formula()));
    let extra = m.workbook.worksheets.len() > src.tables.len();
    match (formulas, extra) {
        (false, false) => None,
        (true, false) => Some("formulas were written as their values".to_string()),
        (false, true) => Some("sheets added since opening were not written; save as .xlsx to keep them".to_string()),
        (true, true) => Some("formulas were written as values, and added sheets were not written".to_string()),
    }
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
    fn a_note_opens_as_its_tables_and_saves_back_in_place() {
        let path = scratch("budget.md");
        let note = "# Budget\n\nintro\n\n| Item | Cost |\n|:-----|-----:|\n| Rent | 1200 |\n| Pens | 3.10 |\n\n## Trips\n\n| Where | Days |\n|---|---|\n| Porto | 3 |\n\nend\n";
        std::fs::write(&path, note).unwrap();
        let mut book = Book::open(&path).unwrap();
        assert_eq!(book.kind, FileKind::Markdown);
        let names: Vec<String> = book.model.get_worksheets_properties().into_iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["Budget", "Trips"]);
        assert_eq!(book.model.get_formatted_cell_value(0, 2, 2).unwrap(), "1200");
        assert!(book.model.get_cell_style(0, 1, 1).unwrap().font.b);
        assert_eq!(book.model.get_frozen_rows_count(1).unwrap(), 1);
        // Edit one cell, add a row and a formula.
        book.model.set_user_input(0, 2, 2, "1250").unwrap();
        book.model.set_user_input(0, 4, 1, "Total").unwrap();
        book.model.set_user_input(0, 4, 2, "=SUM(B2:B3)").unwrap();
        // Meanwhile the note's prose changed in another app.
        std::fs::write(&path, note.replace("intro", "intro, edited")).unwrap();
        let saved = book.save_to(&path).unwrap();
        assert_eq!(saved.as_deref(), Some("formulas were written as their values"));
        let out = std::fs::read_to_string(&path).unwrap();
        assert_eq!(book.model.get_formatted_cell_value(0, 4, 2).unwrap(), "1253.1");
        // The prose edit survives, the edited table is rewritten, the other
        // table and everything else are byte for byte as they were.
        assert_eq!(out, "# Budget\n\nintro, edited\n\n| Item  |   Cost |\n| :---- | -----: |\n| Rent  |   1250 |\n| Pens  |   3.10 |\n| Total | 1253.1 |\n\n## Trips\n\n| Where | Days |\n|---|---|\n| Porto | 3 |\n\nend\n");
        // The untouched table kept its own text; a second save finds the new one.
        book.model.set_user_input(1, 2, 2, "4").unwrap();
        book.save_to(&path).unwrap();
        let out2 = std::fs::read_to_string(&path).unwrap();
        assert!(out2.contains("| Porto | 4    |"), "{out2}");
        assert!(out2.contains("| Pens  |   3.10 |"));
    }

    #[test]
    fn a_note_whose_table_changed_elsewhere_is_left_alone() {
        let path = scratch("clash.md");
        std::fs::write(&path, "| a | b |\n|---|---|\n| 1 | 2 |\n").unwrap();
        let mut book = Book::open(&path).unwrap();
        book.model.set_user_input(0, 2, 1, "9").unwrap();
        std::fs::write(&path, "| a | b |\n|---|---|\n| 1 | 5 |\n").unwrap();
        assert!(book.save_to(&path).unwrap_err().contains("changed"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "| a | b |\n|---|---|\n| 1 | 5 |\n");
        assert!(Book::open(&scratch_with("plain.md", "no tables here\n")).is_err());
        // Tables go back only to their own note.
        assert!(book.kind_for(&scratch("other.md")).is_err());
    }

    fn scratch_with(name: &str, text: &str) -> PathBuf {
        let p = scratch(name);
        std::fs::write(&p, text).unwrap();
        p
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
