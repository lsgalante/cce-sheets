//! cce-sheets — spreadsheets for the cce desktop.
//!
//! The workbook is IronCalc's `UserModel`, which owns the cells, formulas,
//! styles and the selection; this app draws it and turns input into model
//! calls: open .xlsx / .csv / .tsv, scroll, select, edit cells in place or in
//! the formula bar, undo, and save.
//!
//! Keys: arrows / Enter / Tab move · shift extends · ctrl+arrow jumps to an
//! edge · PageUp/PageDown · ctrl+Home/End · ctrl+PageUp/PageDown change
//! sheet. Typing replaces the selected cell, F2 edits it, Delete clears the
//! selection, Escape abandons an edit; ctrl+z / ctrl+shift+z undo and redo.
//! ctrl+s saves, ctrl+shift+s saves as, ctrl+o opens. Wheel scrolls (shift
//! for horizontal); press, drag and shift+press select; header presses select
//! a whole column or row; a double-click edits.
//!
//! ctrl+c / ctrl+x / ctrl+v copy, cut and paste cells (formulas shift; text
//! from other apps pastes as tab-separated values); ctrl+shift+= and ctrl+-
//! insert and delete rows (columns for a whole-column selection). The square
//! at the selection's corner fills (a series where one shows); header edges
//! resize, and a double-click on one fits it. A right-click opens the cell or
//! header menu.
//!
//! The toolbar under the bar sets the number format, bold / italic /
//! underline (also ctrl+b / ctrl+i / ctrl+u), alignment, text and fill
//! colours and borders on the selection. Typing a function name in a formula
//! offers completions: Up/Down pick, Tab or Enter takes one, Escape closes.
//!
//! Closing with unsaved changes keeps them: `on_exit` writes a recovery copy
//! under `$XDG_STATE_HOME/cce-sheets/recovered/` (the toolkit has no way to
//! ask before a window closes).

mod axis;
mod book;
mod clip;
mod edit;
mod format;
mod functions;
mod grid;
mod menu;
mod ops;
mod toolbar;
mod view;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use cce_ui::engine::{AppSender, Application, CursorIcon, LogicalPosition, LogicalSize, WindowSettings};
use cce_ui::scene::layout::Rect;
use cce_ui::scene::paint::{DisplayList, PaintCtx};
use cce_ui::scene::Material;
use cce_ui::widget::scroll_motion::{Bounds, ScrollMotion};
use cce_ui::widget::{ElementState, Key, KeyEvent, MouseButton, MouseScrollDelta, NamedKey};
use ironcalc::base::expressions::types::Area;
use ironcalc::base::worksheet::NavigationDirection;

use book::{column_name, Book, SHEET_EXTS};
use edit::{CellEdit, EditKey, Marks};
use ops::{FillTo, Line};
use toolbar::Tool;
use grid::{dim, Face, Selection};
use view::{Hit, SheetGeom, View};

const WHEEL_LINE_PX: f32 = 48.0;
/// The formula bar's and the sheet tabs' height.
const BAR_H: f32 = 28.0;
/// Two presses on one cell this close together are a double-click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

#[derive(Debug, Clone)]
enum Message {
    /// A row chosen in the right-click menu, by its id.
    Menu(String),
}

/// A header edge being dragged: the column or row, where the drag began,
/// its size then and now, and the axis as it was (the preview is drawn from
/// it with the new size).
struct Resize {
    line: Line,
    index: i32,
    origin: f32,
    start: f64,
    size: f64,
    base: axis::Axis,
}

/// Side of the fill handle, the square at the selection's corner.
const FILL_HANDLE: f32 = 7.0;

/// Where everything sits in the window, from the last frame; input hit-tests
/// against this.
#[derive(Debug, Clone)]
struct Frame {
    address: Rect,
    content: Rect,
    well: Rect,
    /// Headers and cells, inside the well.
    area: Rect,
    header_w: f32,
    header_h: f32,
    tabs: Vec<(Rect, u32)>,
    /// The in-cell editor's box while a cell is edited in place.
    editor: Option<Rect>,
    /// The fill handle, when the selection's corner is on screen.
    fill_handle: Option<Rect>,
    /// The formatting toolbar's groups.
    tools: Vec<Vec<(Rect, Tool)>>,
    /// The completion list under the edited formula, and its row height.
    popover: Option<(Rect, f32)>,
}

const NO_RECT: Rect = Rect { x: 0.0, y: 0.0, width: 0.0, height: 0.0 };

impl Default for Frame {
    fn default() -> Self {
        Frame { address: NO_RECT, content: NO_RECT, well: NO_RECT, area: NO_RECT, header_w: 0.0, header_h: 0.0, tabs: Vec::new(), editor: None, fill_handle: None, tools: Vec::new(), popover: None }
    }
}

/// A line in the tab row: what a save did, or what went wrong.
struct Status {
    text: String,
    error: bool,
}

struct SheetsApp {
    sender: AppSender<Message>,
    book: Book,
    /// Cells copied or cut here, until pasted (a cut) or let go (Escape).
    copied: Option<clip::Copied>,
    resize: Option<Resize>,
    /// A fill-handle drag, and where it reaches so far.
    fill: Option<Option<FillTo>>,
    /// The last press on a header edge, for telling a double-click.
    last_edge_press: Option<(Instant, Hit)>,
    status: Option<Status>,
    /// Changes since the last save.
    dirty: bool,
    editing: Option<CellEdit>,
    /// A press in an edited field is held: drags select text.
    edit_drag: bool,
    /// The last press on a cell, for telling a double-click.
    last_press: Option<(Instant, i32, i32)>,
    /// Shapes the edited text for its caret and click positions.
    font_system: cce_ui::cosmic_text::FontSystem,
    /// Every worksheet function's name, for formula completion.
    func_names: Vec<&'static str>,
    /// The selected sheet's measurements; rebuilt when the sheet changes.
    geom: SheetGeom,
    geom_sheet: u32,
    scroll: (f64, f64),
    /// Each sheet's scroll, kept while another sheet is shown.
    scrolls: HashMap<u32, (f64, f64)>,
    scroll_motion: ScrollMotion,
    frame: Frame,
    face: Option<Face>,
    win: (f32, f32),
    scale: f64,
    /// A press on the cells is held: drags extend the range.
    selecting: bool,
    ctrl: bool,
    shift: bool,
}

impl SheetsApp {
    fn with_book(book: Book, error: Option<String>, font_system: cce_ui::cosmic_text::FontSystem, sender: AppSender<Message>) -> Self {
        let sheet = book.model.get_selected_sheet();
        let geom = grid::sheet_geom(&book.model, sheet);
        SheetsApp {
            sender,
            book,
            copied: None,
            resize: None,
            fill: None,
            last_edge_press: None,
            status: error.map(|text| Status { text, error: true }),
            dirty: false,
            editing: None,
            edit_drag: false,
            last_press: None,
            font_system,
            func_names: ironcalc::base::language::get_language(book::LANGUAGE).map(|l| functions::names(&l.functions)).unwrap_or_default(),
            geom,
            geom_sheet: sheet,
            scroll: (0.0, 0.0),
            scrolls: HashMap::new(),
            scroll_motion: ScrollMotion::new(),
            frame: Frame::default(),
            face: None,
            win: (1100.0, 760.0),
            scale: 1.0,
            selecting: false,
            ctrl: false,
            shift: false,
        }
    }

    fn open(&mut self, path: &std::path::Path) {
        match Book::open(path) {
            Ok(book) => {
                self.keep_unsaved();
                self.end_edit();
                self.load(book);
            }
            Err(e) => self.set_status(format!("{}: {e}", path.display()), true),
        }
    }

    /// Show `book` in place of the current workbook.
    fn load(&mut self, book: Book) {
        let sheet = book.model.get_selected_sheet();
        self.geom = grid::sheet_geom(&book.model, sheet);
        self.geom_sheet = sheet;
        self.book = book;
        self.copied = None;
        self.status = None;
        self.dirty = false;
        self.last_press = None;
        self.scroll = (0.0, 0.0);
        self.scrolls.clear();
        self.scroll_motion = ScrollMotion::new();
    }

