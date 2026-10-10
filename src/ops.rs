//! Operations on the selection that change the sheet's shape or fill it:
//! inserting and deleting rows and columns, the fill handle, and fitting a
//! column to its contents. Each is one model call (one undo step).

use ironcalc::base::expressions::types::Area;
use ironcalc::base::UserModel;

use crate::grid::{LAST_COLUMN, LAST_ROW};

/// `[row1, col1, row2, col2]` with each pair in order.
pub fn normalized([a, b, c, d]: [i32; 4]) -> [i32; 4] {
    [a.min(c), b.min(d), a.max(c), b.max(d)]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    Rows,
    Cols,
}

/// Whether a range is whole columns (every row of them) — the one selection
/// whose insert and delete act on columns from the keyboard.
pub fn whole_cols(range: [i32; 4]) -> bool {
    let [r1, _, r2, _] = normalized(range);
    r1 == 1 && r2 == LAST_ROW
}

pub fn whole_rows(range: [i32; 4]) -> bool {
    let [_, c1, _, c2] = normalized(range);
    c1 == 1 && c2 == LAST_COLUMN
}

/// How many rows (or columns) of the range an insert or delete takes: the
/// range's own count, except a whole-sheet span counts what is used, so a
/// column selection does not insert a million rows.
fn count(first: i32, last: i32, whole: i32, used: i32) -> i32 {
    if last >= whole {
        (used - first + 1).max(1)
    } else {
        last - first + 1
    }
}

/// Insert as many rows (or columns) as the range spans, before it or after.
pub fn insert(model: &mut UserModel, sheet: u32, range: [i32; 4], line: Line, after: bool, used: (i32, i32)) -> Result<(), String> {
    let [r1, c1, r2, c2] = normalized(range);
    match line {
        Line::Rows => {
            let n = if whole_cols(range) { 1 } else { count(r1, r2, LAST_ROW, used.0) };
            model.insert_rows(sheet, if after { r1 + n } else { r1 }, n)
        }
        Line::Cols => {
            let n = if whole_rows(range) { 1 } else { count(c1, c2, LAST_COLUMN, used.1) };
            model.insert_columns(sheet, if after { c1 + n } else { c1 }, n)
        }
    }
}

/// Delete the rows (or columns) the range spans.
pub fn delete(model: &mut UserModel, sheet: u32, range: [i32; 4], line: Line, used: (i32, i32)) -> Result<(), String> {
    let [r1, c1, r2, c2] = normalized(range);
    match line {
        Line::Rows => model.delete_rows(sheet, r1, count(r1, r2, LAST_ROW, used.0)),
        Line::Cols => model.delete_columns(sheet, c1, count(c1, c2, LAST_COLUMN, used.1)),
    }
}

/// Where a fill-handle drag reaches: down/up to a row, or across to a column
/// — whichever way the pointer has gone further past the range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillTo {
    Row(i32),
    Col(i32),
}

pub fn fill_target(range: [i32; 4], row: i32, col: i32) -> Option<FillTo> {
    let [r1, c1, r2, c2] = normalized(range);
    let past = |p: i32, lo: i32, hi: i32| if p > hi { p - hi } else if p < lo { p - lo } else { 0 };
    let (dr, dc) = (past(row, r1, r2), past(col, c1, c2));
    match (dr, dc) {
        (0, 0) => None,
        _ if dr.abs() >= dc.abs() => Some(FillTo::Row(row)),
        _ => Some(FillTo::Col(col)),
    }
}

/// The range a fill covers once done: the source and what it filled.
pub fn filled_range(range: [i32; 4], to: FillTo) -> [i32; 4] {
    let [r1, c1, r2, c2] = normalized(range);
    match to {
        FillTo::Row(r) => [r1.min(r), c1, r2.max(r), c2],
        FillTo::Col(c) => [r1, c1.min(c), r2, c2.max(c)],
    }
}

/// Fill from the range, continuing a series where the engine sees one
/// (1, 2 → 3, 4; Mon → Tue) and copying otherwise.
pub fn fill(model: &mut UserModel, sheet: u32, range: [i32; 4], to: FillTo) -> Result<(), String> {
    let [r1, c1, r2, c2] = normalized(range);
    let area = Area { sheet, row: r1, column: c1, width: c2 - c1 + 1, height: r2 - r1 + 1 };
    match to {
        FillTo::Row(r) => model.auto_fill_rows(&area, r),
        FillTo::Col(c) => model.auto_fill_columns(&area, c),
    }
}

