//! cce-sheets — spreadsheets for the cce desktop.
//!
//! The workbook is IronCalc's `UserModel`, which owns the cells, formulas,
//! styles and the selection; this app draws it and turns input into model
//! calls. Milestone 1 is a viewer: open .xlsx / .csv / .tsv, scroll, select
//! cells and ranges, switch sheets, and read a cell's formula in the bar.
//!
//! Keys: arrows / Enter / Tab move · shift extends · ctrl+arrow jumps to an
//! edge · PageUp/PageDown · ctrl+Home/End · ctrl+PageUp/PageDown change
//! sheet · ctrl+o open. Wheel scrolls (shift for horizontal); press, drag
//! and shift+press select; header presses select a whole column or row.

mod axis;
mod book;
mod grid;
mod view;

use std::collections::HashMap;
use std::path::PathBuf;

use cce_ui::engine::{Application, LogicalPosition, LogicalSize, WindowSettings};
use cce_ui::scene::layout::Rect;
use cce_ui::scene::paint::{ControlPlate, DisplayList, PaintCtx, PlateStance};
use cce_ui::scene::Material;
use cce_ui::widget::scroll_motion::{Bounds, ScrollMotion};
use cce_ui::widget::{ElementState, Key, KeyEvent, MouseButton, MouseScrollDelta, NamedKey};
use ironcalc::base::worksheet::NavigationDirection;

use book::{column_name, Book, SHEET_EXTS};
use grid::{dim, Face, Selection};
use view::{Hit, SheetGeom, View};

const WHEEL_LINE_PX: f32 = 48.0;
/// The formula bar's and the sheet tabs' height.
const BAR_H: f32 = 28.0;

#[derive(Debug, Clone)]
enum Message {}

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
}

const NO_RECT: Rect = Rect { x: 0.0, y: 0.0, width: 0.0, height: 0.0 };

impl Default for Frame {
    fn default() -> Self {
        Frame { address: NO_RECT, content: NO_RECT, well: NO_RECT, area: NO_RECT, header_w: 0.0, header_h: 0.0, tabs: Vec::new() }
    }
}

struct SheetsApp {
    book: Book,
    error: Option<String>,
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
    fn with_book(book: Book, error: Option<String>) -> Self {
        let sheet = book.model.get_selected_sheet();
        let geom = grid::sheet_geom(&book.model, sheet);
        SheetsApp {
            book,
            error,
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
            Ok(book) => *self = SheetsApp::with_book(book, None),
            Err(e) => self.error = Some(format!("{}: {e}", path.display())),
        }
    }

    fn open_dialog(&mut self) {
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

        let well_y = inset + BAR_H + gap;
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
        self.frame = Frame { address, content, well, area, header_w, header_h, tabs };
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
            Hit::Outside => return false,
        };
        done.is_ok()
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

    fn paint_bar(&self, pc: &mut PaintCtx, face: &Face) {
        let sel = self.selection();
        let [r1, c1, r2, c2] = sel.range;
        let address = if (r1, c1) == (r2, c2) {
            format!("{}{}", column_name(sel.col), sel.row)
        } else {
            format!("{}{}:{}{}", column_name(c1.min(c2)), r1.min(r2), column_name(c1.max(c2)), r1.max(r2))
        };
        let content = self.book.model.get_cell_content(self.sheet(), sel.row, sel.col).unwrap_or_default();
        let content = content.lines().next().unwrap_or("").to_string();
        let radius = cce_ui::layout::control_corner_radius();
        let relief = cce_ui::layout::control_relief();
        let text_in = cce_ui::layout::CONTROL_TEXT_INSET;
        for (rect, text) in [(self.frame.address, address), (self.frame.content, content)] {
            if rect.width <= 0.0 {
                continue;
            }
            pc.canvas_well(rect, radius, &Material::root(), relief, false);
            let ty = cce_ui::layout::center_text_y(rect.y, rect.height, face.size);
            let bounds = [rect.x + text_in, rect.y, rect.x + rect.width - text_in, rect.y + rect.height];
            pc.text_with(text, rect.x + text_in, ty, face.size, face.text, Some(face.family.clone()), Some(bounds));
        }
    }

