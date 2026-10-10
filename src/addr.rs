//! A1-style addresses, as agents (and people) write them: `B2`, `A1:C5`,
//! whole columns `A:C`, whole rows `2:4`, any of them with a `Sheet!`
//! prefix (quoted when the name has spaces: `'Q3 data'!A1`). `$` marks are
//! accepted and ignored.

use crate::book::column_name;
use crate::grid::{LAST_COLUMN, LAST_ROW};

/// `A` → 1, `AA` → 27; `None` past `XFD` or for anything not letters.
pub fn column_number(letters: &str) -> Option<i32> {
    if letters.is_empty() || letters.len() > 3 {
        return None;
    }
    let mut n = 0i32;
    for c in letters.chars() {
        if !c.is_ascii_alphabetic() {
            return None;
        }
        n = n * 26 + (c.to_ascii_uppercase() as i32 - 'A' as i32 + 1);
    }
    (n <= LAST_COLUMN).then_some(n)
}

/// One end of a range: a cell, a whole column or a whole row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    Cell(i32, i32),
    Col(i32),
    Row(i32),
}

fn parse_end(s: &str) -> Option<End> {
    let s = s.replace('$', "");
    let split = s.find(|c: char| c.is_ascii_digit()).unwrap_or(s.len());
    let (letters, digits) = s.split_at(split);
    let row = || digits.parse::<i32>().ok().filter(|r| (1..=LAST_ROW).contains(r));
    match (letters.is_empty(), digits.is_empty()) {
        (false, false) => Some(End::Cell(row()?, column_number(letters)?)),
        (false, true) => Some(End::Col(column_number(letters)?)),
        (true, false) => Some(End::Row(row()?)),
        (true, true) => None,
    }
}

/// An address split into its sheet name (if it names one) and the rest.
pub fn split_sheet(addr: &str) -> (Option<String>, &str) {
    match addr.rsplit_once('!') {
        Some((sheet, rest)) => {
            let sheet = sheet.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')).unwrap_or(sheet);
            (Some(sheet.replace("''", "'")), rest)
        }
        None => (None, addr),
    }
}

/// A range as `[row1, col1, row2, col2]`, ordered. A single cell is a range
/// of one; whole columns and rows span the sheet.
pub fn parse_range(s: &str) -> Result<[i32; 4], String> {
    let bad = || format!("not an A1 address: `{s}`");
    let (a, b) = match s.trim().split_once(':') {
        Some((a, b)) => (parse_end(a.trim()).ok_or_else(bad)?, parse_end(b.trim()).ok_or_else(bad)?),
        None => {
            let e = parse_end(s.trim()).ok_or_else(bad)?;
            if !matches!(e, End::Cell(..)) {
                return Err(bad());
            }
            (e, e)
        }
    };
    let [r1, c1, r2, c2] = match (a, b) {
        (End::Cell(r1, c1), End::Cell(r2, c2)) => [r1, c1, r2, c2],
        (End::Col(c1), End::Col(c2)) => [1, c1, LAST_ROW, c2],
        (End::Row(r1), End::Row(r2)) => [r1, 1, r2, LAST_COLUMN],
        _ => return Err(bad()),
    };
    Ok([r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)])
}

/// A single cell, `[row, col]`.
pub fn parse_cell(s: &str) -> Result<(i32, i32), String> {
    match parse_end(s.trim()) {
        Some(End::Cell(r, c)) => Ok((r, c)),
        _ => Err(format!("not a cell address: `{s}`")),
    }
}

pub fn cell_name(row: i32, col: i32) -> String {
    format!("{}{row}", column_name(col))
}

/// A range back in A1 form (`A1` for one cell).
pub fn range_name([r1, c1, r2, c2]: [i32; 4]) -> String {
    if (r1, c1) == (r2, c2) {
        cell_name(r1, c1)
    } else {
        format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_both_ways() {
        assert_eq!(column_number("A"), Some(1));
        assert_eq!(column_number("az"), Some(52));
        assert_eq!(column_number("XFD"), Some(16384));
        assert_eq!(column_number("XFE"), None);
        assert_eq!(column_number("A1"), None);
        for n in [1, 26, 27, 702, 703, 16384] {
            assert_eq!(column_number(&column_name(n)), Some(n));
        }
    }

    #[test]
    fn ranges_parse_in_every_form() {
        assert_eq!(parse_range("B2"), Ok([2, 2, 2, 2]));
        assert_eq!(parse_range("C5:A1"), Ok([1, 1, 5, 3]));
        assert_eq!(parse_range("$A$1:$B$2"), Ok([1, 1, 2, 2]));
        assert_eq!(parse_range("B:D"), Ok([1, 2, LAST_ROW, 4]));
        assert_eq!(parse_range("3:4"), Ok([3, 1, 4, LAST_COLUMN]));
        assert!(parse_range("B").is_err());
        assert!(parse_range("A1:B").is_err());
        assert!(parse_range("A0").is_err());
        assert!(parse_range("hello").is_err());
        assert_eq!(parse_cell("aa10"), Ok((10, 27)));
        assert!(parse_cell("A1:B2").is_err());
    }

    #[test]
    fn sheet_prefixes_split_off() {
        assert_eq!(split_sheet("Data!A1:B2"), (Some("Data".into()), "A1:B2"));
        assert_eq!(split_sheet("'Q3 ''final'''!C3"), (Some("Q3 'final'".into()), "C3"));
        assert_eq!(split_sheet("A1"), (None, "A1"));
        assert_eq!(range_name([1, 1, 1, 1]), "A1");
        assert_eq!(range_name([2, 1, 5, 3]), "A2:C5");
    }
}