    fn set_status(&mut self, text: String, error: bool) {
        if error {
            log::warn!("{text}");
        }
        self.status = Some(Status { text, error });
    }

    /// Before the workbook is let go with unsaved changes (a close, another
    /// file opened over it): write a recovery copy and say where.
    fn keep_unsaved(&mut self) {
        if !self.dirty {
            return;
        }
        match book::save_recovery(&self.book) {
            Ok(p) => log::warn!("unsaved changes to {} kept in {}", self.book.display_name(), p.display()),
            Err(e) => log::error!("unsaved changes to {} could not be kept: {e}", self.book.display_name()),
        }
    }

    /// After the model changed: mark it unsaved, drop the engine's
    /// collaboration queue (nothing here sends it), and re-read sizes.
    fn changed(&mut self) {
        self.dirty = true;
        let _ = self.book.model.flush_send_queue();
        let sheet = self.sheet();
        self.geom = grid::sheet_geom(&self.book.model, sheet);
        self.geom_sheet = sheet;
    }

    // ---- editing ----

    fn begin_edit(&mut self, ed: CellEdit) {
        self.status = None;
        self.editing = Some(ed);
    }

    /// The selected cell, edited with its content (F2, a double-click, the bar).
    fn edit_selected(&mut self, in_bar: bool) {
        let sel = self.selection();
        let sheet = self.sheet();
        let content = self.book.model.get_cell_content(sheet, sel.row, sel.col).unwrap_or_default();
        self.begin_edit(CellEdit::of_content(sheet, sel.row, sel.col, content, in_bar));
    }

    /// End an edit without writing it, letting go of the input method.
    fn end_edit(&mut self) -> Option<CellEdit> {
        let mut ed = self.editing.take()?;
        ed.edit.drop_composition();
        if cce_ui::ime::preedit().is_some() {
            cce_ui::ime::request_reset();
        }
        self.edit_drag = false;
        Some(ed)
    }

    /// Write the edit into its cell. False, with the edit still open and the
    /// reason shown, when the engine refuses it.
    fn commit_edit(&mut self) -> bool {
        let Some(ed) = self.editing.as_ref() else {
            return true;
        };
        let (sheet, row, col) = (ed.sheet, ed.row, ed.col);
        let text = ed.edit.text.clone();
        let before = self.book.model.get_cell_content(sheet, row, col).unwrap_or_default();
        if text != before {
            if let Err(e) = self.book.model.set_user_input(sheet, row, col, &text) {
                self.set_status(e, true);
                return false;
            }
            self.changed();
        }
        self.end_edit();
        true
    }

    /// Delete: clear the selected range's contents, keeping formats. Clamped
    /// to the used range, since a whole column is a million rows.
    fn clear_selection(&mut self) {
        let sel = self.selection();
        let sheet = self.sheet();
        let (used_r, used_c) = grid::used_extent(&self.book.model, sheet);
        let [a, b, c, d] = sel.range;
        let (r1, c1, r2, c2) = (a.min(c), b.min(d), a.max(c).min(used_r), b.max(d).min(used_c));
        if r2 < r1 || c2 < c1 {
            return;
        }
        let area = Area { sheet, row: r1, column: c1, width: c2 - c1 + 1, height: r2 - r1 + 1 };
        match self.book.model.range_clear_contents(&area) {
            Ok(()) => self.changed(),
            Err(e) => self.set_status(e, true),
        }
    }

    /// Marks for the edited text in the cell font, if a cell is being edited.
    fn edit_marks(&mut self) -> Option<Marks> {
        let face = self.face.as_ref()?;
        let ed = self.editing.as_ref()?;
        Some(Marks::of(&mut self.font_system, &ed.edit, face.size, &face.family))
    }

    /// The byte of the edited text under `x` in the cell editor or the bar.
    fn edit_byte_at(&mut self, x: f32, in_bar: bool) -> Option<usize> {
        let marks = self.edit_marks()?;
        let ed = self.editing.as_ref()?;
        let origin = if in_bar {
            self.frame.content.x + cce_ui::layout::CONTROL_TEXT_INSET - ed.bar_shift
        } else {
            self.frame.editor?.x + grid::CELL_PAD - ed.cell_shift
        };
        let shown = marks.byte_at(x - origin);
        Some(ed.edit.text_index(shown))
    }

    // ---- files ----

    /// ctrl+s: to the file it came from; with no file, or `choose`, to one
    /// picked in the save dialog.
    fn save(&mut self, choose: bool) {
        if !self.commit_edit() {
            return;
        }
        let target = match (&self.book.path, choose) {
            (Some(p), false) if self.book.kind != book::FileKind::Untitled => Some(p.clone()),
            _ => {
                let filters: &[(&str, &[&str])] = &[("Excel workbook", &["xlsx"]), ("Comma-separated", &["csv"]), ("Tab-separated", &["tsv"])];
                cce_ui::file_dialog::save_file("Save", filters).map(|p| if p.extension().is_none() { p.with_extension("xlsx") } else { p })
            }
        };
        let Some(path) = target else {
            return;
        };
        match self.book.save_to(&path) {
            Ok(note) => {
                self.dirty = false;
                let name = self.book.display_name();
                let text = match note {
                    Some(n) => format!("{name}: {n}"),
                    None => format!("saved {name}"),
                };
                self.set_status(text, false);
            }
            Err(e) => self.set_status(format!("{}: {e}", path.display()), true),
        }
    }

    fn open_dialog(&mut self) {
        if !self.commit_edit() {
            return;
        }
        let filters: &[(&str, &[&str])] = &[("Spreadsheets", SHEET_EXTS)];
        if let Some(path) = cce_ui::file_dialog::pick_file("Open", filters) {
            self.open(&path);
        }
    }

    fn sheet(&self) -> u32 {
        self.book.model.get_selected_sheet()
    }

    fn selection(&self) -> Selection {
        let v = self.book.model.get_selected_view();
        Selection { row: v.row, col: v.column, range: v.range }
    }

    /// Rebuild the measurements if the selected sheet changed, swapping the
    /// scroll for the new sheet's.
    fn sync_sheet(&mut self) {
        let sheet = self.sheet();
        if sheet != self.geom_sheet {
            self.scrolls.insert(self.geom_sheet, self.scroll);
            self.geom = grid::sheet_geom(&self.book.model, sheet);
            self.geom_sheet = sheet;
            self.scroll = self.scrolls.get(&sheet).copied().unwrap_or((0.0, 0.0));
            self.scroll_motion = ScrollMotion::new();
        }
    }

