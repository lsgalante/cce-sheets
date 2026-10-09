//! Painting a sheet: cell fills, gridlines, values, the selection, the
//! headers and the frozen-pane dividers, for the cells on screen only.
//!
//! **Colours.** Workbooks are authored for black text on white paper, and
//! this grid is a dark well. A cell keeps the colours it was given where it
//! has its own fill; on the bare grid, text whose colour is unset or too dark
//! to read takes the DE's label colour instead.
//!
//! **Text** is set in the DE's label font, which is monospace, so a run's
//! width is its length times one measured advance (`char_w`). Text overflows
//! into empty neighbours as in Excel: left-aligned to the right, right-aligned
//! to the left. A number that does not fit shows `###`.

use cce_ui::scene::layout::Rect;
use cce_ui::scene::paint::{PaintCtx, TextAttrs};
use ironcalc::base::types::{CellType, HorizontalAlignment};
use ironcalc::base::UserModel;

use crate::axis::Axis;
use crate::book::column_name;
use crate::view::{SheetGeom, View};

/// Room between a cell's wall and its text.
const CELL_PAD: f32 = 3.0;
/// Header strip tint, over the well floor.
const HEADER_TINT: [f32; 4] = [1.0, 1.0, 1.0, 0.04];
const GRIDLINE: [f32; 4] = [1.0, 1.0, 1.0, 0.08];
const HEADER_LINE: [f32; 4] = [1.0, 1.0, 1.0, 0.14];
const FROZEN_LINE: [f32; 4] = [1.0, 1.0, 1.0, 0.35];
/// Below this relative luminance a font colour on the bare grid is swapped
/// for the label colour.
const MIN_TEXT_LUMA: f32 = 0.18;

/// Rows and columns as IronCalc sizes them, in px; the view uses them as
/// logical px.
pub fn sheet_geom(model: &UserModel, sheet: u32) -> SheetGeom {
    let m = model.get_model();
    let ws = &m.workbook.worksheets[sheet as usize];
    let col_w = |c: i32| model.get_column_width(sheet, c).unwrap_or(0.0);
    let row_h = |r: i32| model.get_row_height(sheet, r).unwrap_or(0.0);
    let cols = Axis::new(
        col_w(LAST_COLUMN),
        LAST_COLUMN,
        ws.cols.iter().filter(|c| c.custom_width || c.hidden).map(|c| (c.min, c.max, col_w(c.min))),
    );
    let rows = Axis::new(
        default_row_height(model, sheet),
        LAST_ROW,
        ws.rows.iter().filter(|r| r.custom_height || r.hidden).map(|r| (r.r, r.r, row_h(r.r))),
    );
    SheetGeom {
        cols,
        rows,
        frozen_cols: model.get_frozen_columns_count(sheet).unwrap_or(0),
        frozen_rows: model.get_frozen_rows_count(sheet).unwrap_or(0),
    }
}

pub const LAST_COLUMN: i32 = 16_384;
pub const LAST_ROW: i32 = 1_048_576;

/// The height of a row nobody sized: ask for one the sheet has no record of.
fn default_row_height(model: &UserModel, sheet: u32) -> f64 {
    let ws = &model.get_model().workbook.worksheets[sheet as usize];
    let free = (1..=LAST_ROW).rev().find(|r| !ws.rows.iter().any(|row| row.r == *r)).unwrap_or(LAST_ROW);
    model.get_row_height(sheet, free).unwrap_or(25.0)
}

/// The furthest row and column with anything in them (1, 1 for an empty
/// sheet) — what the scrollbars' range is built from.
pub fn used_extent(model: &UserModel, sheet: u32) -> (i32, i32) {
    let ws = &model.get_model().workbook.worksheets[sheet as usize];
    let d = ws.dimension();
    (d.max_row.max(1), d.max_column.max(1))
}

/// What the painter needs to know about the text face.
#[derive(Debug, Clone)]
pub struct Face {
    pub family: String,
    pub size: f32,
    /// One glyph's advance (the face is monospace).
    pub char_w: f32,
    pub text: [u8; 3],
    pub highlight: [f32; 4],
    /// One device pixel, in logical px: the gridline weight.
    pub hair: f32,
}

