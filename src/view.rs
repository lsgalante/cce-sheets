//! Where a sheet's cells land on screen: headers, frozen panes, scrolling and
//! hit testing. Pure geometry over two [`Axis`] values, so it is tested
//! without a window.
//!
//! The cell area is split by the frozen rows and columns into four
//! quadrants. The frozen ones never scroll on their axis; the rest scroll by
//! `scroll`, which is measured from the frozen edge (0 shows the first
//! unfrozen row/column right after the frozen ones).

use cce_ui::scene::layout::Rect;

use crate::axis::Axis;

/// A sheet's measurements: what the view needs from the model.
#[derive(Debug, Clone, PartialEq)]
pub struct SheetGeom {
    pub cols: Axis,
    pub rows: Axis,
    pub frozen_cols: i32,
    pub frozen_rows: i32,
}

impl SheetGeom {
    pub fn frozen_w(&self) -> f64 {
        self.cols.start(self.frozen_cols + 1)
    }

    pub fn frozen_h(&self) -> f64 {
        self.rows.start(self.frozen_rows + 1)
    }
}

/// What a point on the grid is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Cell { row: i32, col: i32 },
    ColHeader(i32),
    RowHeader(i32),
    Corner,
    Outside,
}

/// The cells and headers of one axis that are on screen: the frozen span
/// (`1..=frozen`, possibly empty) and the scrolled span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub frozen: i32,
    pub first: i32,
    pub last: i32,
}

impl Span {
    pub fn iter(self) -> impl Iterator<Item = i32> {
        (1..=self.frozen).chain(self.first..=self.last)
    }
}

/// A sheet laid into a rect of the window.
#[derive(Debug, Clone, Copy)]
pub struct View<'a> {
    pub geom: &'a SheetGeom,
    /// Headers and cells together.
    pub area: Rect,
    pub header_w: f32,
    pub header_h: f32,
    /// Scroll of the unfrozen quadrant, px.
    pub scroll: (f64, f64),
}

impl<'a> View<'a> {
    /// The cells' rect: the area less the headers.
    pub fn body(&self) -> Rect {
        Rect {
            x: self.area.x + self.header_w,
            y: self.area.y + self.header_h,
            width: (self.area.width - self.header_w).max(0.0),
            height: (self.area.height - self.header_h).max(0.0),
        }
    }

    /// How far the scrolled quadrant can show along x and y: the body less
    /// the frozen panes.
    pub fn scroll_viewport(&self) -> (f64, f64) {
        let b = self.body();
        ((b.width as f64 - self.geom.frozen_w()).max(0.0), (b.height as f64 - self.geom.frozen_h()).max(0.0))
    }

    /// Screen x of column `c`'s left edge.
    pub fn col_x(&self, c: i32) -> f64 {
        let x = self.body().x as f64 + self.geom.cols.start(c);
        if c <= self.geom.frozen_cols { x } else { x - self.scroll.0 }
    }

    /// Screen y of row `r`'s top edge.
    pub fn row_y(&self, r: i32) -> f64 {
        let y = self.body().y as f64 + self.geom.rows.start(r);
        if r <= self.geom.frozen_rows { y } else { y - self.scroll.1 }
    }

    pub fn cell_rect(&self, row: i32, col: i32) -> Rect {
        Rect {
            x: self.col_x(col) as f32,
            y: self.row_y(row) as f32,
            width: self.geom.cols.size(col) as f32,
            height: self.geom.rows.size(row) as f32,
        }
    }

    /// The columns on screen.
    pub fn cols(&self) -> Span {
        let g = self.geom;
        let (vw, _) = self.scroll_viewport();
        span(&g.cols, g.frozen_cols, g.frozen_w(), self.scroll.0, vw)
    }

    /// The rows on screen.
    pub fn rows(&self) -> Span {
        let g = self.geom;
        let (_, vh) = self.scroll_viewport();
        span(&g.rows, g.frozen_rows, g.frozen_h(), self.scroll.1, vh)
    }

    /// The clip for column `c`'s cells: the frozen columns' band or the
    /// scrolled band, in x; the whole body in y.
    pub fn col_band(&self, c: i32) -> (f32, f32) {
        let b = self.body();
        let split = b.x + self.geom.frozen_w() as f32;
        if c <= self.geom.frozen_cols { (b.x, split.min(b.x + b.width)) } else { (split.min(b.x + b.width), b.x + b.width) }
    }

    /// The same for row `r`, in y.
    pub fn row_band(&self, r: i32) -> (f32, f32) {
        let b = self.body();
        let split = b.y + self.geom.frozen_h() as f32;
        if r <= self.geom.frozen_rows { (b.y, split.min(b.y + b.height)) } else { (split.min(b.y + b.height), b.y + b.height) }
    }

    pub fn hit(&self, px: f32, py: f32) -> Hit {
        let a = self.area;
        if px < a.x || py < a.y || px >= a.x + a.width || py >= a.y + a.height {
            return Hit::Outside;
        }
        let b = self.body();
        let in_cols = px >= b.x;
        let in_rows = py >= b.y;
        let col = || axis_hit(&self.geom.cols, self.geom.frozen_cols, self.geom.frozen_w(), (px - b.x) as f64, self.scroll.0);
        let row = || axis_hit(&self.geom.rows, self.geom.frozen_rows, self.geom.frozen_h(), (py - b.y) as f64, self.scroll.1);
        match (in_cols, in_rows) {
            (false, false) => Hit::Corner,
            (true, false) => Hit::ColHeader(col()),
            (false, true) => Hit::RowHeader(row()),
            (true, true) => Hit::Cell { row: row(), col: col() },
        }
    }

