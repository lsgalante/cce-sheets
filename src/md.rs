//! Markdown (GFM) tables in a note: finding them, reading their cells, and
//! writing a table back in place of the lines it came from — so the rest of
//! the note stays byte for byte as it was.
//!
//! A table is a header line with pipes, a delimiter line of `---` cells
//! (`:--`, `:-:`, `--:` set the column's alignment), then body lines until a
//! blank line or one without a pipe. Fenced code (``` / ~~~) is skipped. In
//! a cell, `\|` is a literal pipe and `<br>` a line break.

/// A column's alignment marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MdAlign {
    #[default]
    None,
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdTable {
    /// The table's lines: `first..end` (0-based, `end` exclusive).
    pub first: usize,
    pub end: usize,
    /// The nearest heading above it, without its `#`s.
    pub heading: Option<String>,
    /// Header first, then the body rows; every row as wide as the widest.
    pub rows: Vec<Vec<String>>,
    pub align: Vec<MdAlign>,
    /// The table's text exactly as it stood, line endings included.
    pub raw: String,
}

/// Lines with their endings kept, so a write-back can splice exactly.
fn lines_keep(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

fn strip_eol(line: &str) -> &str {
    line.trim_end_matches(['\n', '\r'])
}

/// A row's cells: split on pipes not escaped by `\`, outer pipes optional,
/// each trimmed, `\|` unescaped and `<br>` turned into a line break.
pub fn split_row(line: &str) -> Vec<String> {
    let s = strip_eol(line).trim();
    let s = s.strip_prefix('|').unwrap_or(s);
    let s = if s.ends_with('|') && !s.ends_with("\\|") { &s[..s.len() - 1] } else { s };
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cur.push('|');
                chars.next();
            }
            '|' => cells.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    cells.push(cur);
    cells.into_iter().map(|c| c.trim().replace("<br>", "\n").replace("<br/>", "\n").replace("<br />", "\n")).collect()
}

/// The alignments of a delimiter line, or `None` if it is not one.
fn delimiter(line: &str) -> Option<Vec<MdAlign>> {
    if !line.contains('-') {
        return None;
    }
    split_row(line)
        .iter()
        .map(|c| {
            let c = c.trim();
            let (l, r) = (c.starts_with(':'), c.ends_with(':'));
            let dashes = c.trim_matches(':');
            (!dashes.is_empty() && dashes.chars().all(|ch| ch == '-')).then_some(match (l, r) {
                (true, true) => MdAlign::Center,
                (true, false) => MdAlign::Left,
                (false, true) => MdAlign::Right,
                (false, false) => MdAlign::None,
            })
        })
        .collect()
}

fn is_fence(line: &str) -> Option<&'static str> {
    let t = line.trim_start();
    if t.starts_with("```") {
        Some("```")
    } else if t.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

