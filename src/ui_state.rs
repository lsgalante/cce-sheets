//! Where the window was, so it comes back there: the workbook that was open,
//! and the sheet, selection and scroll it was showing. Saved when a file is
//! opened or saved under a new name, and when the window exits (`on_exit` —
//! which a restart, the remote's "Restart focused app", runs as any close
//! does); a launch reopens it — after a restart, at login, or when reopened.
//!
//! **A restart replays the same command line**, so a window opened as
//! `cce-spreadsheet a.xlsx` would always come back on `a.xlsx`. The file
//! therefore also records the file the command line named (`launched`): the
//! same command line again (or none) is this window reopening, and the saved
//! workbook wins, even one opened since; a command line naming a different
//! file is a new request, and it wins.
//!
//! Restoring only reopens the file as it is on disk: unsaved changes are the
//! recovery copy's (`book::save_recovery`), not this. A file that is gone is
//! not reopened.
//!
//! `cce_state_dir()/spreadsheet/state.json`, written whole to a temporary
//! file and renamed over, so a crash mid-write leaves the last one.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// What the window showed of its workbook.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct View {
    /// The selected sheet, by name (indices shift when sheets are added).
    pub sheet: String,
    pub row: i32,
    pub col: i32,
    /// The selected range, `(row1, col1, row2, col2)`.
    pub range: [i32; 4],
    /// The sheet's scroll, logical px.
    pub scroll: (f64, f64),
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    /// The open workbook's file (`None`: untitled).
    pub file: Option<PathBuf>,
    pub view: Option<View>,
    /// The file the command line named when this was saved, if any.
    pub launched: Option<PathBuf>,
}

/// The file this run's command line named. Unset until `initial` runs, and
/// nothing is saved until then, so the tests never write the real file.
static LAUNCHED: OnceLock<Option<PathBuf>> = OnceLock::new();

fn path() -> PathBuf {
    cce_ui::config::cce_state_dir().join("spreadsheet").join("state.json")
}

/// The saved place to reopen, if this launch is the window coming back;
/// `None`: open what the command line names, as before.
pub fn initial(argv: Option<&Path>) -> Option<Saved> {
    let launched = argv.map(|p| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()));
    let _ = LAUNCHED.set(launched.clone());
    let saved = std::fs::read_to_string(path()).ok().and_then(|s| serde_json::from_str::<Saved>(&s).ok());
    choose(launched.as_deref(), saved, |p| p.is_file())
}

/// The saved place when this command line is the one it was saved under (or
/// names nothing) and its file still exists.
fn choose(launched: Option<&Path>, saved: Option<Saved>, exists: impl Fn(&Path) -> bool) -> Option<Saved> {
    saved
        .filter(|s| launched.is_none() || s.launched.as_deref() == launched)
        .filter(|s| s.file.as_deref().is_some_and(&exists))
}

/// Where the window is now.
pub fn save(file: Option<&Path>, view: Option<View>) {
    let Some(launched) = LAUNCHED.get() else { return };
    let file = file.map(|p| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()));
    let saved = Saved { file, view, launched: launched.clone() };
    let path = path();
    let Some(dir) = path.parent() else { return };
    let tmp = path.with_extension("json.tmp");
    let written = std::fs::create_dir_all(dir)
        .and_then(|()| std::fs::write(&tmp, serde_json::to_vec(&saved).unwrap_or_default()))
        .and_then(|()| std::fs::rename(&tmp, &path));
    if let Err(e) = written {
        log::warn!("cannot save the open workbook to {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(file: &str, launched: Option<&str>) -> Saved {
        Saved { file: Some(file.into()), view: Some(View { sheet: "Sheet1".into(), row: 3, col: 2, range: [3, 2, 3, 2], scroll: (0.0, 40.0) }), launched: launched.map(PathBuf::from) }
    }

    fn all(_: &Path) -> bool {
        true
    }

    #[test]
    fn the_same_command_line_reopens_where_it_was() {
        // Launched on a.xlsx, opened b.xlsx, restarted with the same argv: b.
        let s = saved("/d/b.xlsx", Some("/d/a.xlsx"));
        assert_eq!(choose(Some(Path::new("/d/a.xlsx")), Some(s.clone()), all), Some(s));
        // A plain launch reopens whatever was open, however it was launched.
        let s = saved("/d/b.xlsx", Some("/d/a.xlsx"));
        assert_eq!(choose(None, Some(s.clone()), all), Some(s));
        let s = saved("/d/b.xlsx", None);
        assert_eq!(choose(None, Some(s.clone()), all), Some(s));
    }

    #[test]
    fn a_command_line_naming_another_file_wins() {
        let c = Some(Path::new("/d/c.xlsx"));
        assert_eq!(choose(c, Some(saved("/d/b.xlsx", Some("/d/a.xlsx"))), all), None);
        assert_eq!(choose(c, Some(saved("/d/b.xlsx", None)), all), None);
    }

    #[test]
    fn nothing_saved_or_nothing_there_is_not_restored() {
        assert_eq!(choose(None, None, all), None);
        assert_eq!(choose(Some(Path::new("/d/a.xlsx")), None, all), None);
        // The saved workbook was deleted or moved.
        let gone = |p: &Path| p != Path::new("/d/b.xlsx");
        assert_eq!(choose(None, Some(saved("/d/b.xlsx", None)), gone), None);
        assert_eq!(choose(Some(Path::new("/d/a.xlsx")), Some(saved("/d/b.xlsx", Some("/d/a.xlsx"))), gone), None);
        // An untitled workbook has nothing to reopen.
        let untitled = Saved { file: None, view: None, launched: None };
        assert_eq!(choose(None, Some(untitled), all), None);
    }

    #[test]
    fn the_file_reads_back() {
        let s = saved("/d/b.xlsx", Some("/d/a.xlsx"));
        let back: Saved = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }
}
