//! The right-click menu: a `cce-cloud --json` popup, the mechanism the
//! desktop's, the window's and cce-grid's menus use, so it looks and behaves
//! like every other menu in the DE. (cce-ui's own context menu delivers only
//! the toolkit's fixed `ContextAction`s to a widget; this app's actions are
//! its own.) Rows carry an action id; the choice comes back as that id.

use crate::view::Hit;

pub const CUT: &str = "cut";
pub const COPY: &str = "copy";
pub const PASTE: &str = "paste";
pub const CLEAR: &str = "clear";
pub const INSERT_ROWS_ABOVE: &str = "insert-rows-above";
pub const INSERT_ROWS_BELOW: &str = "insert-rows-below";
pub const DELETE_ROWS: &str = "delete-rows";
pub const INSERT_COLS_LEFT: &str = "insert-cols-left";
pub const INSERT_COLS_RIGHT: &str = "insert-cols-right";
pub const DELETE_COLS: &str = "delete-cols";
pub const FIT_COLS: &str = "fit-cols";

/// The menu's title and rows (id, label) for a right-click on `hit`.
pub fn entries(hit: Hit, can_paste: bool) -> (String, Vec<(&'static str, &'static str)>) {
    let mut rows = vec![(CUT, "Cut"), (COPY, "Copy")];
    if can_paste {
        rows.push((PASTE, "Paste"));
    }
    let title = match hit {
        Hit::Cell { .. } => {
            rows.extend([
                (CLEAR, "Clear contents"),
                (INSERT_ROWS_ABOVE, "Insert rows above"),
                (INSERT_ROWS_BELOW, "Insert rows below"),
                (DELETE_ROWS, "Delete rows"),
                (INSERT_COLS_LEFT, "Insert columns left"),
                (INSERT_COLS_RIGHT, "Insert columns right"),
                (DELETE_COLS, "Delete columns"),
            ]);
            "Cells"
        }
        Hit::ColHeader(_) => {
            rows.extend([
                (CLEAR, "Clear contents"),
                (INSERT_COLS_LEFT, "Insert columns left"),
                (INSERT_COLS_RIGHT, "Insert columns right"),
                (DELETE_COLS, "Delete columns"),
                (FIT_COLS, "Fit width to contents"),
            ]);
            "Columns"
        }
        Hit::RowHeader(_) => {
            rows.extend([
                (CLEAR, "Clear contents"),
                (INSERT_ROWS_ABOVE, "Insert rows above"),
                (INSERT_ROWS_BELOW, "Insert rows below"),
                (DELETE_ROWS, "Delete rows"),
            ]);
            "Rows"
        }
        _ => return (String::new(), Vec::new()),
    };
    (title.to_string(), rows)
}

/// Show the menu at the pointer and wait for a choice; the chosen id, or
/// `None` when it was dismissed. Blocks: call it off the event loop.
///
/// The pointer's place on the screen has to be asked of the compositor — a
/// client knows where its own surface was pressed, never where that is.
pub fn show((title, rows): &(String, Vec<(&'static str, &'static str)>)) -> Option<String> {
    use std::io::Write;
    let loc = std::process::Command::new(de_bin("ccectl")).arg("pointer-location").output().ok()?;
    let loc = String::from_utf8_lossy(&loc.stdout);
    let coord = |key: &str| -> Option<i32> {
        loc.split_whitespace().find_map(|t| t.strip_prefix(key)).and_then(|v| v.trim().parse::<f64>().ok()).map(|v| v.round() as i32)
    };
    let (x, y) = (coord("x=")?, coord("y=")?);
    let layout = layout(title, rows).to_string();
    let mut child = std::process::Command::new(de_bin("cce-cloud"))
        .args(["--json", "-x", &x.to_string(), "-y", &y.to_string(), "--parent-app-id", "cce-sheets"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(layout.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    chosen(&out.stdout)
}

/// The popup's page: a titled column of left-justified buttons.
fn layout(title: &str, rows: &[(&str, &str)]) -> serde_json::Value {
    let widgets: Vec<serde_json::Value> = rows.iter().map(|(id, label)| serde_json::json!({"type": "button", "text": label, "id": id})).collect();
    serde_json::json!({"pages": [{"title": title, "justify": "left", "widgets": widgets}]})
}

/// The button id in cce-cloud's reply.
fn chosen(reply: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(reply).ok()?;
    v.get("button")?.as_str().map(str::to_string)
}

/// A DE binary: beside this one (an installed set, or a workspace build's
/// target/), else by name from PATH.
fn de_bin(name: &str) -> std::path::PathBuf {
    if let Some(p) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(name))).filter(|p| p.exists()) {
        return p;
    }
    std::path::PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menus_fit_what_was_clicked() {
        let (t, rows) = entries(Hit::ColHeader(3), true);
        assert_eq!(t, "Columns");
        let ids: Vec<&str> = rows.iter().map(|r| r.0).collect();
        assert!(ids.contains(&FIT_COLS) && ids.contains(&PASTE) && !ids.contains(&DELETE_ROWS));
        let (_, rows) = entries(Hit::Cell { row: 1, col: 1 }, false);
        assert!(!rows.iter().any(|r| r.0 == PASTE));
        assert!(entries(Hit::Corner, true).1.is_empty());
    }

    #[test]
    fn the_layout_and_reply_speak_cce_clouds_json() {
        let l = layout("Rows", &[(DELETE_ROWS, "Delete rows")]);
        assert_eq!(l["pages"][0]["widgets"][0]["id"], DELETE_ROWS);
        assert_eq!(chosen(br#"{"button":"delete-rows","checkboxes":{}}"#).as_deref(), Some(DELETE_ROWS));
        assert_eq!(chosen(b""), None);
    }
}
