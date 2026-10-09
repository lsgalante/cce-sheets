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
cell's address and formula in the bar. Planned next: editing and save (M2),
clipboard/fill/resize (M3), formatting (M4), an MCP server (M5), vault markdown
tables (M6).

## Layout

| file | role |
|---|---|
| `src/main.rs` | `SheetsApp` (the `Application`): window layout, bar, sheet tabs, input |
| `src/book.rs` | `Book`: open .xlsx (IronCalc) or delimited text (sniffed separator); `column_name` |
| `src/axis.rs` | `Axis`: one dimension's sizes — default + sparse runs, O(log n) `start` / `index_at` |
| `src/view.rs` | `View`: headers, frozen quadrants, scroll, hit test, scroll-into-view (pure, tested) |
| `src/grid.rs` | sheet geometry from the model, and the painter (fills, gridlines, text, cursor, headers) |

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
and `ctl set-mode fullscreen cce-sheets` — the scale-2 output is only 640×360
logical). Never run a new build on the live seat.

Not done yet: the app icon (`cce-icons/hicolor`), merged cells, wrapped text,
borders, conditional formatting, ODS.
