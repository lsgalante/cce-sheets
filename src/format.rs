//! Cell formatting: what the toolbar shows for the selected cell, and the
//! style changes it makes to the selection — font, alignment, number
//! format, colours, borders. Each is one model call, so one undo step.
//!
//! Styles go through `UserModel::update_range_style` by path (`font.b`,
//! `alignment.horizontal`, …), which special-cases whole rows and whole
//! columns, so a header selection is not a million cell writes.

use ironcalc::base::expressions::types::Area;
use ironcalc::base::types::HorizontalAlignment;
use ironcalc::base::{BorderArea, UserModel};

use crate::ops::normalized;

/// The selected cell's formatting, as the toolbar shows it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Active {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub align: Align,
    pub num_fmt: String,
    /// `#rrggbb`, or empty when unset.
    pub font_color: String,
    pub fill_color: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Align {
    /// By the value's type (numbers right, text left).
    #[default]
    General,
    Left,
    Center,
    Right,
}

impl Align {
    fn param(self) -> &'static str {
        match self {
            Align::General => "general",
            Align::Left => "left",
            Align::Center => "center",
            Align::Right => "right",
        }
    }
}

pub fn active(model: &UserModel, sheet: u32, row: i32, col: i32) -> Active {
    let Ok(s) = model.get_cell_style(sheet, row, col) else {
        return Active::default();
    };
    let align = match s.alignment.as_ref().map(|a| &a.horizontal) {
        Some(HorizontalAlignment::Left) => Align::Left,
        Some(HorizontalAlignment::Center | HorizontalAlignment::CenterContinuous) => Align::Center,
        Some(HorizontalAlignment::Right) => Align::Right,
        _ => Align::General,
    };
    Active {
        bold: s.font.b,
        italic: s.font.i,
        underline: s.font.u,
        align,
        num_fmt: s.num_fmt.clone(),
        font_color: model.resolve_color(&s.font.color),
        fill_color: model.resolve_color(&s.fill.color),
    }
}

/// The selection as the area a style is written to.
fn area(sheet: u32, range: [i32; 4]) -> Area {
    let [r1, c1, r2, c2] = normalized(range);
    Area { sheet, row: r1, column: c1, width: c2 - c1 + 1, height: r2 - r1 + 1 }
}

/// Set one style path on the selection.
pub fn set(model: &mut UserModel, sheet: u32, range: [i32; 4], path: &str, value: &str) -> Result<(), String> {
    model.update_range_style(&area(sheet, range), path, value)
}

/// Bold, italic or underline: the selection takes the opposite of what the
/// selected cell has, as Excel's toggles do.
pub fn toggle(model: &mut UserModel, sheet: u32, range: [i32; 4], path: &str, now: bool) -> Result<(), String> {
    set(model, sheet, range, path, if now { "false" } else { "true" })
}

/// An alignment button: pressing the one in force puts it back to general.
pub fn align(model: &mut UserModel, sheet: u32, range: [i32; 4], want: Align, now: Align) -> Result<(), String> {
    let to = if want == now { Align::General } else { want };
    set(model, sheet, range, "alignment.horizontal", to.param())
}

/// The number formats the toolbar offers: (menu id, label, format code).
pub const NUMBER_FORMATS: &[(&str, &str, &str)] = &[
    ("fmt-general", "General", "general"),
    ("fmt-number", "Number  1,234.56", "#,##0.00"),
    ("fmt-integer", "Integer  1,235", "#,##0"),
    ("fmt-currency", "Currency  $1,234.56", "$#,##0.00"),
    ("fmt-percent", "Percent  12.34%", "0.00%"),
    ("fmt-scientific", "Scientific  1.23E+03", "0.00E+00"),
    ("fmt-date", "Date  2026-10-10", "yyyy-mm-dd"),
    ("fmt-time", "Time  13:45:00", "hh:mm:ss"),
    ("fmt-text", "Text", "@"),
];

/// The toolbar's name for a format code: its menu label's first word, or
/// "Custom" for one it does not offer.
pub fn format_name(code: &str) -> &'static str {
    NUMBER_FORMATS
        .iter()
        .find(|(_, _, c)| c.eq_ignore_ascii_case(code))
        .map_or("Custom", |(_, label, _)| label.split_whitespace().next().unwrap_or(label))
}

