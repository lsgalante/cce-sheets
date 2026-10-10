//! The formatting toolbar under the formula bar, and the strip painter it
//! shares with the sheet tabs.
//!
//! Each group of tools is a ButtonStrip-style strip: one well carved around
//! the run, its segments butting together on the floor, a pressed (on)
//! segment a plateau raised back out of it. Symbols are cce-icons glyphs.

use cce_ui::scene::layout::Rect;
use cce_ui::scene::paint::{ControlPlate, PaintCtx, PlateStance};

use crate::format::{format_name, Active, Align};
use crate::grid::{dim, Face};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// The number format, a menu.
    Format,
    Bold,
    Italic,
    Underline,
    AlignLeft,
    AlignCenter,
    AlignRight,
    /// Colour menus; the button wears the colour in force on a bar.
    TextColor,
    FillColor,
    /// The borders menu.
    Borders,
}

impl Tool {
    fn glyph(self) -> &'static str {
        match self {
            Tool::Format => "chevron-down",
            Tool::Bold => "bold",
            Tool::Italic => "italic",
            Tool::Underline => "underline",
            Tool::AlignLeft => "align-left",
            Tool::AlignCenter => "align-center",
            Tool::AlignRight => "align-right",
            Tool::TextColor => "font",
            Tool::FillColor => "fill",
            Tool::Borders => "borders",
        }
    }

    /// Whether the tool shows pressed for the selected cell.
    fn on(self, a: &Active) -> bool {
        match self {
            Tool::Bold => a.bold,
            Tool::Italic => a.italic,
            Tool::Underline => a.underline,
            Tool::AlignLeft => a.align == Align::Left,
            Tool::AlignCenter => a.align == Align::Center,
            Tool::AlignRight => a.align == Align::Right,
            _ => false,
        }
    }
}

/// The groups, left to right.
const GROUPS: &[&[Tool]] = &[
    &[Tool::Format],
    &[Tool::Bold, Tool::Italic, Tool::Underline],
    &[Tool::AlignLeft, Tool::AlignCenter, Tool::AlignRight],
    &[Tool::TextColor, Tool::FillColor],
    &[Tool::Borders],
];

/// Lay the groups out from `x` along a row at `y`, `h` tall: glyph tools are
/// square, the format menu wide enough for its longest name.
pub fn layout(x: f32, y: f32, h: f32, char_w: f32, gap: f32) -> Vec<Vec<(Rect, Tool)>> {
    let text_in = cce_ui::layout::CONTROL_TEXT_INSET;
    let mut x = x;
    let mut out = Vec::new();
    for group in GROUPS {
        let mut g = Vec::new();
        for &tool in *group {
            let w = match tool {
                Tool::Format => char_w * "Scientific".len() as f32 + 2.0 * text_in + h * 0.5,
                _ => h,
            };
            g.push((Rect { x, y, width: w, height: h }, tool));
            x += w;
        }
        x += gap;
        out.push(g);
    }
    out
}

pub fn hit(groups: &[Vec<(Rect, Tool)>], x: f32, y: f32) -> Option<Tool> {
    groups.iter().flatten().find(|(r, _)| x >= r.x && y >= r.y && x < r.x + r.width && y < r.y + r.height).map(|&(_, t)| t)
}