    /// The sheet tabs as a ButtonStrip draws its recessed style: one well
    /// carved around the run, the selected tab a plateau raised back out of
    /// its floor; flat, the selected tab is a fill.
    fn paint_tabs(&self, pc: &mut PaintCtx, face: &Face) {
        let (Some((first, _)), Some((last, _))) = (self.frame.tabs.first(), self.frame.tabs.last()) else {
            return;
        };
        let strip = Rect { x: first.x, y: first.y, width: last.x + last.width - first.x, height: first.height };
        let radius = cce_ui::layout::button_corner_radius();
        let relief = cce_ui::layout::control_relief();
        let depth = if relief {
            let depth = cce_ui::layout::bevel_width().min(strip.height * 0.2);
            let (well, radii) = cce_ui::layout::carve_inside(strip, (radius, radius, radius, radius), depth);
            pc.recess(well, radii, depth);
            depth
        } else {
            0.0
        };
        let cur = self.sheet();
        let text_in = cce_ui::layout::CONTROL_TEXT_INSET;
        let names: HashMap<u32, String> = self.visible_sheets().into_iter().collect();
        for (rect, i) in &self.frame.tabs {
            let selected = *i == cur;
            let inset = depth * 0.5;
            let seg = Rect { x: rect.x + inset, y: rect.y + inset, width: (rect.width - 2.0 * inset).max(0.0), height: (rect.height - 2.0 * inset).max(0.0) };
            let seg_r = (radius - inset).max(0.0);
            if selected {
                pc.rounded_rect(seg, seg_r, (true, true, true, true), cce_ui::colors::PANEL_MENU_FOCUSED);
                if relief {
                    pc.control_plate(&ControlPlate::control(seg, seg_r, PlateStance::Raised, None).with_depth(depth));
                }
            }
            let ty = cce_ui::layout::center_text_y(rect.y, rect.height, face.size);
            let color = if selected { face.text } else { dim(face.text) };
            let name = names.get(i).cloned().unwrap_or_default();
            pc.text_with(name, rect.x + text_in, ty, face.size, color, Some(face.family.clone()), None);
        }
    }
}

impl Application for SheetsApp {
    type Message = Message;

    fn create(_sender: cce_ui::engine::AppSender<Self::Message>) -> Self {
        let arg: Option<PathBuf> = std::env::args_os().nth(1).map(PathBuf::from);
        match arg {
            Some(path) => match Book::open(&path) {
                Ok(book) => SheetsApp::with_book(book, None),
                Err(e) => SheetsApp::with_book(Book::untitled(), Some(format!("{}: {e}", path.display()))),
            },
            None => SheetsApp::with_book(Book::untitled(), None),
        }
    }

    fn settings(&self) -> WindowSettings {
        WindowSettings {
            title: format!("{} — Sheets", self.book.display_name()),
            app_id: "cce-sheets".to_string(),
            width: 1100,
            height: 760,
            fullscreen: false,
            min_size: Some((420, 300)),
        }
    }

    fn update(&mut self, msg: Self::Message, _needs_rebuild: &mut bool, _exit: &mut bool) {
        match msg {}
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
        if button != MouseButton::Left {
            return None;
        }
        if state == ElementState::Released {
            self.selecting = false;
            return None;
        }
        if let Some(&(_, sheet)) = self.frame.tabs.iter().find(|(r, _)| contains(r, pos.x, pos.y)) {
            let _ = self.book.model.set_selected_sheet(sheet);
            self.sync_sheet();
            *needs_rebuild = true;
            return None;
        }
        let hit = self.view().hit(pos.x, pos.y);
        if self.select_hit(hit, self.shift) {
            self.selecting = matches!(hit, Hit::Cell { .. });
            *needs_rebuild = true;
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
        let handled = match &event.logical_key {
            Key::Character(c) if self.ctrl && c.eq_ignore_ascii_case("o") => {
                self.open_dialog();
                true
            }
            key => self.navigate(key),
        };
        if handled {
            *needs_rebuild = true;
        }
        None
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

        let mut pc = PaintCtx::new();
        pc.root_plate(size.width, size.height);
        self.paint_bar(&mut pc, &face);

        let well = self.frame.well;
        let radius = cce_ui::layout::plate_corner_radius();
        let relief = cce_ui::layout::control_relief();
        pc.well_floor(well, radius, &Material::root(), false);
        let sel = self.selection();
        let sheet = self.sheet();
        pc.clip_rounded(well, radius, |pc| {
            grid::paint_sheet(pc, &self.book.model, sheet, &self.view(), &face, &sel);
        });
        pc.well_rim(well, radius, relief);

        self.paint_tabs(&mut pc, &face);
        if let Some(err) = &self.error {
            let x = self.frame.tabs.last().map_or(self.frame.well.x, |(r, _)| r.x + r.width + cce_ui::layout::control_gap() * 2.0);
            let y = self.frame.tabs.first().map_or(self.frame.well.y, |(r, _)| cce_ui::layout::center_text_y(r.y, r.height, face.size));
            pc.text_with(err.clone(), x, y, face.size, [0xff, 0x8a, 0x80], Some(face.family.clone()), None);
        }
        Some(pc.finish())
    }

    fn display_list_text(&self) -> bool {
        true
    }
}

fn contains(r: &Rect, x: f32, y: f32) -> bool {
    x >= r.x && y >= r.y && x < r.x + r.width && y < r.y + r.height
}

fn main() {
    env_logger::init();
    cce_ui::engine::run::<SheetsApp>();
}
