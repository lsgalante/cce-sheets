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

Milestone 3 (done): ctrl+c / ctrl+x / ctrl+v (cells with shifted
references and styles; text from other apps as tab-separated values typed
in), a dashed marquee on the copied range, the fill handle (series or copy,
with a preview), header-edge resizing (double-click fits a column / resets a
row), ctrl+shift+= / ctrl+- insert and delete, and a right-click menu.

Milestone 4 (done): a formatting toolbar under the bar — number format,
bold / italic / underline (also ctrl+b / ctrl+i / ctrl+u), alignment, text
and fill colours, borders — drawn as ButtonStrip-style strips with cce-icons
glyphs; the grid now draws borders, underline and strikethrough; typing a
function name in a formula offers completions (Up/Down, Tab or Enter, Escape).

Milestone 5 (done): an MCP server, so agents read and edit the workbook in
the running window — see "MCP" below.

Planned next: vault markdown tables (M6).

## Layout

| file | role |
|---|---|
| `src/main.rs` | `SheetsApp` (the `Application`): window layout, bar, sheet tabs, input |
| `src/book.rs` | `Book`: open .xlsx (IronCalc) or delimited text (sniffed separator); `column_name` |
| `src/axis.rs` | `Axis`: one dimension's sizes — default + sparse runs, O(log n) `start` / `index_at` |
| `src/view.rs` | `View`: headers, frozen quadrants, scroll, hit test, scroll-into-view (pure, tested) |
| `src/grid.rs` | sheet geometry from the model, and the painter (fills, gridlines, text, cursor, headers) |
| `src/edit.rs` | `CellEdit` (one `LineEdit` shown in the cell and the bar), key classes, shaped caret geometry |
| `src/clip.rs` | copy/cut/paste: IronCalc's clipboard read through serde, system clipboard vs copied cells |
| `src/ops.rs` | insert/delete rows and columns, fill-handle reach and fill, fitting a column |
| `src/menu.rs` | `cce-cloud --json` popups: the right-click menu and the toolbar's format/colour/border menus |
| `src/format.rs` | the selected cell's formatting (`Active`) and the style writes: toggles, alignment, number formats, palette, borders |
| `src/toolbar.rs` | toolbar layout, hit test and paint; `strip`, the recessed-strip painter the sheet tabs share |
| `src/mcp.rs` | MCP tools (`workbook`, `read_range`, `write_cells`, `clear_range`, `format_range`, `change_lines`, `find`, `add_sheet`, `rename_sheet`, `select`, `undo`, `redo`, `save`, `open`) |
| `src/addr.rs` | A1 addresses: cells, ranges, whole columns/rows, `Sheet!` prefixes |
| `src/functions.rs` | every function's name, GENERATED as field accessors of `language::Functions` (regenerate on an ironcalc bump) |

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
- **Paste picks its source by the system clipboard**: if it still holds the
  text this window copied (or cannot be read), the copied cells paste
  (formulas shift); otherwise its text pastes as typed values. IronCalc's
  `Clipboard` fields are crate-private but it serializes them, which is how
  `clip.rs` reads them.
- **The right-click menu is `cce-cloud --json`**, run on a thread with the
  choice sent back as `Message::Menu`: cce-ui's context menu only carries its
  own `ContextAction`s to a widget. Ids are the `menu::` constants, never labels.
- **Whole-sheet spans count what is used**: a whole-column selection inserts one
  row, deletes only used rows, and the copy marquee is clamped to the body (a
  column is 26 million px tall).
- A resize previews by swapping in `Axis::with_size` and writes once on release
  (one undo step), to every selected whole column/row when the dragged one is
  among them.
- **Glyphs come from cce-icons** (`bold`, `italic`, `underline`,
  `align-left/center/right`, `fill`, `borders`, plus `font`, `chevron-down`) —
  never letters in a fallback font. The app icon is `table.svg`
  (`hicolor/scalable/apps/cce-sheets.svg`).
- **Anything drawn over text must hide that text**: the editor and the
  completion list are passed to `paint_sheet(.., hide)` (cells and header
  labels under them draw no text) and to `toolbar::paint`. A box alone cannot
  cover text — the glyph pass is above every fill.
- **Function names for completion are IronCalc's own** (`language::Functions`
  fields): its function table is private. `every_name_is_a_function_the_engine_knows`
  fails if one stops parsing; a function IronCalc adds is missed until
  `functions.rs` is regenerated from its `language/mod.rs`.
- `BorderArea`, like the clipboard, is built through serde (private fields,
  public `Deserialize`). Dark borders on the bare grid take the label colour,
  as text does.
- **IronCalc's `paste_csv_string` has two traps** (both met by this app):
  its reader is strict about record lengths and silently SKIPS a row shorter
  than the first — every paste goes through `clip::as_tsv`, which pads — and
  it ends by selecting what it pasted, which it refuses unless the selected
  cell is that range's corner, so a write elsewhere points the selection there
  and puts the person's selection back after (`mcp.rs` `write_cells`).
- Model strings borrow for the model's life (`Model<'a>`): construct with the
  `'static` constants in `book.rs`, then rename.

## MCP

`claude mcp add --transport http cce-sheets http://127.0.0.1:3003/mcp`.
cce-ui's tools-only server (`cce_ui::mcp`); calls run on the event loop
against the model the window draws, through the same paths as the keyboard:
an open edit is committed first, every write is an undo step the person at
the window can take back, the window's selection does not move unless the
call is `select`, and the window repaints after. Each window is its own
process, so only the first one open holds the port; a later one logs that it
is taken. `CCE_SHEETS_MCP_PORT` moves it — use one (3903) for a shadow, since
a live window may hold 3003. `mcp.rs`'s tests build a real `SheetsApp` and
drive `mcp_dispatch`, and `every_listed_tool_is_dispatched` keeps the tool
list and the dispatcher in step. To drive the HTTP side by hand:
`curl -s -X POST http://127.0.0.1:3903/mcp -H 'Content-Type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"workbook","arguments":{}}}'`.

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
(ctrl+z); `ccectl keypress` carries no modifier state. The right-click menu
opens in the shadow (the app runs ccectl and cce-cloud with its own display)
and is clicked like any window; seed an external clipboard with
`cce-shadow run sh -c 'printf … | wl-copy'`. Drive from bash, not zsh: zsh
does not word-split a `$C`-style command variable. Never press ctrl+o,
ctrl+shift+s or ctrl+s on an untitled book in a shadow: the portal file chooser
opens on the LIVE display. Never run a new build on the live seat.

**Installing while cce-ui is dirty** (another session's work in the tree): a
workspace build would ship it. Build this crate alone at its pin instead —
`git archive HEAD` into a scratch dir, `cargo build --release --locked` with a
private `CARGO_TARGET_DIR`, then `install -m755 … ~/.local/bin/cce-sheets.new &&
mv` (the desktop entry is ccebuild's; reinstall it with `ccebuild install
--no-build cce-sheets` only from a clean-tree build).

Not done yet: merged cells, wrapped text and vertical alignment, per-cell font
sizes and faces (every cell is the DE label font), conditional formatting, ODS.