/// The palette the colour menus offer: (name, `#rrggbb`); an empty colour is
/// "automatic" text or no fill.
pub const PALETTE: &[(&str, &str)] = &[
    ("Red", "#E5534B"),
    ("Orange", "#E8913A"),
    ("Yellow", "#F2CC60"),
    ("Green", "#57AB5A"),
    ("Teal", "#39A0A0"),
    ("Blue", "#4472C4"),
    ("Purple", "#9A6DD7"),
    ("Grey", "#8B8F99"),
    ("White", "#FFFFFF"),
    ("Black", "#000000"),
];

/// The borders menu: (menu id, label, IronCalc border type).
pub const BORDERS: &[(&str, &str, &str)] = &[
    ("border-all", "All borders", "All"),
    ("border-outer", "Outline", "Outer"),
    ("border-inner", "Inside", "Inner"),
    ("border-bottom", "Bottom", "Bottom"),
    ("border-none", "No borders", "None"),
];

/// Draw (or clear) thin borders on the selection. IronCalc's `BorderArea`
/// keeps its fields private but deserializes, as its web frontend sends it.
pub fn borders(model: &mut UserModel, sheet: u32, range: [i32; 4], kind: &str) -> Result<(), String> {
    let spec = serde_json::json!({ "item": { "style": "thin", "color": "#000000" }, "type": kind });
    let border: BorderArea = serde_json::from_value(spec).map_err(|e| e.to_string())?;
    model.set_area_with_border(&area(sheet, range), &border)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> UserModel<'static> {
        let mut m = UserModel::new_empty("t", "en", "UTC", "en").unwrap();
        m.set_user_input(0, 1, 1, "1234.5").unwrap();
        m
    }

    #[test]
    fn toggles_flip_what_the_selected_cell_has() {
        let mut m = model();
        let r = [1, 1, 2, 2];
        let now = active(&m, 0, 1, 1).bold;
        toggle(&mut m, 0, r, "font.b", now).unwrap();
        assert!(active(&m, 0, 2, 2).bold);
        let now = active(&m, 0, 1, 1).bold;
        toggle(&mut m, 0, r, "font.b", now).unwrap();
        assert!(!active(&m, 0, 1, 1).bold);
        m.undo().unwrap();
        assert!(active(&m, 0, 1, 1).bold);
    }

    #[test]
    fn alignment_presses_set_and_unset() {
        let mut m = model();
        let r = [1, 1, 1, 1];
        let now = active(&m, 0, 1, 1).align;
        align(&mut m, 0, r, Align::Center, now).unwrap();
        assert_eq!(active(&m, 0, 1, 1).align, Align::Center);
        let now = active(&m, 0, 1, 1).align;
        align(&mut m, 0, r, Align::Center, now).unwrap();
        assert_eq!(active(&m, 0, 1, 1).align, Align::General);
    }

    #[test]
    fn number_formats_change_what_shows() {
        let mut m = model();
        for (_, _, code) in NUMBER_FORMATS {
            set(&mut m, 0, [1, 1, 1, 1], "num_fmt", code).unwrap();
        }
        set(&mut m, 0, [1, 1, 1, 1], "num_fmt", "$#,##0.00").unwrap();
        assert_eq!(m.get_formatted_cell_value(0, 1, 1).unwrap(), "$1,234.50");
        assert_eq!(format_name(&active(&m, 0, 1, 1).num_fmt), "Currency");
        assert_eq!(format_name("general"), "General");
        assert_eq!(format_name("0.000"), "Custom");
    }

    #[test]
    fn colours_set_and_clear() {
        let mut m = model();
        set(&mut m, 0, [1, 1, 1, 1], "fill.color", "#F2CC60").unwrap();
        set(&mut m, 0, [1, 1, 1, 1], "font.color", "#E5534B").unwrap();
        let a = active(&m, 0, 1, 1);
        assert_eq!((a.fill_color.to_uppercase().as_str(), a.font_color.to_uppercase().as_str()), ("#F2CC60", "#E5534B"));
        set(&mut m, 0, [1, 1, 1, 1], "fill.color", "").unwrap();
        assert_eq!(active(&m, 0, 1, 1).fill_color, "");
    }

    #[test]
    fn borders_go_on_and_come_off() {
        let mut m = model();
        borders(&mut m, 0, [1, 1, 2, 2], "Outer").unwrap();
        let s = m.get_cell_style(0, 1, 1).unwrap();
        assert!(s.border.top.is_some() && s.border.left.is_some());
        assert!(m.get_cell_style(0, 2, 2).unwrap().border.bottom.is_some());
        borders(&mut m, 0, [1, 1, 2, 2], "None").unwrap();
        assert!(m.get_cell_style(0, 1, 1).unwrap().border.top.is_none());
    }
}