/// `hide`: a box drawn over the toolbar (the completion list); text cannot
/// be covered by a box, so the format name under one is not drawn.
pub fn paint(pc: &mut PaintCtx, groups: &[Vec<(Rect, Tool)>], a: &Active, face: &Face, hide: Option<Rect>) {
    let text_in = cce_ui::layout::CONTROL_TEXT_INSET;
    let ink = srgb(face.text);
    for g in groups {
        let segs: Vec<(Rect, bool)> = g.iter().map(|&(r, t)| (r, t.on(a))).collect();
        strip(pc, &segs);
        for &(r, tool) in g {
            match tool {
                Tool::Format => {
                    if hide.is_some_and(|h| h.x < r.x + r.width && h.x + h.width > r.x && h.y < r.y + r.height && h.y + h.height > r.y) {
                        continue;
                    }
                    let ty = cce_ui::layout::center_text_y(r.y, r.height, face.size);
                    pc.text_with(format_name(&a.num_fmt), r.x + text_in, ty, face.size, face.text, Some(face.family.clone()), None);
                    let s = r.height * 0.4;
                    pc.icon(tool.glyph(), Rect { x: r.x + r.width - text_in - s, y: r.y + (r.height - s) / 2.0, width: s, height: s }, [ink[0], ink[1], ink[2], 0.75]);
                }
                Tool::TextColor | Tool::FillColor => {
                    // The glyph raised a little, the colour on a bar beneath.
                    let s = r.height * 0.5;
                    let gx = r.x + (r.width - s) / 2.0;
                    pc.icon(tool.glyph(), Rect { x: gx, y: r.y + r.height * 0.14, width: s, height: s }, ink);
                    let bar = Rect { x: gx - 1.0, y: r.y + r.height * 0.7, width: s + 2.0, height: (r.height * 0.11).max(2.0) };
                    let hex = if tool == Tool::TextColor { &a.font_color } else { &a.fill_color };
                    match cce_ui::color::parse_hex_rgba_linear(hex) {
                        Some(c) => pc.quad(bar, [c[0], c[1], c[2], 1.0]),
                        // Automatic text is the label colour; no fill is an outline.
                        None if tool == Tool::TextColor => pc.quad(bar, [ink[0], ink[1], ink[2], 0.9]),
                        None => crate::grid::frame_with(pc, bar, [ink[0], ink[1], ink[2], 0.5], 1.0),
                    }
                }
                _ => {
                    let s = r.height * 0.55;
                    pc.icon(tool.glyph(), Rect { x: r.x + (r.width - s) / 2.0, y: r.y + (r.height - s) / 2.0, width: s, height: s }, if tool.on(a) { ink } else { srgb(dim(face.text)) });
                }
            }
        }
    }
}

/// Segments butting together in one recessed strip; an on segment is a
/// plateau raised out of the floor (flat relief: a fill).
pub fn strip(pc: &mut PaintCtx, segs: &[(Rect, bool)]) {
    let (Some(&(first, _)), Some(&(last, _))) = (segs.first(), segs.last()) else {
        return;
    };
    let whole = Rect { x: first.x, y: first.y, width: last.x + last.width - first.x, height: first.height };
    let radius = cce_ui::layout::button_corner_radius();
    let relief = cce_ui::layout::control_relief();
    let depth = if relief {
        let depth = cce_ui::layout::bevel_width().min(whole.height * 0.2);
        let (well, radii) = cce_ui::layout::carve_inside(whole, (radius, radius, radius, radius), depth);
        pc.recess(well, radii, depth);
        depth
    } else {
        0.0
    };
    for &(rect, on) in segs {
        if !on {
            continue;
        }
        let inset = depth * 0.5;
        let seg = Rect { x: rect.x + inset, y: rect.y + inset, width: (rect.width - 2.0 * inset).max(0.0), height: (rect.height - 2.0 * inset).max(0.0) };
        let seg_r = (radius - inset).max(0.0);
        pc.rounded_rect(seg, seg_r, (true, true, true, true), cce_ui::color::PANEL_MENU_FOCUSED);
        if relief {
            pc.control_plate(&ControlPlate::control(seg, seg_r, PlateStance::Raised, None).with_depth(depth));
        }
    }
}

/// A label colour as an icon tint: sRGB in 0..1, opaque.
fn srgb(c: [u8; 3]) -> [f32; 4] {
    [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, 1.0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_lay_out_left_to_right_and_hit() {
        let g = layout(10.0, 50.0, 28.0, 8.0, 6.0);
        assert_eq!(g.len(), GROUPS.len());
        assert_eq!(g[1].iter().map(|s| s.1).collect::<Vec<_>>(), vec![Tool::Bold, Tool::Italic, Tool::Underline]);
        // Segments of a group butt together; groups stand a gap apart.
        assert_eq!(g[1][1].0.x, g[1][0].0.x + 28.0);
        assert_eq!(g[2][0].0.x, g[1][2].0.x + 28.0 + 6.0);
        let b = g[1][0].0;
        assert_eq!(hit(&g, b.x + 1.0, b.y + 1.0), Some(Tool::Bold));
        assert_eq!(hit(&g, 5.0, 60.0), None);
        assert_eq!(hit(&g, b.x + 1.0, 49.0), None);
    }

    #[test]
    fn toggles_show_what_the_cell_has() {
        let a = Active { bold: true, align: Align::Right, ..Default::default() };
        assert!(Tool::Bold.on(&a) && Tool::AlignRight.on(&a));
        assert!(!Tool::Italic.on(&a) && !Tool::AlignLeft.on(&a) && !Tool::Format.on(&a));
    }
}