impl Face {
    pub fn current(scale: f64) -> Face {
        let (family, size) = cce_ui::layout::control_label_font_parsed();
        // The measurer reports a run's INK extent, which falls short of its
        // advance by the margins of its end glyphs; two runs of one glyph
        // have the same margins, so their difference is whole advances.
        let ink = |n: usize| cce_ui::widget::display::measure_text_width(&"0".repeat(n), &family, size);
        let char_w = ((ink(41) - ink(1)) / 40.0).max(1.0);
        Face {
            family,
            size,
            char_w,
            text: cce_ui::colors::control_label_color_u8(),
            highlight: cce_ui::colors::highlight_primary_color(),
            hair: (1.0 / scale.max(1.0)) as f32,
        }
    }

    pub fn text_width(&self, s: &str) -> f32 {
        s.chars().count() as f32 * self.char_w
    }
}

/// One on-screen cell, as it will be painted.
struct CellPaint {
    row: i32,
    col: i32,
    rect: Rect,
    text: String,
    align: Align,
    numeric: bool,
    /// The cell's number format is General: a number too wide drops
    /// decimals instead of turning to `###`.
    general: bool,
    color: [u8; 3],
    fill: Option<[f32; 4]>,
    attrs: TextAttrs,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Align {
    Left,
    Center,
    Right,
}

/// The selected cell and range, as IronCalc's selected view reports them.
pub struct Selection {
    pub row: i32,
    pub col: i32,
    /// `[row1, col1, row2, col2]`.
    pub range: [i32; 4],
}

pub fn paint_sheet(pc: &mut PaintCtx, model: &UserModel, sheet: u32, view: &View, face: &Face, sel: &Selection) {
    let area = view.area;
    let body = view.body();
    let cols = view.cols();
    let rows = view.rows();
    let col_list: Vec<i32> = cols.iter().filter(|&c| view.geom.cols.size(c) > 0.0).collect();
    let row_list: Vec<i32> = rows.iter().filter(|&r| view.geom.rows.size(r) > 0.0).collect();

    let grid_lines = model.get_show_grid_lines(sheet).unwrap_or(true);
    let cells = collect_cells(model, sheet, view, face, &row_list, &col_list);
    let [r1, c1, r2, c2] = normalized(sel.range);
    let in_range = |r: i32, c: i32| r >= r1 && r <= r2 && c >= c1 && c <= c2;

    pc.clip(body, |pc| {
        // Fills, then the range wash, then gridlines over both, then text.
        for cell in &cells {
            if let Some(fill) = cell.fill {
                clipped_to_cell_band(pc, view, cell.row, cell.col, |pc| pc.quad(cell.rect, fill));
            }
        }
        if (r1, c1) != (r2, c2) {
            let wash = with_alpha(face.highlight, 0.16);
            for &r in &row_list {
                for &c in &col_list {
                    if in_range(r, c) && (r, c) != (sel.row, sel.col) {
                        clipped_to_cell_band(pc, view, r, c, |pc| pc.quad(view.cell_rect(r, c), wash));
                    }
                }
            }
        }
        if grid_lines {
            for &c in &col_list {
                let (x0, x1) = view.col_band(c);
                let x = view.col_x(c) as f32 + view.geom.cols.size(c) as f32 - face.hair;
                if x >= x0 && x < x1 {
                    pc.quad(Rect { x, y: body.y, width: face.hair, height: body.height }, GRIDLINE);
                }
            }
            for &r in &row_list {
                let (y0, y1) = view.row_band(r);
                let y = view.row_y(r) as f32 + view.geom.rows.size(r) as f32 - face.hair;
                if y >= y0 && y < y1 {
                    pc.quad(Rect { x: body.x, y, width: body.width, height: face.hair }, GRIDLINE);
                }
            }
        }
        paint_text(pc, view, face, &cells, &col_list);
        paint_cursor(pc, view, face, sel);
    });

    paint_headers(pc, view, face, &row_list, &col_list, (r1, c1, r2, c2));

    // The frozen edges, across headers and cells.
    let g = view.geom;
    if g.frozen_cols > 0 {
        let x = body.x + g.frozen_w() as f32;
        if x < area.x + area.width {
            pc.quad(Rect { x, y: area.y, width: face.hair * 2.0, height: area.height }, FROZEN_LINE);
        }
    }
    if g.frozen_rows > 0 {
        let y = body.y + g.frozen_h() as f32;
        if y < area.y + area.height {
            pc.quad(Rect { x: area.x, y, width: area.width, height: face.hair * 2.0 }, FROZEN_LINE);
        }
    }
}

fn collect_cells(model: &UserModel, sheet: u32, view: &View, face: &Face, rows: &[i32], cols: &[i32]) -> Vec<CellPaint> {
    let m = model.get_model();
    let mut out = Vec::new();
    for &row in rows {
        for &col in cols {
            let text = model.get_formatted_cell_value(sheet, row, col).unwrap_or_default();
            let style = m.get_style_for_cell(sheet, row, col).unwrap_or_default();
            let fill = hex_srgb(&model.resolve_color(&style.fill.color));
            if text.is_empty() && fill.is_none() {
                continue;
            }
            let kind = model.get_cell_type(sheet, row, col).ok();
            let numeric = matches!(kind, Some(CellType::Number)) && !text.is_empty();
            let align = match style.alignment.as_ref().map(|a| &a.horizontal) {
                Some(HorizontalAlignment::Left) => Align::Left,
                Some(HorizontalAlignment::Right) => Align::Right,
                Some(HorizontalAlignment::Center | HorizontalAlignment::CenterContinuous) => Align::Center,
                _ => match kind {
                    Some(CellType::Number) => Align::Right,
                    Some(CellType::LogicalValue | CellType::ErrorValue) => Align::Center,
                    _ => Align::Left,
                },
            };
            let font = hex_srgb(&model.resolve_color(&style.font.color));
            let color = match (font, fill) {
                // On its own fill a cell keeps its own colours (black when unset).
                (Some(c), Some(_)) => to_u8(c),
                (None, Some(_)) => [0, 0, 0],
                (Some(c), None) if luma(c) >= MIN_TEXT_LUMA => to_u8(c),
                _ => face.text,
            };
            out.push(CellPaint {
                row,
                col,
                rect: view.cell_rect(row, col),
                // One line: wrapped text is a later milestone.
                text: text.lines().next().unwrap_or("").to_string(),
                align,
                numeric,
                general: style.num_fmt.eq_ignore_ascii_case("general"),
                color,
                fill: fill.map(|[r, g, b]| [srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b), 1.0]),
                attrs: TextAttrs { italic: style.font.i, weight: style.font.b.then_some(700), ..Default::default() },
            });
        }
    }
    out
}