    /// The scroll that brings cell (`row`, `col`) wholly into view (as much
    /// of it as fits), moving as little as possible. Frozen cells are always
    /// in view on their axis.
    pub fn scroll_to_show(&self, row: i32, col: i32) -> (f64, f64) {
        let g = self.geom;
        let (vw, vh) = self.scroll_viewport();
        let x = if col <= g.frozen_cols {
            self.scroll.0
        } else {
            reveal(self.scroll.0, g.cols.start(col) - g.frozen_w(), g.cols.end(col) - g.frozen_w(), vw)
        };
        let y = if row <= g.frozen_rows {
            self.scroll.1
        } else {
            reveal(self.scroll.1, g.rows.start(row) - g.frozen_h(), g.rows.end(row) - g.frozen_h(), vh)
        };
        (x, y)
    }
}

fn span(axis: &Axis, frozen: i32, frozen_len: f64, scroll: f64, viewport: f64) -> Span {
    let first = axis.index_at(frozen_len + scroll).max(frozen + 1);
    let last = if viewport <= 0.0 { first - 1 } else { axis.index_at(frozen_len + scroll + viewport - 0.001) };
    Span { frozen, first, last: last.max(first - 1) }
}

/// The index under `pos` (px from the body's leading edge) on an axis.
fn axis_hit(axis: &Axis, frozen: i32, frozen_len: f64, pos: f64, scroll: f64) -> i32 {
    if pos < frozen_len {
        axis.index_at(pos).min(frozen.max(1))
    } else {
        axis.index_at(pos + scroll).max(frozen + 1)
    }
}

/// The scroll showing the span `start..end` (scroll coordinates) in a
/// viewport of `len`, given the current `scroll`.
fn reveal(scroll: f64, start: f64, end: f64, len: f64) -> f64 {
    if start < scroll {
        start
    } else if end > scroll + len {
        (end - len).min(start)
    } else {
        scroll
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geom(fr: i32, fc: i32) -> SheetGeom {
        SheetGeom { cols: Axis::new(100.0, 1000, []), rows: Axis::new(20.0, 100_000, []), frozen_cols: fc, frozen_rows: fr }
    }

    fn view(g: &SheetGeom, scroll: (f64, f64)) -> View<'_> {
        View { geom: g, area: Rect { x: 10.0, y: 50.0, width: 440.0, height: 220.0 }, header_w: 40.0, header_h: 20.0, scroll }
    }

    #[test]
    fn spans_without_frozen_panes() {
        let g = geom(0, 0);
        let v = view(&g, (0.0, 0.0));
        // Body is 400 x 200: four columns, ten rows exactly.
        assert_eq!(v.cols(), Span { frozen: 0, first: 1, last: 4 });
        assert_eq!(v.rows(), Span { frozen: 0, first: 1, last: 10 });
        let v = view(&g, (150.0, 30.0));
        assert_eq!(v.cols(), Span { frozen: 0, first: 2, last: 6 });
        assert_eq!(v.rows(), Span { frozen: 0, first: 2, last: 12 });
        // Column 2 starts 100 in; scrolled 150, it begins half a column off the body's edge.
        assert_eq!(v.col_x(2), 50.0 + 100.0 - 150.0);
    }

    #[test]
    fn frozen_panes_hold_still() {
        let g = geom(2, 1);
        let v = view(&g, (250.0, 100.0));
        let cols = v.cols();
        assert_eq!(cols.frozen, 1);
        // The scrolled band starts at x=100 (col 2's start) + 250 scroll.
        assert_eq!(cols.first, 4);
        assert_eq!(v.col_x(1), 50.0);
        assert_eq!(v.col_x(4), 50.0 + 300.0 - 250.0);
        assert_eq!(v.rows().first, 8);
        assert_eq!(v.rows().iter().take(3).collect::<Vec<_>>(), vec![1, 2, 8]);
        assert_eq!(v.col_band(1), (50.0, 150.0));
        assert_eq!(v.col_band(9), (150.0, 450.0));
    }

    #[test]
    fn hits_land_on_the_quadrant_under_the_pointer() {
        let g = geom(2, 1);
        let v = view(&g, (250.0, 100.0));
        assert_eq!(v.hit(15.0, 55.0), Hit::Corner);
        assert_eq!(v.hit(60.0, 55.0), Hit::ColHeader(1));
        assert_eq!(v.hit(160.0, 55.0), Hit::ColHeader(4));
        assert_eq!(v.hit(15.0, 75.0), Hit::RowHeader(1));
        // y=70+40 is the frozen edge: the first scrolled row, 3 + 100/20 = 8.
        assert_eq!(v.hit(60.0, 110.0), Hit::Cell { row: 8, col: 1 });
        assert_eq!(v.hit(5.0, 75.0), Hit::Outside);
    }

    #[test]
    fn scroll_to_show_moves_the_least() {
        let g = geom(1, 0);
        let v = view(&g, (0.0, 0.0));
        // In view: nothing moves.
        assert_eq!(v.scroll_to_show(5, 2), (0.0, 0.0));
        // Below: the row's bottom meets the viewport's (180 tall after the frozen row).
        let (_, y) = v.scroll_to_show(20, 1);
        assert_eq!(y, 20.0 * 20.0 - 20.0 - 180.0);
        // Right: column 6's right edge meets the body's.
        assert_eq!(v.scroll_to_show(2, 6).0, 600.0 - 400.0);
        // Above/left from far away: its start meets the edge.
        let v = view(&g, (900.0, 900.0));
        assert_eq!(v.scroll_to_show(3, 2), (100.0, 20.0));
        // A frozen row never scrolls y.
        assert_eq!(v.scroll_to_show(1, 2).1, 900.0);
    }
}