    fn view(&self) -> View<'_> {
        View {
            geom: &self.geom,
            area: self.frame.area,
            header_w: self.frame.header_w,
            header_h: self.frame.header_h,
            scroll: self.scroll,
        }
    }

    /// How far each axis scrolls: to the used range or the selection,
    /// whichever reaches further, plus half a screen of empty sheet.
    fn scroll_bounds(&self) -> (f64, f64) {
        let view = self.view();
        let (vw, vh) = view.scroll_viewport();
        let (used_r, used_c) = grid::used_extent(&self.book.model, self.sheet());
        let sel = self.selection();
        let g = &self.geom;
        let span_x = g.cols.end(used_c.max(sel.col)) - g.frozen_w();
        let span_y = g.rows.end(used_r.max(sel.row)) - g.frozen_h();
        let max_x = ((span_x - vw).max(0.0) + vw * 0.5).min(g.cols.end(g.cols.last()) - g.frozen_w() - vw).max(0.0);
        let max_y = ((span_y - vh).max(0.0) + vh * 0.5).min(g.rows.end(g.rows.last()) - g.frozen_h() - vh).max(0.0);
        (max_x, max_y)
    }

    fn clamp_scroll(&mut self) {
        let (mx, my) = self.scroll_bounds();
        self.scroll = (self.scroll.0.clamp(0.0, mx), self.scroll.1.clamp(0.0, my));
    }

    /// After the selection moves: bring the selected cell into view.
    fn reveal_selection(&mut self) {
        self.sync_sheet();
        let sel = self.selection();
        self.scroll = self.view().scroll_to_show(sel.row, sel.col);
        self.clamp_scroll();
    }

    fn tick_scroll(&mut self, dt: f32) -> bool {
        self.scroll_motion.reconcile(self.scroll.0 as f32, self.scroll.1 as f32);
        if !self.scroll_motion.is_animating() {
            return false;
        }
        let (mx, my) = self.scroll_bounds();
        let moved = self.scroll_motion.tick(dt, Bounds::max(mx as f32), Bounds::max(my as f32));
        self.scroll = (self.scroll_motion.x.pos() as f64, self.scroll_motion.y.pos() as f64);
        moved || self.scroll_motion.is_animating()
    }

    /// Rows that fit the scrolled viewport, for PageUp/PageDown.
    fn page_rows(&self) -> i32 {
        let (_, vh) = self.view().scroll_viewport();
        let dh = self.geom.rows.size(self.geom.rows.last()).max(1.0);
        ((vh / dh).floor() as i32 - 1).max(1)
    }

    /// The visible sheets in workbook order, with their indices.
    fn visible_sheets(&self) -> Vec<(u32, String)> {
        self.book
            .model
            .get_worksheets_properties()
            .into_iter()
            .enumerate()
            .filter(|(_, p)| p.state == "visible")
            .map(|(i, p)| (i as u32, p.name))
            .collect()
    }

    fn step_sheet(&mut self, step: i32) {
        let sheets = self.visible_sheets();
        let cur = self.sheet();
        if let Some(pos) = sheets.iter().position(|(i, _)| *i == cur) {
            let next = (pos as i32 + step).clamp(0, sheets.len() as i32 - 1) as usize;
            let _ = self.book.model.set_selected_sheet(sheets[next].0);
            self.sync_sheet();
        }
    }

    /// Lay the window out: the formula bar on top, the sheet tabs at the
    /// bottom, the grid's well between.
    fn layout(&mut self, w: f32, h: f32) {
        let face = self.face.as_ref().expect("face is set before layout");
        let inset = cce_ui::layout::root_plate_inset();
        let gap = cce_ui::layout::root_plate_gap();
        let cgap = cce_ui::layout::control_gap();
        let text_in = cce_ui::layout::CONTROL_TEXT_INSET;
        let inner_w = (w - 2.0 * inset).max(0.0);

        let addr_w = face.char_w * 12.0 + 2.0 * text_in;
        let address = Rect { x: inset, y: inset, width: addr_w.min(inner_w), height: BAR_H };
        let content = Rect { x: inset + addr_w + cgap, y: inset, width: (inner_w - addr_w - cgap).max(0.0), height: BAR_H };

        let tabs_y = h - inset - BAR_H;
        let mut tabs = Vec::new();
        let mut x = inset;
        // The tabs butt together in one strip, like a ButtonStrip's segments.
        for (i, name) in self.visible_sheets() {
            let tw = face.text_width(&name) + 2.0 * text_in;
            tabs.push((Rect { x, y: tabs_y, width: tw, height: BAR_H }, i));
            x += tw;
        }

        let tools_y = inset + BAR_H + gap;
        let tools = toolbar::layout(inset, tools_y, BAR_H, face.char_w, cgap * 2.0);
        let well_y = tools_y + BAR_H + gap;
        let well = Rect { x: inset, y: well_y, width: inner_w, height: (tabs_y - gap - well_y).max(0.0) };
        // The cells run to the well's edge and slide under its rim.
        let rim = cce_ui::layout::bevel_width().min(well.height * 0.2) * 0.5;
        let area = Rect { x: well.x + rim, y: well.y + rim, width: (well.width - 2.0 * rim).max(0.0), height: (well.height - 2.0 * rim).max(0.0) };

        let header_h = (face.size + 12.0).max(22.0);
        // The row header is as wide as the last row on screen needs; measure
        // with a guess, then with what that guess shows.
        let mut header_w = grid::row_header_width(face, 999);
        for _ in 0..2 {
            let v = View { geom: &self.geom, area, header_w, header_h, scroll: self.scroll };
            header_w = grid::row_header_width(face, v.rows().last);
        }
        self.frame = Frame { address, content, well, area, header_w, header_h, tabs, editor: None, fill_handle: None, tools, popover: None };
    }

    fn select_hit(&mut self, hit: Hit, extend: bool) -> bool {
        let m = &mut self.book.model;
        let last_row = self.geom.rows.last();
        let last_col = self.geom.cols.last();
        let done = match hit {
            Hit::Cell { row, col } if extend => m.on_area_selecting(row, col),
            Hit::Cell { row, col } => m.set_selected_cell(row, col),
            Hit::ColHeader(col) => m.set_selected_cell(1, col).and_then(|_| m.set_selected_range(1, col, last_row, col)),
            Hit::RowHeader(row) => m.set_selected_cell(row, 1).and_then(|_| m.set_selected_range(row, 1, row, last_col)),
            Hit::Corner => m.set_selected_cell(1, 1).and_then(|_| m.set_selected_range(1, 1, last_row, last_col)),
            Hit::Outside | Hit::ColEdge(_) | Hit::RowEdge(_) => return false,
        };
        done.is_ok()
    }

    // ---- clipboard, structure, fill, resize ----

    fn copy(&mut self, cut: bool) {
        match clip::copy(&self.book.model, cut) {
            Ok(c) => {
                cce_ui::widget::clipboard::copy_to_clipboard(&c.text);
                self.copied = Some(c);
            }
            Err(e) => self.set_status(e, true),
        }
    }

    fn paste(&mut self) {
        let system = cce_ui::widget::clipboard::read_from_clipboard();
        let result = match clip::source(self.copied.as_ref(), system) {
            clip::Source::Cells => {
                let c = self.copied.as_ref().expect("source said cells");
                let r = clip::paste_cells(&mut self.book.model, c);
                // A cut moves once; its source is empty now.
                if c.cut {
                    self.copied = None;
                }
                r
            }
            clip::Source::Text(t) => clip::paste_text(&mut self.book.model, &t),
            clip::Source::Nothing => return,
        };
        match result {
            Ok(()) => {
                self.changed();
                self.reveal_selection();
            }
            Err(e) => self.set_status(e, true),
        }
    }

    /// Insert (or, with `insert` false, delete) rows or columns at the selection.
    fn structure(&mut self, line: Line, insert: bool, after: bool) {
        let sheet = self.sheet();
        let range = self.selection().range;
        let used = grid::used_extent(&self.book.model, sheet);
        let r = if insert {
            ops::insert(&mut self.book.model, sheet, range, line, after, used)
        } else {
            ops::delete(&mut self.book.model, sheet, range, line, used)
        };
        match r {
            Ok(()) => {
                // Copied ranges no longer name the cells they did.
                self.copied = None;
                self.changed();
            }
            Err(e) => self.set_status(e, true),
        }
    }

    /// Fit each selected column to its widest value (a double-click on its
    /// header edge, or the menu).
    fn autofit(&mut self, c1: i32, c2: i32) {
        let Some(face) = self.face.clone() else {
            return;
        };
        let sheet = self.sheet();
        let (_, used_c) = grid::used_extent(&self.book.model, sheet);
        let default = self.geom.cols.size(grid::LAST_COLUMN);
        let mut any = false;
        for c in c1..=c2.min(used_c.max(c1)) {
            let widths = ops::column_texts(&self.book.model, sheet, c).iter().map(|t| face.text_width(t)).collect::<Vec<_>>();
            let w = ops::fit_width(widths, grid::CELL_PAD).unwrap_or(default);
            any |= self.book.model.set_columns_width(sheet, c, c, w).is_ok();
        }
        if any {
            self.changed();
        }
    }

    /// A press on a header edge: a double-click fits the column (or puts the
    /// row back to the default height); a single press starts a resize.
    fn press_edge(&mut self, hit: Hit, x: f32, y: f32) {
        let now = Instant::now();
        let double = self.last_edge_press.is_some_and(|(t, h)| h == hit && now.duration_since(t) < DOUBLE_CLICK);
        self.last_edge_press = if double { None } else { Some((now, hit)) };
        let sheet = self.sheet();
        match (hit, double) {
            (Hit::ColEdge(c), true) => self.autofit(c, c),
            (Hit::RowEdge(r), true) => {
                let h = self.geom.rows.size(grid::LAST_ROW);
                if self.book.model.set_rows_height(sheet, r, r, h).is_ok() {
                    self.changed();
                }
            }
            (Hit::ColEdge(c), false) => {
                let s = self.geom.cols.size(c);
                self.resize = Some(Resize { line: Line::Cols, index: c, origin: x, start: s, size: s, base: self.geom.cols.clone() });
            }
            (Hit::RowEdge(r), false) => {
                let s = self.geom.rows.size(r);
                self.resize = Some(Resize { line: Line::Rows, index: r, origin: y, start: s, size: s, base: self.geom.rows.clone() });
            }
            _ => {}
        }
    }

    /// The resize drag moved: preview the new size.
    fn drag_resize(&mut self, x: f32, y: f32) {
        let Some(rz) = self.resize.as_mut() else {
            return;
        };
        let d = match rz.line {
            Line::Cols => x - rz.origin,
            Line::Rows => y - rz.origin,
        };
        rz.size = (rz.start + d as f64).max(4.0);
        match rz.line {
            Line::Cols => self.geom.cols = rz.base.with_size(rz.index, rz.size),
            Line::Rows => self.geom.rows = rz.base.with_size(rz.index, rz.size),
        }
    }

    /// The resize drag ended: write the size — to every selected whole
    /// column (or row) when the dragged one is among them, as Excel does.
    fn end_resize(&mut self) {
        let Some(rz) = self.resize.take() else {
            return;
        };
        if rz.size == rz.start {
            return;
        }
        let sheet = self.sheet();
        let [r1, c1, r2, c2] = ops::normalized(self.selection().range);
        let range = [r1, c1, r2, c2];
        let r = match rz.line {
            Line::Cols if ops::whole_cols(range) && (c1..=c2).contains(&rz.index) => self.book.model.set_columns_width(sheet, c1, c2, rz.size),
            Line::Cols => self.book.model.set_columns_width(sheet, rz.index, rz.index, rz.size),
            Line::Rows if ops::whole_rows(range) && (r1..=r2).contains(&rz.index) => self.book.model.set_rows_height(sheet, r1, r2, rz.size),
            Line::Rows => self.book.model.set_rows_height(sheet, rz.index, rz.index, rz.size),
        };
        match r {
            Ok(()) => self.changed(),
            Err(e) => self.set_status(e, true),
        }
    }

    /// The fill drag ended: fill to where it reached, and select the result.
    fn end_fill(&mut self) {
        let Some(Some(to)) = self.fill.take() else {
            return;
        };
        let sheet = self.sheet();
        let range = self.selection().range;
        match ops::fill(&mut self.book.model, sheet, range, to) {
            Ok(()) => {
                let [r1, c1, r2, c2] = ops::filled_range(range, to);
                let sel = self.selection();
                let _ = self.book.model.set_selected_range(r1, c1, r2, c2).or_else(|_| {
                    self.book.model.set_selected_cell(sel.row, sel.col)
                });
                self.changed();
            }
            Err(e) => self.set_status(e, true),
        }
    }

    /// Open the right-click menu for what is under the pointer; the choice
    /// comes back as [`Message::Menu`].
    fn open_menu(&mut self, hit: Hit) {
        let entries = menu::entries(hit, self.copied.is_some() || cce_ui::widget::clipboard::read_from_clipboard().is_some());
        self.open_menu_with(entries);
    }

    /// Show `entries` at the pointer on a thread; the choice comes back as
    /// [`Message::Menu`].
    fn open_menu_with(&mut self, entries: menu::Menu) {
        if entries.1.is_empty() {
            return;
        }
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            if let Some(id) = menu::show(&entries) {
                let _ = sender.send(Message::Menu(id));
            }
        });
    }

    fn menu_action(&mut self, id: &str) {
        match id {
            menu::CUT => self.copy(true),
            menu::COPY => self.copy(false),
            menu::PASTE => self.paste(),
            menu::CLEAR => self.clear_selection(),
            menu::INSERT_ROWS_ABOVE => self.structure(Line::Rows, true, false),
            menu::INSERT_ROWS_BELOW => self.structure(Line::Rows, true, true),
            menu::DELETE_ROWS => self.structure(Line::Rows, false, false),
            menu::INSERT_COLS_LEFT => self.structure(Line::Cols, true, false),
            menu::INSERT_COLS_RIGHT => self.structure(Line::Cols, true, true),
            menu::DELETE_COLS => self.structure(Line::Cols, false, false),
            menu::FIT_COLS => {
                let [_, c1, _, c2] = ops::normalized(self.selection().range);
                self.autofit(c1, c2);
            }
            id if id.starts_with("font-color:") || id.starts_with("fill-color:") => {
                let (key, hex) = id.split_once(':').expect("matched a colon");
                let path = if key == "font-color" { "font.color" } else { "fill.color" };
                let (sheet, range) = (self.sheet(), self.selection().range);
                let r = format::set(&mut self.book.model, sheet, range, path, hex);
                self.styled(r);
            }
            id if id.starts_with("fmt-") => {
                if let Some((_, _, code)) = format::NUMBER_FORMATS.iter().find(|(f, _, _)| *f == id) {
                    let (sheet, range) = (self.sheet(), self.selection().range);
                    let r = format::set(&mut self.book.model, sheet, range, "num_fmt", code);
                    self.styled(r);
                }
            }
            id if id.starts_with("border-") => {
                if let Some((_, _, kind)) = format::BORDERS.iter().find(|(b, _, _)| *b == id) {
                    let (sheet, range) = (self.sheet(), self.selection().range);
                    let r = format::borders(&mut self.book.model, sheet, range, kind);
                    self.styled(r);
                }
            }
            other => log::warn!("unknown menu action {other:?}"),
        }
    }

    fn navigate(&mut self, key: &Key) -> bool {
        let page = self.page_rows();
        let last_row = self.geom.rows.last();
        let m = &mut self.book.model;
        let sel = m.get_selected_view();
        let r = match key {
            Key::Named(NamedKey::ArrowLeft) | Key::Named(NamedKey::ArrowRight) | Key::Named(NamedKey::ArrowUp) | Key::Named(NamedKey::ArrowDown) => {
                let (name, dir) = match key {
                    Key::Named(NamedKey::ArrowLeft) => ("ArrowLeft", NavigationDirection::Left),
                    Key::Named(NamedKey::ArrowRight) => ("ArrowRight", NavigationDirection::Right),
                    Key::Named(NamedKey::ArrowUp) => ("ArrowUp", NavigationDirection::Up),
                    _ => ("ArrowDown", NavigationDirection::Down),
                };
                if self.ctrl {
                    m.on_navigate_to_edge_in_direction(dir)
                } else if self.shift {
                    m.on_expand_selected_range(name)
                } else {
                    match dir {
                        NavigationDirection::Left => m.on_arrow_left(),
                        NavigationDirection::Right => m.on_arrow_right(),
                        NavigationDirection::Up => m.on_arrow_up(),
                        NavigationDirection::Down => m.on_arrow_down(),
                    }
                }
            }
            Key::Named(NamedKey::Enter) if self.shift => m.on_arrow_up(),
            Key::Named(NamedKey::Enter) => m.on_arrow_down(),
            Key::Named(NamedKey::Tab) if self.shift => m.on_arrow_left(),
            Key::Named(NamedKey::Tab) => m.on_arrow_right(),
            Key::Named(NamedKey::PageDown) | Key::Named(NamedKey::PageUp) if self.ctrl => {
                let step = if matches!(key, Key::Named(NamedKey::PageDown)) { 1 } else { -1 };
                self.step_sheet(step);
                return true;
            }
            Key::Named(NamedKey::PageDown) => {
                let row = (sel.row + page).min(last_row);
                m.set_selected_cell(row, sel.column)
            }
            Key::Named(NamedKey::PageUp) => {
                let row = (sel.row - page).max(1);
                m.set_selected_cell(row, sel.column)
            }
            Key::Named(NamedKey::Home) if self.ctrl => m.set_selected_cell(1, 1),
            Key::Named(NamedKey::Home) => m.set_selected_cell(sel.row, 1),
            Key::Named(NamedKey::End) if self.ctrl => {
                let (r, c) = grid::used_extent(m, sel.sheet);
                m.set_selected_cell(r, c)
            }
            _ => return false,
        };
        if let Err(e) = r {
            log::debug!("navigation refused: {e}");
        }
        self.reveal_selection();
        true
    }

    fn paint_bar(&self, pc: &mut PaintCtx, face: &Face, marks: Option<&Marks>) {
        let sel = self.selection();
        let [r1, c1, r2, c2] = sel.range;
        let address = if let Some(ed) = &self.editing {
            format!("{}{}", column_name(ed.col), ed.row)
        } else if (r1, c1) == (r2, c2) {
            format!("{}{}", column_name(sel.col), sel.row)
        } else {
            format!("{}{}:{}{}", column_name(c1.min(c2)), r1.min(r2), column_name(c1.max(c2)), r1.max(r2))
        };
        let content = self.book.model.get_cell_content(self.sheet(), sel.row, sel.col).unwrap_or_default();
        let content = content.lines().next().unwrap_or("").to_string();
        let radius = cce_ui::layout::control_corner_radius();
        let relief = cce_ui::layout::control_relief();
        let text_in = cce_ui::layout::CONTROL_TEXT_INSET;
        for (i, (rect, text)) in [(self.frame.address, address), (self.frame.content, content)].into_iter().enumerate() {
            if rect.width <= 0.0 {
                continue;
            }
            pc.canvas_well(rect, radius, &Material::root(), relief, false);
            if i == 1 {
                if let (Some(ed), Some(marks)) = (&self.editing, marks) {
                    // The edit, mirrored; the caret here when the bar has it.
                    let field = Rect { x: rect.x + text_in, y: rect.y, width: (rect.width - 2.0 * text_in).max(0.0), height: rect.height };
                    paint_field(pc, field, field.x - ed.bar_shift, marks, face, face.text, ed.in_bar);
                    continue;
                }
            }
            let ty = cce_ui::layout::center_text_y(rect.y, rect.height, face.size);
            let bounds = [rect.x + text_in, rect.y, rect.x + rect.width - text_in, rect.y + rect.height];
            pc.text_with(text, rect.x + text_in, ty, face.size, face.text, Some(face.family.clone()), Some(bounds));
        }
    }

    /// The in-cell editor over the grid: an opaque box at the cell, widened
    /// to fit the text, framed like the cursor.
    fn paint_editor(&self, pc: &mut PaintCtx, face: &Face, marks: &Marks) {
        let (Some(ed), Some(r)) = (&self.editing, self.frame.editor) else {
            return;
        };
        pc.clip(self.view().body(), |pc| {
            pc.quad(r, EDITOR_FILL);
            grid::frame(pc, r, grid::cursor_color(face));
            let field = Rect { x: r.x + grid::CELL_PAD, y: r.y, width: (r.width - 2.0 * grid::CELL_PAD).max(0.0), height: r.height };
            paint_field(pc, field, field.x - ed.cell_shift, marks, face, face.text, !ed.in_bar);
        });
    }

    /// The sheet tabs as a ButtonStrip draws its recessed style: one well
    /// carved around the run, the selected tab a plateau raised back out of
    /// its floor; flat, the selected tab is a fill.
    /// The sheet tabs: a strip whose selected segment is the shown sheet.
    fn paint_tabs(&self, pc: &mut PaintCtx, face: &Face) {
        let cur = self.sheet();
        let segs: Vec<(Rect, bool)> = self.frame.tabs.iter().map(|&(r, i)| (r, i == cur)).collect();
        toolbar::strip(pc, &segs);
        let text_in = cce_ui::layout::CONTROL_TEXT_INSET;
        let names: HashMap<u32, String> = self.visible_sheets().into_iter().collect();
        for (rect, i) in &self.frame.tabs {
            let ty = cce_ui::layout::center_text_y(rect.y, rect.height, face.size);
            let color = if *i == cur { face.text } else { dim(face.text) };
            let name = names.get(i).cloned().unwrap_or_default();
            pc.text_with(name, rect.x + text_in, ty, face.size, color, Some(face.family.clone()), None);
        }
    }

    /// The completion list: names under the formula, the picked one lit.
    fn paint_popover(&self, pc: &mut PaintCtx, face: &Face) {
        let (Some((r, row_h)), Some(c)) = (self.frame.popover, self.editing.as_ref().and_then(|e| e.completion.as_ref())) else {
            return;
        };
        let text_in = cce_ui::layout::CONTROL_TEXT_INSET;
        pc.quad(r, EDITOR_FILL);
        grid::frame_with(pc, r, [1.0, 1.0, 1.0, 0.22], 1.0);
        for (i, name) in c.names.iter().enumerate() {
            let row = Rect { x: r.x + 2.0, y: r.y + 2.0 + i as f32 * row_h, width: r.width - 4.0, height: row_h };
            if i == c.pick {
                let h = face.highlight;
                pc.quad(row, [h[0], h[1], h[2], 0.45]);
            }
            let ty = cce_ui::layout::center_text_y(row.y, row.height, face.size);
            pc.text_with(name.clone(), row.x + text_in, ty, face.size, face.text, Some(face.family.clone()), None);
        }
    }

    /// Re-read what the edited formula could complete to, keeping the pick
    /// while the list is the same.
    fn refresh_completion(&mut self) {
        let Some(ed) = self.editing.as_ref() else {
            return;
        };
        let text = ed.edit.text.clone();
        let (sheet, row, col, cursor) = (ed.sheet, ed.row, ed.col, ed.edit.cursor);
        let quiet = ed.completion_dismissed.as_deref() == Some(text.as_str()) || ed.edit.selection.is_some();
        let fresh = if quiet { None } else { edit::completion(&mut self.book.model, sheet, row, col, &text, cursor, &self.func_names) };
        let ed = self.editing.as_mut().expect("checked above");
        ed.completion = match (ed.completion.take(), fresh) {
            (Some(old), Some(new)) if old.names == new.names => Some(edit::Completion { pick: old.pick, ..new }),
            (_, new) => new,
        };
    }

    /// Take completion `i`: the typed prefix becomes the name and `(`.
    fn accept_completion(&mut self, i: usize) {
        let Some(ed) = self.editing.as_mut() else {
            return;
        };
        let Some(c) = ed.completion.take() else {
            return;
        };
        let Some(name) = c.names.get(i) else {
            return;
        };
        let (text, caret) = edit::accept(&ed.edit.text, &c, name);
        ed.edit.text = text;
        ed.edit.cursor = caret;
        ed.edit.selection = None;
    }

    /// A toolbar press: a toggle or alignment acts at once; the format,
    /// colour and border tools open their menus.
    fn press_tool(&mut self, tool: Tool) {
        let sheet = self.sheet();
        let sel = self.selection();
        let a = format::active(&self.book.model, sheet, sel.row, sel.col);
        let m = &mut self.book.model;
        let r = match tool {
            Tool::Bold => format::toggle(m, sheet, sel.range, "font.b", a.bold),
            Tool::Italic => format::toggle(m, sheet, sel.range, "font.i", a.italic),
            Tool::Underline => format::toggle(m, sheet, sel.range, "font.u", a.underline),
            Tool::AlignLeft => format::align(m, sheet, sel.range, format::Align::Left, a.align),
            Tool::AlignCenter => format::align(m, sheet, sel.range, format::Align::Center, a.align),
            Tool::AlignRight => format::align(m, sheet, sel.range, format::Align::Right, a.align),
            Tool::Format => {
                let rows: Vec<(&str, &str)> = format::NUMBER_FORMATS.iter().map(|(id, label, _)| (*id, *label)).collect();
                return self.open_menu_with(menu::of("Number format", &rows));
            }
            Tool::TextColor | Tool::FillColor => {
                let (key, title, none) = if tool == Tool::TextColor { ("font-color:", "Text colour", "Automatic") } else { ("fill-color:", "Fill colour", "No fill") };
                let mut rows = vec![(key.to_string(), none.to_string())];
                rows.extend(format::PALETTE.iter().map(|(name, hex)| (format!("{key}{hex}"), name.to_string())));
                return self.open_menu_with((title.to_string(), rows));
            }
            Tool::Borders => {
                let rows: Vec<(&str, &str)> = format::BORDERS.iter().map(|(id, label, _)| (*id, *label)).collect();
                return self.open_menu_with(menu::of("Borders", &rows));
            }
        };
        self.styled(r);
    }

    /// After a style change: unsaved and repainted, or the reason shown.
    fn styled(&mut self, r: Result<(), String>) {
        match r {
            Ok(()) => self.changed(),
            Err(e) => self.set_status(e, true),
        }
    }
}