/// A column width that shows the widest of `widths` (text widths in px),
/// with the cell padding; `None` for a column with nothing in it.
pub fn fit_width(widths: impl IntoIterator<Item = f32>, pad: f32) -> Option<f64> {
    let widest = widths.into_iter().fold(None, |m: Option<f32>, w| Some(m.map_or(w, |m| m.max(w))))?;
    Some(((widest + 2.0 * pad + 4.0).clamp(24.0, 800.0)) as f64)
}

/// Every non-empty value of column `col`, as shown.
pub fn column_texts(model: &UserModel, sheet: u32, col: i32) -> Vec<String> {
    let m = model.get_model();
    let Some(ws) = m.workbook.worksheets.get(sheet as usize) else {
        return Vec::new();
    };
    ws.sheet_data
        .iter()
        .filter(|(_, cells)| cells.contains_key(&col))
        .filter_map(|(&row, _)| model.get_formatted_cell_value(sheet, row, col).ok())
        .filter(|t| !t.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> UserModel<'static> {
        let mut m = UserModel::new_empty("t", "en", "UTC", "en").unwrap();
        for r in 1..=4 {
            m.set_user_input(0, r, 1, &r.to_string()).unwrap();
        }
        m.set_user_input(0, 1, 2, "=SUM(A1:A4)").unwrap();
        m
    }

    #[test]
    fn inserting_rows_above_shifts_cells_and_references() {
        let mut m = model();
        insert(&mut m, 0, [2, 1, 3, 1], Line::Rows, false, (4, 2)).unwrap();
        assert_eq!(m.get_formatted_cell_value(0, 4, 1).unwrap(), "2");
        assert_eq!(m.get_cell_content(0, 1, 2).unwrap(), "=SUM(A1:A6)");
        m.undo().unwrap();
        assert_eq!(m.get_formatted_cell_value(0, 2, 1).unwrap(), "2");
    }

    #[test]
    fn inserting_after_lands_below_the_range() {
        let mut m = model();
        insert(&mut m, 0, [2, 1, 2, 1], Line::Rows, true, (4, 2)).unwrap();
        assert_eq!(m.get_formatted_cell_value(0, 3, 1).unwrap(), "");
        assert_eq!(m.get_formatted_cell_value(0, 4, 1).unwrap(), "3");
    }

    #[test]
    fn a_whole_column_selection_deletes_columns_and_inserts_one() {
        let mut m = model();
        let col_a = [1, 1, LAST_ROW, 1];
        assert!(whole_cols(col_a));
        insert(&mut m, 0, col_a, Line::Cols, false, (4, 2)).unwrap();
        assert_eq!(m.get_formatted_cell_value(0, 1, 2).unwrap(), "1");
        delete(&mut m, 0, [1, 1, LAST_ROW, 1], Line::Cols, (4, 3)).unwrap();
        assert_eq!(m.get_formatted_cell_value(0, 1, 1).unwrap(), "1");
        // Deleting a whole column's rows takes only the used ones.
        delete(&mut m, 0, col_a, Line::Rows, (4, 2)).unwrap();
        assert_eq!(m.get_formatted_cell_value(0, 1, 1).unwrap(), "");
    }

    #[test]
    fn the_fill_follows_the_longer_drag() {
        let r = [2, 2, 3, 3];
        assert_eq!(fill_target(r, 3, 3), None);
        assert_eq!(fill_target(r, 7, 4), Some(FillTo::Row(7)));
        assert_eq!(fill_target(r, 4, 9), Some(FillTo::Col(9)));
        assert_eq!(fill_target(r, 1, 3), Some(FillTo::Row(1)));
        assert_eq!(filled_range(r, FillTo::Row(7)), [2, 2, 7, 3]);
        assert_eq!(filled_range(r, FillTo::Col(1)), [2, 1, 3, 3]);
    }

    #[test]
    fn a_fill_continues_a_series() {
        let mut m = model();
        fill(&mut m, 0, [3, 1, 4, 1], FillTo::Row(7)).unwrap();
        assert_eq!(m.get_formatted_cell_value(0, 7, 1).unwrap(), "7");
        m.set_user_input(0, 1, 4, "=A1*2").unwrap();
        fill(&mut m, 0, [1, 4, 1, 4], FillTo::Row(3)).unwrap();
        assert_eq!(m.get_cell_content(0, 3, 4).unwrap(), "=A3*2");
    }

    #[test]
    fn a_column_fits_its_widest_value() {
        assert_eq!(fit_width([10.0, 50.0, 30.0], 3.0), Some(60.0));
        assert_eq!(fit_width([], 3.0), None);
        assert_eq!(fit_width([2000.0], 3.0), Some(800.0));
        let m = model();
        let mut t = column_texts(&m, 0, 1);
        t.sort();
        assert_eq!(t, vec!["1", "2", "3", "4"]);
    }
}