/// Every table in `text`, in order.
pub fn tables(text: &str) -> Vec<MdTable> {
    let lines = lines_keep(text);
    let mut out = Vec::new();
    let mut fence: Option<&str> = None;
    let mut heading: Option<String> = None;
    let mut i = 0;
    while i < lines.len() {
        let line = strip_eol(lines[i]);
        if let Some(f) = fence {
            if line.trim_start().starts_with(f) {
                fence = None;
            }
            i += 1;
            continue;
        }
        if let Some(f) = is_fence(line) {
            fence = Some(f);
            i += 1;
            continue;
        }
        if let Some(h) = line.trim_start().strip_prefix('#') {
            let h = h.trim_start_matches('#');
            if h.starts_with(' ') || h.is_empty() {
                heading = Some(h.trim().to_string()).filter(|h| !h.is_empty());
            }
        }
        let header_like = line.contains('|');
        if let (true, Some(align)) = (header_like, lines.get(i + 1).and_then(|l| delimiter(strip_eol(l)))) {
            let header = split_row(line);
            if header.len() == align.len() {
                let mut end = i + 2;
                while end < lines.len() {
                    let l = strip_eol(lines[end]);
                    if l.trim().is_empty() || !l.contains('|') || is_fence(l).is_some() {
                        break;
                    }
                    end += 1;
                }
                let mut rows = vec![header];
                rows.extend(lines[i + 2..end].iter().map(|l| split_row(l)));
                let width = rows.iter().map(Vec::len).max().unwrap_or(0).max(align.len());
                for r in &mut rows {
                    r.resize(width, String::new());
                }
                let mut align = align;
                align.resize(width, MdAlign::None);
                out.push(MdTable { first: i, end, heading: heading.clone(), rows, align, raw: lines[i..end].concat() });
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// A cell as Markdown: pipes escaped, line breaks as `<br>`.
fn cell_md(s: &str) -> String {
    s.replace('|', "\\|").replace("\r\n", "<br>").replace('\n', "<br>")
}

/// A table as Markdown lines, columns padded to line up (by characters),
/// each line ended with `eol`.
pub fn render(rows: &[Vec<String>], align: &[MdAlign], eol: &str) -> String {
    let width = rows.iter().map(Vec::len).max().unwrap_or(0).max(align.len()).max(1);
    let cells: Vec<Vec<String>> = rows.iter().map(|r| (0..width).map(|c| cell_md(r.get(c).map_or("", String::as_str))).collect()).collect();
    let col_w: Vec<usize> = (0..width).map(|c| cells.iter().map(|r| r[c].chars().count()).max().unwrap_or(0).max(3)).collect();
    let pad = |s: &str, w: usize, a: MdAlign| {
        let n = w.saturating_sub(s.chars().count());
        match a {
            MdAlign::Right => format!("{}{s}", " ".repeat(n)),
            MdAlign::Center => format!("{}{s}{}", " ".repeat(n / 2), " ".repeat(n - n / 2)),
            _ => format!("{s}{}", " ".repeat(n)),
        }
    };
    let a = |c: usize| align.get(c).copied().unwrap_or_default();
    let line = |r: &Vec<String>| format!("| {} |{eol}", (0..width).map(|c| pad(&r[c], col_w[c], a(c))).collect::<Vec<_>>().join(" | "));
    let mut out = String::new();
    if let Some(header) = cells.first() {
        out.push_str(&line(header));
        let delim: Vec<String> = (0..width)
            .map(|c| {
                let w = col_w[c];
                match a(c) {
                    MdAlign::None => "-".repeat(w),
                    MdAlign::Left => format!(":{}", "-".repeat(w - 1)),
                    MdAlign::Right => format!("{}:", "-".repeat(w - 1)),
                    MdAlign::Center => format!(":{}:", "-".repeat(w - 2)),
                }
            })
            .collect();
        out.push_str(&format!("| {} |{eol}", delim.join(" | ")));
        for r in &cells[1..] {
            out.push_str(&line(r));
        }
    }
    out
}

/// The line ending a text uses (`\r\n` if its first line has one).
pub fn eol_of(text: &str) -> &'static str {
    match text.find('\n') {
        Some(i) if i > 0 && text.as_bytes()[i - 1] == b'\r' => "\r\n",
        _ => "\n",
    }
}

/// `text` with each table replaced: `(the table as loaded, its new text)`,
/// matched against the tables `text` holds NOW. Refused — the note is left
/// alone — when a table no longer stands as it was loaded (edited or moved
/// away in another app since), since splicing would overwrite that edit.
pub fn splice(text: &str, replacements: &[(MdTable, String)]) -> Result<String, String> {
    let now = tables(text);
    let lines = lines_keep(text);
    let mut spans = Vec::new();
    for (old, new) in replacements {
        let cur = now
            .iter()
            .find(|t| t.raw == old.raw && t.first == old.first)
            .or_else(|| now.iter().find(|t| t.raw == old.raw))
            .ok_or_else(|| format!("a table in the note changed since it was opened{}; reopen it", old.heading.as_ref().map(|h| format!(" (under \"{h}\")")).unwrap_or_default()))?;
        spans.push((cur.first, cur.end, new));
    }
    spans.sort_by_key(|s| s.0);
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (first, end, new) in spans {
        out.push_str(&lines[at..first].concat());
        out.push_str(new);
        at = end;
    }
    out.push_str(&lines[at..].concat());
    // A table that ended the file without a newline keeps that.
    if !text.ends_with('\n') && out.ends_with('\n') && !lines.is_empty() && at == lines.len() {
        let eol = eol_of(text);
        out.truncate(out.len() - eol.len());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: &str = "---\ntags: [budget]\n---\n# Budget\n\nSome text | with a pipe.\n\n| Item | Cost |\n|:-----|-----:|\n| Rent | 1200 |\n| Food | 410.5 |\n\n## Code\n\n```\n| a | b |\n|---|---|\n| 1 | 2 |\n```\n\n## Trips\nA | B | C\n--|:-:|--\nx | y\n";

    #[test]
    fn finds_tables_outside_code_with_their_headings() {
        let t = tables(NOTE);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].heading.as_deref(), Some("Budget"));
        assert_eq!(t[0].rows, vec![vec!["Item", "Cost"], vec!["Rent", "1200"], vec!["Food", "410.5"]]);
        assert_eq!(t[0].align, vec![MdAlign::Left, MdAlign::Right]);
        assert_eq!((t[0].first, t[0].end), (7, 11));
        // Without outer pipes; a short row is padded.
        assert_eq!(t[1].heading.as_deref(), Some("Trips"));
        assert_eq!(t[1].rows[1], vec!["x", "y", ""]);
        assert_eq!(t[1].align[1], MdAlign::Center);
    }

    #[test]
    fn cells_unescape_and_break() {
        assert_eq!(split_row("| a \\| b | c<br>d |"), vec!["a | b", "c\nd"]);
        assert_eq!(split_row("x|y"), vec!["x", "y"]);
        assert_eq!(cell_md("a | b\nc"), "a \\| b<br>c");
    }

    #[test]
    fn rendering_pads_and_marks_alignment() {
        let rows = vec![vec!["Item".into(), "Cost".into()], vec!["Rent".into(), "1200".into()]];
        let out = render(&rows, &[MdAlign::Left, MdAlign::Right], "\n");
        assert_eq!(out, "| Item | Cost |\n| :--- | ---: |\n| Rent | 1200 |\n");
        let c = render(&[vec!["a".into()], vec!["bbbbb".into()]], &[MdAlign::Center], "\n");
        assert_eq!(c, "|   a   |\n| :---: |\n| bbbbb |\n");
    }

    #[test]
    fn rendered_tables_parse_back_the_same() {
        let rows = vec![vec!["h|1".into(), "h2".into()], vec!["two\nlines".into(), "".into()]];
        let md = render(&rows, &[MdAlign::None, MdAlign::Center], "\n");
        let t = tables(&md);
        assert_eq!(t[0].rows, rows);
        assert_eq!(t[0].align, vec![MdAlign::None, MdAlign::Center]);
    }

    #[test]
    fn splicing_touches_only_the_table() {
        let t = tables(NOTE);
        let new = render(&[vec!["Item".into(), "Cost".into()], vec!["Rent".into(), "1250".into()]], &t[0].align, "\n");
        let out = splice(NOTE, &[(t[0].clone(), new.clone())]).unwrap();
        assert!(out.starts_with("---\ntags: [budget]\n---\n# Budget\n\nSome text | with a pipe.\n\n| Item | Cost |"));
        assert!(out.contains("| Rent | 1250 |\n\n## Code"));
        assert!(!out.contains("Food"));
        assert!(out.ends_with("## Trips\nA | B | C\n--|:-:|--\nx | y\n"));
        // Edits elsewhere in the note since loading do not block it...
        let edited = NOTE.replace("Some text", "Edited text");
        let out = splice(&edited, &[(t[0].clone(), new.clone())]).unwrap();
        assert!(out.contains("Edited text") && out.contains("1250"));
        // ...an edit to the table itself does.
        let clash = NOTE.replace("410.5", "999");
        assert!(splice(&clash, &[(t[0].clone(), new)]).unwrap_err().contains("changed"));
    }

    #[test]
    fn crlf_notes_stay_crlf_and_an_unterminated_end_stays_so() {
        let note = "# T\r\n\r\n| a | b |\r\n|---|---|\r\n| 1 | 2 |";
        assert_eq!(eol_of(note), "\r\n");
        let t = tables(note);
        assert_eq!(t[0].rows[1], vec!["1", "2"]);
        let new = render(&[vec!["a".into(), "b".into()], vec!["3".into(), "4".into()]], &t[0].align, "\r\n");
        let out = splice(note, &[(t[0].clone(), new)]).unwrap();
        assert_eq!(out, "# T\r\n\r\n| a   | b   |\r\n| --- | --- |\r\n| 3   | 4   |");
    }
}