impl Application for SheetsApp {
    type Message = Message;

    fn create(sender: AppSender<Self::Message>) -> Self {
        let fs = cce_ui::create_font_system();
        let arg: Option<PathBuf> = std::env::args_os().nth(1).map(PathBuf::from);
        match arg {
            Some(path) => match Book::open(&path) {
                Ok(book) => SheetsApp::with_book(book, None, fs, sender),
                Err(e) => SheetsApp::with_book(Book::untitled(), Some(format!("{}: {e}", path.display())), fs, sender),
            },
            None => SheetsApp::with_book(Book::untitled(), None, fs, sender),
        }
    }

    fn settings(&self) -> WindowSettings {
        WindowSettings {
            title: format!("{}{} — Sheets", if self.dirty { "*" } else { "" }, self.book.display_name()),
            app_id: "cce-sheets".to_string(),
            width: 1100,
            height: 760,
            fullscreen: false,
            min_size: Some((420, 300)),
        }
    }

    fn update(&mut self, msg: Self::Message, needs_rebuild: &mut bool, _exit: &mut bool) {
        match msg {
            Message::Menu(id) => {
                if self.commit_edit() {
                    self.menu_action(&id);
                }
                *needs_rebuild = true;
            }
        }
    }

    fn cursor_icon(&self, x: f32, y: f32) -> Option<CursorIcon> {
        if let Some(rz) = &self.resize {
            return Some(if rz.line == Line::Cols { CursorIcon::ColResize } else { CursorIcon::RowResize });
        }
        if self.fill.is_some() || self.frame.fill_handle.is_some_and(|r| contains(&r, x, y)) {
            return Some(CursorIcon::Crosshair);
        }
        if self.frame.editor.is_some_and(|r| contains(&r, x, y)) || contains(&self.frame.content, x, y) {
            return Some(CursorIcon::Text);
        }
        match self.view().hit(x, y) {
            Hit::ColEdge(_) => Some(CursorIcon::ColResize),
            Hit::RowEdge(_) => Some(CursorIcon::RowResize),
            _ => None,
        }
    }

