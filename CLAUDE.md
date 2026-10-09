# CLAUDE.md — cce-sheets

Spreadsheets for the cce desktop. Read the workspace guide
`../cce-compositor/WORKSPACE.md` first (multi-repo layout, `ccebuild`, concurrent
sessions) and `../cce-ui/CLAUDE.md` for the toolkit.

## What it is

The workbook is **IronCalc**'s `UserModel` (`ironcalc` 0.8, MIT/Apache): cells,
formulas, recalculation, styles, undo, the selected sheet/cell/range, keyboard
navigation, and .xlsx import/export all live there. This app draws the model
with cce-ui and turns input into model calls. Do not keep a second copy of
anything the model knows (selection, sheet list, sizes): read it back.

Milestone 1 (done): a viewer. Open .xlsx / .csv / .tsv from argv or ctrl+o,
scroll with frozen panes, select cells/ranges/rows/columns, switch sheets, see a
cell's address and formula in the bar.

Milestone 2 (done): editing and saving. Type over a cell, F2 / double-click /
a press in the bar to edit its content, Enter/Tab/arrows commit and move,
Escape abandons; Delete clears the selection; ctrl+z / ctrl+shift+z undo the
edit's typing while editing, else the workbook. ctrl+s saves (xlsx, or the
selected sheet's values as delimited text, with a note saying what that
dropped); ctrl+shift+s saves as. The title carries `*` while unsaved.

Planned next: clipboard/fill/resize (M3), formatting (M4), an MCP server (M5),
vault markdown tables (M6).

## Layout

| file | role |
|---|---|
| `src/main.rs` | `SheetsApp` (the `Application`): window layout, bar, sheet tabs, input |
| `src/book.rs` | `Book`: open .xlsx (IronCalc) or delimited text (sniffed separator); `column_name` |
| `src/axis.rs` | `Axis`: one dimension's sizes — default + sparse runs, O(log n) `start` / `index_at` |
| `src/view.rs` | `View`: headers, frozen quadrants, scroll, hit test, scroll-into-view (pure, tested) |
| `src/grid.rs` | sheet geometry from the model, and the painter (fills, gridlines, text, cursor, headers) |
| `src/edit.rs` | `CellEdit` (one `LineEdit` shown in the cell and the bar), key classes, shaped caret geometry |

## Rules

- **Only on-screen cells are touched.** A sheet is 16,384 × 1,048,576; nothing may
  walk an axis or the sheet data per frame. Sizes come from `Axis`, cells from the
  visible `Span`s.
- **IronCalc sizes are px, used as logical px** (default column 90, row 25).
- **Text is set in the DE label font, which is monospace**; widths are
  `chars × Face::char_w`. `char_w` is the difference of two measured runs of
  zeros, because `measure_text_width` reports INK extent: dividing one run by its
  length came out short and right-aligned text drifted past its wall.
- **Colours**: a cell with its own fill keeps its own colours; on the bare grid,
  unset or dark font colours become the DE label colour (workbooks assume white
  paper).
- **General-format numbers shed decimals, then go scientific, before `###`**
  (`fit_general`); fixed formats go straight to `###`, as in Excel.
- **Editing**: one `LineEdit` is the edit; the in-cell editor and the bar both
  draw it. Typing over a cell is enter mode (Left/Right commit and move); F2, a
  double-click or the bar is edit mode (they move the caret). Caret, selection
  and click positions come from `engine::shaped_cluster_offsets`, never a
  measured width.
- **Text is drawn above every fill**, in its own pass, so a box cannot hide a
  value: the in-cell editor works by `paint_sheet(.., hide)` drawing no text
  under it and stopping overflow at its edge.
- **Saves are atomic** (temp file beside the target, synced, renamed).
  IronCalc's own `save_to_xlsx` refuses an existing file and unwraps the create;
  use `save_xlsx_to_writer` through `book::write_atomic`.
- **No close prompt exists in the toolkit**, so `on_exit` commits an open edit
  and writes unsaved changes to `$XDG_STATE_HOME/cce-sheets/recovered/` (an open
  over a dirty workbook does the same).
- Every model change goes through `SheetsApp::changed` (unsaved mark, sizes
  re-read, and the engine's collaboration send-queue dropped — it grows forever
  otherwise).
- Delete clamps the range to the used extent: `range_clear_contents` walks every
  row of the range, and a whole column is a million.
- Model strings borrow for the model's life (`Model<'a>`): construct with the
  `'static` constants in `book.rs`, then rename.

## Build, test, verify

```sh
cargo build -p cce-sheets
cargo test  -p cce-sheets            # headless: axis, view, loaders, geometry, number fitting
cargo clippy -p cce-sheets --all-targets
make install                         # release build + ccebuild install --no-build cce-sheets
```

Verify drawing and input in a shadow at **both** scale 1 and `--scale 2`
(`cce-shadow start --new [--scale 2]`, then
`cce-shadow spawn env CCE_FONTS_DIR=… CCE_ICONS_DIR=… /abs/path/target/release/cce-sheets file.xlsx`
and `ctl set-mode fullscreen cce-sheets` once the window is listed — the scale-2
output is only 640×360 logical). Keys with modifiers need a real keyboard:
`cce-shadow run ../cce-compositor/verify/clients/target/release/vkey mod:4 44`
(ctrl+z); `ccectl keypress` carries no modifier state. Never press ctrl+o,
ctrl+shift+s or ctrl+s on an untitled book in a shadow: the portal file chooser
opens on the LIVE display. Never run a new build on the live seat.

**Installing while cce-ui is dirty** (another session's work in the tree): a
workspace build would ship it. Build this crate alone at its pin instead —
`git archive HEAD` into a scratch dir, `cargo build --release --locked` with a
private `CARGO_TARGET_DIR`, then `install -m755 … ~/.local/bin/cce-sheets.new &&
mv` (the desktop entry is ccebuild's; reinstall it with `ccebuild install
--no-build cce-sheets` only from a clean-tree build).

Not done yet: the app icon (`cce-icons/hicolor`), merged cells, wrapped text,
borders, conditional formatting, ODS.