fn paint_text(pc: &mut PaintCtx, view: &View, face: &Face, cells: &[CellPaint], cols: &[i32]) {
    let filled: std::collections::HashSet<(i32, i32)> = cells.iter().filter(|c| !c.text.is_empty()).map(|c| (c.row, c.col)).collect();
    let occupied = |row: i32, col: i32| filled.contains(&(row, col));
    for cell in cells.iter().filter(|c| !c.text.is_empty()) {
        let (bx0, bx1) = view.col_band(cell.col);
        let (by0, by1) = view.row_band(cell.row);
        let inner = cell.rect.width - 2.0 * CELL_PAD;
        let mut text = cell.text.clone();
        let mut width = face.text_width(&text);
        if cell.numeric && width > inner {
            let n = (inner / face.char_w).floor().max(1.0) as usize;
            text = cell.general.then(|| fit_general(&text, n)).flatten().unwrap_or_else(|| "#".repeat(n));
            width = face.text_width(&text);
        }
        // How far the text may run: its own cell, plus empty neighbours in
        // the direction it reads, within the same frozen band.
        let (mut left, mut right) = (cell.rect.x, cell.rect.x + cell.rect.width);
        let pos = cols.iter().position(|&c| c == cell.col).unwrap_or(0);
        let same_band = |c: i32| view.col_band(c) == (bx0, bx1);
        if width > inner && !cell.numeric {
            match cell.align {
                Align::Left => {
                    for &c in cols[pos + 1..].iter().take_while(|&&c| same_band(c) && !occupied(cell.row, c)) {
                        right = view.col_x(c) as f32 + view.geom.cols.size(c) as f32;
                        if right - cell.rect.x - CELL_PAD >= width {
                            break;
                        }
                    }
                }
                Align::Right => {
                    for &c in cols[..pos].iter().rev().take_while(|&&c| same_band(c) && !occupied(cell.row, c)) {
                        left = view.col_x(c) as f32;
                        if cell.rect.x + cell.rect.width - CELL_PAD - left >= width {
                            break;
                        }
                    }
                }
                Align::Center => {}
            }
        }
        let x = match cell.align {
            Align::Left => cell.rect.x + CELL_PAD,
            Align::Right => cell.rect.x + cell.rect.width - CELL_PAD - width,
            Align::Center => cell.rect.x + (cell.rect.width - width) / 2.0,
        };
        let y = cce_ui::layout::center_text_y(cell.rect.y, cell.rect.height, face.size);
        let clip_l = left.max(bx0);
        let clip_r = (right - CELL_PAD * 0.5).min(bx1);
        let clip_t = cell.rect.y.max(by0);
        let clip_b = (cell.rect.y + cell.rect.height).min(by1);
        if clip_r <= clip_l || clip_b <= clip_t {
            continue;
        }
        pc.text_attrs(text, x, y, face.size, cell.color, Some(face.family.clone()), Some([clip_l, clip_t, clip_r, clip_b]), cell.attrs);
    }
}