    fn tick(&mut self, dt: f32, needs_rebuild: &mut bool) {
        if self.tick_scroll(dt) {
            *needs_rebuild = true;
        }
    }

    /// The grid is the window's whole content and takes Tab itself.
    fn plate_navigation(&self) -> bool {
        false
    }

    fn handle_resize(&mut self, width: f32, height: f32, scale: f64) {
        self.win = (width, height);
        self.scale = scale;
    }

    fn handle_pointer_move(&mut self, pos: LogicalPosition, needs_rebuild: &mut bool) {
        if self.resize.is_some() {
            self.drag_resize(pos.x, pos.y);
            *needs_rebuild = true;
            return;
        }
        if self.fill.is_some() {
            if let Hit::Cell { row, col } = self.view().hit(pos.x, pos.y) {
                let to = ops::fill_target(self.selection().range, row, col);
                if self.fill != Some(to) {
                    self.fill = Some(to);
                    *needs_rebuild = true;
                }
            }
            return;
        }
        if self.edit_drag {
            let in_bar = self.editing.as_ref().is_some_and(|e| e.in_bar);
            if let Some(at) = self.edit_byte_at(pos.x, in_bar) {
                if self.editing.as_mut().is_some_and(|e| e.edit.drag_to(at)) {
                    *needs_rebuild = true;
                }
            }
            return;
        }
        if self.selecting {
            let hit = self.view().hit(pos.x, pos.y);
            if let Hit::Cell { .. } = hit {
                if self.select_hit(hit, true) {
                    *needs_rebuild = true;
                }
            }
        }
    }