/// A General-format number in at most `max` characters, as Excel shows one
/// in a narrow column: fewer decimals (rounded), then scientific notation;
/// `None` when not even that fits (the caller shows `###`).
pub fn fit_general(text: &str, max: usize) -> Option<String> {
    if text.chars().count() <= max {
        return Some(text.to_string());
    }
    let v: f64 = text.parse().ok()?;
    let trim = |s: String| if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    let whole = format!("{v:.0}");
    // Decimals only while they keep a significant digit: a small number
    // rounded to 0 says nothing, so it goes scientific instead.
    if whole.len() <= max && (v == 0.0 || v.abs() >= 1.0 || -v.abs().log10().floor() <= max.saturating_sub(2) as f64) {
        for d in (0..max.saturating_sub(whole.len())).rev() {
            let s = trim(format!("{v:.d$}"));
            if s.len() <= max {
                return Some(s);
            }
        }
    }
    for d in (0..max).rev() {
        let s = format!("{v:.d$e}");
        let (mant, exp) = s.split_once('e')?;
        let exp: i32 = exp.parse().ok()?;
        let s = format!("{}E{}{:02}", trim(mant.to_string()), if exp < 0 { '-' } else { '+' }, exp.abs());
        if s.len() <= max {
            return Some(s);
        }
    }
    None
}

/// The selected cell's frame, in its quadrant.
fn paint_cursor(pc: &mut PaintCtx, view: &View, face: &Face, sel: &Selection) {
    let r = view.cell_rect(sel.row, sel.col);
    let t = 2.0;
    // The highlight lifted a third toward white: the frame has to read on a
    // filled cell as well as on the bare well.
    let h = face.highlight;
    let c = [h[0] + (1.0 - h[0]) / 3.0, h[1] + (1.0 - h[1]) / 3.0, h[2] + (1.0 - h[2]) / 3.0, 1.0];
    clipped_to_cell_band(pc, view, sel.row, sel.col, |pc| {
        pc.quad(Rect { x: r.x, y: r.y, width: r.width, height: t }, c);
        pc.quad(Rect { x: r.x, y: r.y + r.height - t, width: r.width, height: t }, c);
        pc.quad(Rect { x: r.x, y: r.y, width: t, height: r.height }, c);
        pc.quad(Rect { x: r.x + r.width - t, y: r.y, width: t, height: r.height }, c);
    });
}

fn paint_headers(pc: &mut PaintCtx, view: &View, face: &Face, rows: &[i32], cols: &[i32], range: (i32, i32, i32, i32)) {
    let area = view.area;
    let body = view.body();
    let (r1, c1, r2, c2) = range;
    let col_strip = Rect { x: body.x, y: area.y, width: body.width, height: view.header_h };
    let row_strip = Rect { x: area.x, y: body.y, width: view.header_w, height: body.height };
    pc.quad(Rect { x: area.x, y: area.y, width: area.width, height: view.header_h }, HEADER_TINT);
    pc.quad(row_strip, HEADER_TINT);
    let active = with_alpha(face.highlight, 0.22);
    let dim = dim(face.text);

    pc.clip(col_strip, |pc| {
        for &c in cols {
            let (x0, x1) = view.col_band(c);
            let x = view.col_x(c) as f32;
            let w = view.geom.cols.size(c) as f32;
            let cell = Rect { x, y: area.y, width: w, height: view.header_h };
            let band = Rect { x: x0, y: area.y, width: x1 - x0, height: view.header_h };
            pc.clip(band, |pc| {
                if c >= c1 && c <= c2 {
                    pc.quad(cell, active);
                }
                pc.quad(Rect { x: x + w - face.hair, y: area.y, width: face.hair, height: view.header_h }, HEADER_LINE);
                let name = column_name(c);
                let tx = x + (w - face.text_width(&name)) / 2.0;
                let ty = cce_ui::layout::center_text_y(area.y, view.header_h, face.size);
                let color = if c >= c1 && c <= c2 { face.text } else { dim };
                pc.text_with(name, tx, ty, face.size, color, Some(face.family.clone()), Some([x0, area.y, x1, area.y + view.header_h]));
            });
        }
    });
    pc.clip(row_strip, |pc| {
        for &r in rows {
            let (y0, y1) = view.row_band(r);
            let y = view.row_y(r) as f32;
            let h = view.geom.rows.size(r) as f32;
            let cell = Rect { x: area.x, y, width: view.header_w, height: h };
            let band = Rect { x: area.x, y: y0, width: view.header_w, height: y1 - y0 };
            pc.clip(band, |pc| {
                if r >= r1 && r <= r2 {
                    pc.quad(cell, active);
                }
                pc.quad(Rect { x: area.x, y: y + h - face.hair, width: view.header_w, height: face.hair }, HEADER_LINE);
                let label = r.to_string();
                let tx = area.x + view.header_w - CELL_PAD * 2.0 - face.text_width(&label);
                let ty = cce_ui::layout::center_text_y(y, h, face.size);
                let color = if r >= r1 && r <= r2 { face.text } else { dim };
                pc.text_with(label, tx, ty, face.size, color, Some(face.family.clone()), Some([area.x, y0, area.x + view.header_w, y1]));
            });
        }
    });
    // The headers' inner edges.
    pc.quad(Rect { x: area.x, y: body.y - face.hair, width: area.width, height: face.hair }, HEADER_LINE);
    pc.quad(Rect { x: body.x - face.hair, y: area.y, width: face.hair, height: area.height }, HEADER_LINE);
}

/// The row-header strip's width for the rows on screen: the widest label
/// plus its padding.
pub fn row_header_width(face: &Face, last_row: i32) -> f32 {
    let digits = last_row.max(1).to_string().len().max(3);
    digits as f32 * face.char_w + CELL_PAD * 4.0
}

fn clipped_to_cell_band(pc: &mut PaintCtx, view: &View, row: i32, col: i32, f: impl FnOnce(&mut PaintCtx)) {
    let (x0, x1) = view.col_band(col);
    let (y0, y1) = view.row_band(row);
    if x1 > x0 && y1 > y0 {
        pc.clip(Rect { x: x0, y: y0, width: x1 - x0, height: y1 - y0 }, f);
    }
}