    fn handle_mouse_input(
        &mut self,
        button: MouseButton,
        state: ElementState,
        pos: LogicalPosition,
        needs_rebuild: &mut bool,
    ) -> Option<Self::Message> {
        if button == MouseButton::Right && state == ElementState::Pressed {
            if !self.commit_edit() {
                return None;
            }
            // A right-click outside the selection selects what it is on first.
            let hit = self.view().hit(pos.x, pos.y);
            let [r1, c1, r2, c2] = ops::normalized(self.selection().range);
            let inside = match hit {
                Hit::Cell { row, col } => (r1..=r2).contains(&row) && (c1..=c2).contains(&col),
                Hit::ColHeader(c) => ops::whole_cols([r1, c1, r2, c2]) && (c1..=c2).contains(&c),
                Hit::RowHeader(r) => ops::whole_rows([r1, c1, r2, c2]) && (r1..=r2).contains(&r),
                _ => false,
            };
            if !inside {
                self.select_hit(hit, false);
            }
            *needs_rebuild = true;
            self.open_menu(hit);
            return None;
        }
        if button != MouseButton::Left {
            return None;
        }
        if state == ElementState::Released {
            self.selecting = false;
            if self.resize.is_some() {
                self.end_resize();
                *needs_rebuild = true;
            }
            if self.fill.is_some() {
                self.end_fill();
                *needs_rebuild = true;
            }
            if std::mem::take(&mut self.edit_drag) {
                if let Some(ed) = self.editing.as_mut() {
                    ed.edit.release();
                }
            }
            return None;
        }
        *needs_rebuild = true;
        let (x, y) = (pos.x, pos.y);
        // A completion row takes that name.
        if let Some((r, row_h)) = self.frame.popover.filter(|(r, _)| contains(r, x, y)) {
            let i = ((y - r.y - 2.0) / row_h).floor().max(0.0) as usize;
            self.accept_completion(i);
            return None;
        }
        // A toolbar tool applies to the selection, an open edit kept first.
        if let Some(tool) = toolbar::hit(&self.frame.tools, x, y) {
            if self.commit_edit() {
                self.press_tool(tool);
            }
            return None;
        }
        // A press in the field being edited, or in the bar: the caret goes there.
        let in_editor = self.frame.editor.is_some_and(|r| contains(&r, x, y));
        let in_bar = contains(&self.frame.content, x, y);
        if in_editor || in_bar {
            if self.editing.is_none() {
                self.edit_selected(true);
            }
            if let Some(ed) = self.editing.as_mut() {
                ed.in_bar = in_bar;
            }
            if let Some(at) = self.edit_byte_at(x, in_bar) {
                let shift = self.shift;
                if let Some(ed) = self.editing.as_mut() {
                    ed.edit.press(at, shift);
                    self.edit_drag = true;
                }
            }
            return None;
        }
        // Anywhere else ends an edit, kept; a refused one stays open.
        if !self.commit_edit() {
            return None;
        }
        if let Some(&(_, sheet)) = self.frame.tabs.iter().find(|(r, _)| contains(r, x, y)) {
            let _ = self.book.model.set_selected_sheet(sheet);
            self.sync_sheet();
            return None;
        }
        if self.frame.fill_handle.is_some_and(|r| contains(&r, x, y)) {
            self.fill = Some(None);
            return None;
        }
        let hit = self.view().hit(x, y);
        if matches!(hit, Hit::ColEdge(_) | Hit::RowEdge(_)) {
            self.press_edge(hit, x, y);
            return None;
        }
        if let Hit::Cell { row, col } = hit {
            let now = Instant::now();
            let double = self.last_press.is_some_and(|(t, r, c)| (r, c) == (row, col) && now.duration_since(t) < DOUBLE_CLICK);
            self.last_press = if double { None } else { Some((now, row, col)) };
            if double && !self.shift {
                self.edit_selected(false);
                return None;
            }
        }
        if self.select_hit(hit, self.shift) {
            self.selecting = matches!(hit, Hit::Cell { .. });
        }
        None
    }

    fn handle_mouse_wheel(&mut self, delta: &MouseScrollDelta, _pos: LogicalPosition, needs_rebuild: &mut bool) {
        let (mut dx, mut dy) = ScrollMotion::delta_px(delta, (WHEEL_LINE_PX, WHEEL_LINE_PX));
        if self.shift {
            dx = dy;
            dy = 0.0;
        }
        let discrete = matches!(delta, MouseScrollDelta::LineDelta(..));
        let (mx, my) = self.scroll_bounds();
        self.scroll_motion.reconcile(self.scroll.0 as f32, self.scroll.1 as f32);
        if self.scroll_motion.apply_px(dx, dy, discrete, Bounds::max(mx as f32), Bounds::max(my as f32)) {
            self.scroll = (self.scroll_motion.x.pos() as f64, self.scroll_motion.y.pos() as f64);
            *needs_rebuild = true;
        }
    }

    fn handle_key_input(&mut self, event: &KeyEvent, needs_rebuild: &mut bool) -> Option<Self::Message> {
        // Wheel and pointer events carry no modifiers; follow them here.
        match &event.logical_key {
            Key::Named(NamedKey::Control) => self.ctrl = event.state == ElementState::Pressed,
            Key::Named(NamedKey::Shift) => self.shift = event.state == ElementState::Pressed,
            _ => {
                self.ctrl = event.ctrl;
                self.shift = event.shift;
            }
        }
        if event.state != ElementState::Pressed {
            return None;
        }
        // The app's own chords, edit or no edit.
        if let Key::Character(c) = &event.logical_key {
            if event.ctrl && !event.alt {
                let handled = match c.to_ascii_lowercase().as_str() {
                    "s" => {
                        self.save(event.shift);
                        true
                    }
                    "o" => {
                        self.open_dialog();
                        true
                    }
                    _ => false,
                };
                if handled {
                    *needs_rebuild = true;
                    return None;
                }
            }
        }
        // The completion list, while it shows, has the keys that move in it.
        if let Some(c) = self.editing.as_ref().and_then(|e| e.completion.as_ref()) {
            let n = c.names.len();
            let pick = c.pick;
            let took = match &event.logical_key {
                Key::Named(NamedKey::ArrowDown) => Some(Some((pick + 1) % n)),
                Key::Named(NamedKey::ArrowUp) => Some(Some((pick + n - 1) % n)),
                Key::Named(NamedKey::Tab | NamedKey::Enter) => {
                    self.accept_completion(pick);
                    Some(None)
                }
                Key::Named(NamedKey::Escape) => {
                    let ed = self.editing.as_mut().expect("has a completion");
                    ed.completion_dismissed = Some(ed.edit.text.clone());
                    ed.completion = None;
                    Some(None)
                }
                _ => None,
            };
            if let Some(next) = took {
                if let (Some(p), Some(c)) = (next, self.editing.as_mut().and_then(|e| e.completion.as_mut())) {
                    c.pick = p;
                }
                *needs_rebuild = true;
                return None;
            }
        }
        if let Some(ed) = self.editing.as_mut() {
            match edit::classify(ed, event) {
                EditKey::Cancel => {
                    self.end_edit();
                }
                EditKey::CommitAndMove => {
                    if self.commit_edit() {
                        self.navigate(&event.logical_key);
                    }
                }
                EditKey::Field => {
                    ed.edit.handle_key(event);
                    self.refresh_completion();
                }
            }
            *needs_rebuild = true;
            return None;
        }
        let sheet = self.sheet();
        let sel = self.selection();
        // Cell chords: the edit had first claim on these, as its own text's.
        if let Some(chord) = cell_chord(event) {
            let line = if ops::whole_cols(sel.range) { Line::Cols } else { Line::Rows };
            match chord {
                CellChord::Copy => self.copy(false),
                CellChord::Cut => self.copy(true),
                CellChord::Paste => self.paste(),
                CellChord::Insert => self.structure(line, true, false),
                CellChord::Delete => self.structure(line, false, false),
                CellChord::Bold => self.press_tool(Tool::Bold),
                CellChord::Italic => self.press_tool(Tool::Italic),
                CellChord::Underline => self.press_tool(Tool::Underline),
            }
            *needs_rebuild = true;
            return None;
        }
        let handled = match &event.logical_key {
            Key::Named(NamedKey::Escape) if self.copied.is_some() => {
                self.copied = None;
                true
            }
            Key::Named(NamedKey::F2) => {
                self.edit_selected(false);
                true
            }
            Key::Named(NamedKey::Delete) => {
                self.clear_selection();
                true
            }
            // Backspace starts over in the cell: an empty edit replacing it.
            Key::Named(NamedKey::Backspace) => {
                self.begin_edit(CellEdit::typed_over(sheet, sel.row, sel.col));
                true
            }
            _ if edit::types(event) => {
                let mut ed = CellEdit::typed_over(sheet, sel.row, sel.col);
                ed.edit.handle_key(event);
                self.begin_edit(ed);
                true
            }
            key => self.navigate(key),
        };
        if handled {
            *needs_rebuild = true;
        }
        None
    }

    /// Undo: the edit's own typing while a cell is edited, else the
    /// workbook's last change.
    fn undo(&mut self, needs_rebuild: &mut bool) -> bool {
        *needs_rebuild = true;
        if let Some(ed) = self.editing.as_mut() {
            ed.edit.undo();
            return true;
        }
        if !self.book.model.can_undo() {
            return false;
        }
        match self.book.model.undo() {
            Ok(()) => {
                self.changed();
                self.reveal_selection();
            }
            Err(e) => self.set_status(e, true),
        }
        true
    }

    fn redo(&mut self, needs_rebuild: &mut bool) -> bool {
        *needs_rebuild = true;
        if let Some(ed) = self.editing.as_mut() {
            ed.edit.redo();
            return true;
        }
        if !self.book.model.can_redo() {
            return false;
        }
        match self.book.model.redo() {
            Ok(()) => {
                self.changed();
                self.reveal_selection();
            }
            Err(e) => self.set_status(e, true),
        }
        true
    }

    /// The window is going: an edit in progress is kept, and unsaved changes
    /// go to a recovery copy (see the module doc).
    fn on_exit(&mut self) {
        self.commit_edit();
        self.keep_unsaved();
    }