fn normalized([a, b, c, d]: [i32; 4]) -> [i32; 4] {
    [a.min(c), b.min(d), a.max(c), b.max(d)]
}

/// A label colour taken down a quarter: headers and tabs not selected.
pub fn dim(c: [u8; 3]) -> [u8; 3] {
    [c[0] / 4 * 3, c[1] / 4 * 3, c[2] / 4 * 3]
}

fn with_alpha(c: [f32; 4], a: f32) -> [f32; 4] {
    [c[0], c[1], c[2], a]
}

/// `#rrggbb` as sRGB in 0..1; `None` for anything else (an unset colour
/// resolves to the empty string).
fn hex_srgb(s: &str) -> Option<[f32; 3]> {
    let b = cce_ui::colors::parse_hex_bytes(s)?;
    Some([b[0] as f32 / 255.0, b[1] as f32 / 255.0, b[2] as f32 / 255.0])
}

fn to_u8(c: [f32; 3]) -> [u8; 3] {
    [(c[0] * 255.0).round() as u8, (c[1] * 255.0).round() as u8, (c[2] * 255.0).round() as u8]
}

fn srgb_to_linear(c: f32) -> f32 {
    cce_ui::colors::srgb_to_linear(c)
}

/// Relative luminance of an sRGB colour.
fn luma(c: [f32; 3]) -> f32 {
    0.2126 * srgb_to_linear(c[0]) + 0.7152 * srgb_to_linear(c[1]) + 0.0722 * srgb_to_linear(c[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_follows_the_workbook() {
        let mut m = UserModel::new_empty("t", "en", "UTC", "en").unwrap();
        m.set_columns_width(0, 2, 2, 200.0).unwrap();
        m.set_rows_height(0, 3, 3, 50.0).unwrap();
        m.set_frozen_rows_count(0, 1).unwrap();
        let g = sheet_geom(&m, 0);
        let (dw, dh) = (g.cols.size(1), g.rows.size(1));
        assert_eq!(g.cols.size(2), 200.0);
        assert_eq!(g.cols.start(3), dw + 200.0);
        assert_eq!(g.rows.size(3), 50.0);
        assert_eq!(g.rows.start(4), 2.0 * dh + 50.0);
        assert_eq!(g.frozen_rows, 1);
    }

    #[test]
    fn extent_is_the_used_range() {
        let mut m = UserModel::new_empty("t", "en", "UTC", "en").unwrap();
        assert_eq!(used_extent(&m, 0), (1, 1));
        m.set_user_input(0, 40, 7, "x").unwrap();
        assert_eq!(used_extent(&m, 0), (40, 7));
    }

    #[test]
    fn general_numbers_shed_decimals_before_hashes() {
        assert_eq!(fit_general("3.46410161513775", 9).as_deref(), Some("3.4641016"));
        assert_eq!(fit_general("3.46410161513775", 4).as_deref(), Some("3.46"));
        assert_eq!(fit_general("2.999", 3).as_deref(), Some("3"));
        assert_eq!(fit_general("42", 5).as_deref(), Some("42"));
        assert_eq!(fit_general("123456789012345", 8).as_deref(), Some("1.23E+14"));
        assert_eq!(fit_general("0.000012345", 6).as_deref(), Some("1E-05"));
        assert_eq!(fit_general("0.0512", 4).as_deref(), Some("0.05"));
        assert_eq!(fit_general("-123456789", 3), None);
        assert_eq!(fit_general("$1,200.00", 4), None);
    }

    #[test]
    fn dark_text_on_the_bare_grid_reads_as_the_label_colour() {
        assert!(luma([0.0, 0.0, 0.0]) < MIN_TEXT_LUMA);
        assert!(luma([1.0, 0.4, 0.4]) >= MIN_TEXT_LUMA);
        assert_eq!(hex_srgb(""), None);
        assert_eq!(hex_srgb("#FF0000"), Some([1.0, 0.0, 0.0]));
    }
}