    fn display_list(&mut self, size: LogicalSize, scale: f64) -> Option<DisplayList> {
        self.win = (size.width, size.height);
        if self.face.is_none() || scale != self.scale {
            self.face = Some(Face::current(scale));
        }
        self.scale = scale;
        self.sync_sheet();
        self.layout(size.width, size.height);
        self.clamp_scroll();
        let face = self.face.clone().expect("set above");

        // The edit: take up the input method's composition, measure the text,
        // place the in-cell editor and keep each field's caret in view.
        if let Some(ed) = self.editing.as_mut() {
            ed.edit.sync_ime();
        }
        let marks = self.edit_marks();
        self.frame.editor = None;
        if let (Some(m), Some(ed)) = (&marks, &self.editing) {
            let view = self.view();
            if ed.sheet == self.sheet() {
                let body = view.body();
                let cell = view.cell_rect(ed.row, ed.col);
                self.frame.editor = Some(edit::editor_rect(cell, m.width(), grid::CELL_PAD, body.x + body.width));
            }
        }
        if let (Some(m), Some(ed)) = (&marks, self.editing.as_mut()) {
            let text_in = cce_ui::layout::CONTROL_TEXT_INSET;
            if let Some(r) = self.frame.editor {
                ed.cell_shift = edit::keep_caret(ed.cell_shift, m.caret, m.width(), r.width - 2.0 * grid::CELL_PAD);
            }
            ed.bar_shift = edit::keep_caret(ed.bar_shift, m.caret, m.width(), self.frame.content.width - 2.0 * text_in);
        }
        // The completion list hangs under the typed prefix, in the bar or
        // under the in-cell editor, whichever has the caret.
        self.refresh_completion();
        if let (Some(m), Some(ed)) = (&marks, &self.editing) {
            if let Some(c) = &ed.completion {
                let text_in = cce_ui::layout::CONTROL_TEXT_INSET;
                // (text origin x, the field's top, its bottom)
                let anchor = if ed.in_bar {
                    let b = self.frame.content;
                    Some((b.x + text_in - ed.bar_shift, b.y, b.y + b.height))
                } else {
                    self.frame.editor.map(|r| (r.x + grid::CELL_PAD - ed.cell_shift, r.y, r.y + r.height))
                };
                if let Some((x0, top, bottom)) = anchor {
                    let row_h = (face.size + 10.0).max(20.0);
                    let widest = c.names.iter().map(|n| n.chars().count()).max().unwrap_or(4) as f32;
                    let w = widest * face.char_w + 2.0 * text_in + 4.0;
                    let h = c.names.len() as f32 * row_h + 4.0;
                    let x = (x0 + m.x_of(ed.edit.display_index(c.from)) - text_in).clamp(0.0, (size.width - w).max(0.0));
                    // Below the field, or above it where below would leave the window.
                    let room = size.height - cce_ui::layout::root_plate_inset();
                    let y = if bottom + 2.0 + h > room && top - 2.0 - h >= 0.0 { top - 2.0 - h } else { bottom + 2.0 };
                    self.frame.popover = Some((Rect { x, y, width: w, height: h }, row_h));
                }
            }
        }

        // The fill handle sits on the selection's bottom-right corner, when
        // that corner is on screen and nothing is being edited.
        self.frame.fill_handle = None;
        if self.editing.is_none() {
            let [_, _, r2, c2] = ops::normalized(self.selection().range);
            let view = self.view();
            let corner = view.cell_rect(r2, c2);
            let (cx, cy) = (corner.x + corner.width, corner.y + corner.height);
            let body = view.body();
            let (x0, x1) = view.col_band(c2);
            let (y0, y1) = view.row_band(r2);
            if cx > x0 && cx <= x1 + 1.0 && cy > y0 && cy <= y1 + 1.0 && cx <= body.x + body.width && cy <= body.y + body.height {
                let h = FILL_HANDLE;
                self.frame.fill_handle = Some(Rect { x: cx - h / 2.0 - 1.0, y: cy - h / 2.0 - 1.0, width: h, height: h });
            }
        }
        let overlay = grid::Overlay {
            marquee: self.copied.as_ref().filter(|c| c.sheet == self.sheet()).map(|c| [c.range.0, c.range.1, c.range.2, c.range.3]),
            fill_to: match self.fill {
                Some(Some(to)) => Some(ops::filled_range(self.selection().range, to)),
                _ => None,
            },
            fill_handle: self.frame.fill_handle,
        };

        let mut pc = PaintCtx::new();
        pc.root_plate(size.width, size.height);
        self.paint_bar(&mut pc, &face, marks.as_ref());
        let sel = self.selection();
        let active = format::active(&self.book.model, self.sheet(), sel.row, sel.col);
        let popover = self.frame.popover.map(|(r, _)| r);
        toolbar::paint(&mut pc, &self.frame.tools, &active, &face, popover);

        let well = self.frame.well;
        let radius = cce_ui::layout::plate_corner_radius();
        let relief = cce_ui::layout::control_relief();
        pc.well_floor(well, radius, &Material::root(), false);
        let sheet = self.sheet();
        let hide: Vec<Rect> = self.frame.editor.into_iter().chain(popover).collect();
        pc.clip_rounded(well, radius, |pc| {
            grid::paint_sheet(pc, &self.book.model, sheet, &self.view(), &face, &sel, &hide);
            grid::paint_overlay(pc, &self.view(), &face, &overlay);
            if let Some(m) = &marks {
                self.paint_editor(pc, &face, m);
            }
        });
        pc.well_rim(well, radius, relief);

        self.paint_tabs(&mut pc, &face);
        self.paint_popover(&mut pc, &face);
        if let Some(status) = &self.status {
            let x = self.frame.tabs.last().map_or(self.frame.well.x, |(r, _)| r.x + r.width + cce_ui::layout::control_gap() * 2.0);
            let y = self.frame.tabs.first().map_or(self.frame.well.y, |(r, _)| cce_ui::layout::center_text_y(r.y, r.height, face.size));
            let color = if status.error { [0xff, 0x8a, 0x80] } else { dim(face.text) };
            pc.text_with(status.text.clone(), x, y, face.size, color, Some(face.family.clone()), None);
        }
        Some(pc.finish())
    }

    fn display_list_text(&self) -> bool {
        true
    }
}

/// The in-cell editor's ground: opaque, so the cell's own value and any
/// overflow beneath it do not show through.
const EDITOR_FILL: [f32; 4] = [0.055, 0.058, 0.07, 1.0];

/// One edited field's text in `field` (its clip), the text starting at
/// `origin_x`: the selection under it, the composition underlined, the caret
/// when `caret`. A field with the caret reports it to the input method,
/// which is also how the shell knows text input is wanted.
fn paint_field(pc: &mut PaintCtx, field: Rect, origin_x: f32, marks: &Marks, face: &Face, color: [u8; 3], caret: bool) {
    let ty = cce_ui::layout::center_text_y(field.y, field.height, face.size);
    let (top, h) = (field.y + 3.0, (field.height - 6.0).max(1.0));
    let sel_fill = {
        let c = face.highlight;
        [c[0], c[1], c[2], 0.45]
    };
    pc.clip(field, |pc| {
        if let Some((a, b)) = marks.selection {
            pc.quad(Rect { x: origin_x + a, y: top, width: b - a, height: h }, sel_fill);
        }
        pc.text_with(marks.shown.clone(), origin_x, ty, face.size, color, Some(face.family.clone()), None);
        if let Some((a, b)) = marks.composition {
            pc.quad(Rect { x: origin_x + a, y: top + h - 1.0, width: b - a, height: 1.0 }, [1.0, 1.0, 1.0, 0.8]);
        }
    });
    if caret {
        let x = (origin_x + marks.caret).clamp(field.x, field.x + field.width);
        let rect = Rect { x, y: top, width: (face.hair * 2.0).max(1.0), height: h };
        pc.quad(rect, [1.0, 1.0, 1.0, 0.95]);
        cce_ui::ime::report_caret(rect.x, rect.y, rect.width, rect.height);
    }
}

/// The chords that act on the selected cells when nothing is being edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CellChord {
    Copy,
    Cut,
    Paste,
    /// ctrl++ (ctrl+shift+= on a US keyboard, reported either way): insert
    /// rows, or columns for a whole-column selection.
    Insert,
    /// ctrl+-: delete them.
    Delete,
    Bold,
    Italic,
    Underline,
}

fn cell_chord(event: &KeyEvent) -> Option<CellChord> {
    let Key::Character(c) = &event.logical_key else {
        return None;
    };
    if !event.ctrl || event.alt {
        return None;
    }
    Some(match c.to_ascii_lowercase().as_str() {
        "c" => CellChord::Copy,
        "x" => CellChord::Cut,
        "v" => CellChord::Paste,
        "+" => CellChord::Insert,
        "=" if event.shift => CellChord::Insert,
        "-" => CellChord::Delete,
        "b" => CellChord::Bold,
        "i" => CellChord::Italic,
        "u" => CellChord::Underline,
        _ => return None,
    })
}

fn contains(r: &Rect, x: f32, y: f32) -> bool {
    x >= r.x && y >= r.y && x < r.x + r.width && y < r.y + r.height
}

fn main() {
    env_logger::init();
    cce_ui::engine::run::<SheetsApp>();
}
