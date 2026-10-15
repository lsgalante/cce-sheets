//! MCP tools, so agents can read and edit the workbook in the running
//! window (cce-ui's tools-only server, as cce-designer and cce-notes use):
//!
//! ```sh
//! claude mcp add --transport http cce-spreadsheet http://127.0.0.1:3003/mcp
//! ```
//!
//! Each window is its own process, so only the first one open serves; a
//! later one finds the port taken and logs it. `CCE_SPREADSHEET_MCP_PORT` moves
//! the port, for a second instance in a shadow.
//!
//! Calls run on the app's event loop against the same model the window
//! draws, and go through the same paths as the keyboard and toolbar: a write
//! is an undo step the person at the window can take back, an edit open in
//! the window is committed first, and the window repaints after.
//! Addresses are A1 notation (`B2`, `A1:C5`, `B:D`, `2:4`, `Data!A1`).

use cce_ui::mcp::{McpServer, McpTool, McpToolCall, ToolClass};
use ironcalc::base::expressions::types::Area;
use serde_json::{json, Value};

use crate::clip::as_tsv;
use crate::addr::{cell_name, parse_cell, parse_range, range_name, split_sheet};
use crate::book::{Book, FileKind};
use crate::ops::Line;
use crate::{format, grid, ops, Message, SpreadsheetApp};

const DEFAULT_PORT: u16 = 3003;
/// The most cells one `read_range` returns.
const MAX_READ: usize = 20_000;

/// The most rows and columns of the selection `context` quotes.
const CONTEXT_ROWS: i32 = 30;
const CONTEXT_COLS: i32 = 12;

/// What the model is told about the spreadsheet when a client connects.
const BRIEF: &str = "\
cce-spreadsheet is the user's spreadsheet app (IronCalc formulas, .xlsx/.csv/.tsv, and the \
tables of a Markdown note); the user keeps budgets and other records in it. Call `context` \
first: it names the open file, the sheet on show, the selection and the selected cells' values. \
Addresses are A1 notation (`B2`, `A1:C5`, `Data!A1`). Read with `read_range` and `find` before \
writing. `write_cells` types values as a person would (`=` starts a formula). Every write is \
one step of the window's undo history: take it back with `undo`. Nothing reaches the file \
until `save`. The user's notes and calendar may be connected as well.";

pub fn start(sender: calloop::channel::Sender<Message>) {
    let port = std::env::var("CCE_SPREADSHEET_MCP_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_PORT);
    server().fixed_port(port).start(sender, Message::Mcp);
}

/// The server: the tools, what each does to the world, the brief, and the
/// vault apps a client should load beside it.
fn server() -> McpServer {
    McpServer::new("cce-spreadsheet", tools())
        .instructions(BRIEF)
        .companions(&["cce-notes", "cce-calendar"])
        .classify(ToolClass::Read, &["workbook", "read_range", "find"])
        .classify(
            ToolClass::Edit,
            &["write_cells", "clear_range", "format_range", "change_lines", "add_sheet", "rename_sheet", "select", "undo", "redo"],
        )
        // Saving replaces the file on disk; opening replaces the workbook in
        // the window. Neither is a step of the undo history.
        .classify(ToolClass::Outward, &["save", "open"])
}

/// The `context` answer, from the `workbook` answer and the selected cells'
/// shown values (rows of tab-separated values).
fn context_from(workbook: &Value, selected_values: Option<String>) -> cce_ui::mcp::Context {
    let file = workbook["file"].as_str();
    let sel = &workbook["selected"];
    cce_ui::mcp::Context {
        document: Some(cce_ui::mcp::ContextDocument {
            title: file
                .and_then(|f| std::path::Path::new(f).file_name())
                .map_or_else(|| "Untitled workbook".to_string(), |n| n.to_string_lossy().into_owned()),
            path: file.map(str::to_string),
        }),
        view: json!({ "sheet": sel["sheet"], "sheets": workbook["sheets"], "editing": workbook["editing"] }),
        selection: sel["range"].as_str().map(|r| format!("{}!{r}", sel["sheet"].as_str().unwrap_or_default())).into_iter().collect(),
        dirty: workbook["unsaved"].as_bool(),
        undo: Some("the `undo` tool: the window's undo history, one step per write".into()),
        summary: selected_values,
        ..cce_ui::mcp::Context::new(concat!("cce-spreadsheet ", env!("CARGO_PKG_VERSION")))
    }
}

pub fn tools() -> Vec<McpTool> {
    let tool = |name: &str, description: &str, input_schema: Value| McpTool { name: name.into(), description: description.into(), input_schema };
    let sheet = json!({ "type": "string", "description": "Sheet name; default the sheet the window shows. A `Sheet!` prefix on an address also names it." });
    let range = json!({ "type": "string", "description": "A1 address: `B2`, `A1:C5`, whole columns `B:D`, whole rows `2:4`, optionally `Sheet!`-prefixed" });
    vec![
        cce_ui::mcp::context_tool(),
        tool(
            "workbook",
            "The open workbook: its file, whether it has unsaved changes, each sheet (name, visible, used range, frozen rows/columns) and what the window has selected.",
            json!({ "type": "object", "properties": {} }),
        ),
        tool(
            "read_range",
            "Cell values as shown (formatted), row by row. Whole columns or rows are cut to the used range; at most 20000 cells. With `formulas`, also each cell's content as typed (a formula's text, e.g. `=SUM(A1:A3)`).",
            json!({ "type": "object", "properties": { "range": range, "sheet": sheet, "formulas": { "type": "boolean" } }, "required": ["range"] }),
        ),
        tool(
            "write_cells",
            "Type values into a rectangle starting at `start`, one row per inner array, as a person typing them would: `=...` is a formula, `12.5` a number, `TRUE` a boolean, \"\" clears. The whole rectangle (widest row) is written, so a short row clears the rest of its width. One undo step.",
            json!({ "type": "object", "properties": {
                "start": { "type": "string", "description": "Top-left cell, e.g. `B2` or `Data!B2`" },
                "sheet": sheet,
                "values": { "type": "array", "items": { "type": "array", "items": {} } }
            }, "required": ["start", "values"] }),
        ),
        tool(
            "clear_range",
            "Clear the contents of a range (formatting stays).",
            json!({ "type": "object", "properties": { "range": range, "sheet": sheet }, "required": ["range"] }),
        ),
        tool(
            "format_range",
            "Format a range; only the fields given change. `align`: general|left|center|right. `number_format`: an Excel format code (`#,##0.00`, `0.0%`, `yyyy-mm-dd`, `general`, …). Colours `#rrggbb`, or \"\" for automatic text / no fill. `borders`: all|outer|inner|bottom|none (thin).",
            json!({ "type": "object", "properties": {
                "range": range, "sheet": sheet,
                "bold": { "type": "boolean" }, "italic": { "type": "boolean" }, "underline": { "type": "boolean" },
                "align": { "type": "string", "enum": ["general", "left", "center", "right"] },
                "number_format": { "type": "string" },
                "font_color": { "type": "string" }, "fill_color": { "type": "string" },
                "borders": { "type": "string", "enum": ["all", "outer", "inner", "bottom", "none"] }
            }, "required": ["range"] }),
        ),
        tool(
            "change_lines",
            "Insert or delete rows or columns. `at` is a row number or a column letter; inserted lines go before it. References in formulas follow.",
            json!({ "type": "object", "properties": {
                "action": { "type": "string", "enum": ["insert", "delete"] },
                "lines": { "type": "string", "enum": ["rows", "columns"] },
                "at": { "type": ["string", "integer"] },
                "count": { "type": "integer", "description": "Default 1" },
                "sheet": sheet
            }, "required": ["action", "lines", "at"] }),
        ),
        tool(
            "find",
            "Cells whose shown value or typed content contains `query` (any case), on one sheet or all visible sheets. Returns sheet, cell, value and content.",
            json!({ "type": "object", "properties": {
                "query": { "type": "string" }, "sheet": sheet,
                "limit": { "type": "integer", "description": "Default 50" }
            }, "required": ["query"] }),
        ),
        tool(
            "add_sheet",
            "Add a sheet (named, or Sheet<n>) and show it.",
            json!({ "type": "object", "properties": { "name": { "type": "string" } } }),
        ),
        tool(
            "rename_sheet",
            "Rename a sheet.",
            json!({ "type": "object", "properties": { "sheet": { "type": "string" }, "name": { "type": "string" } }, "required": ["sheet", "name"] }),
        ),
        tool(
            "select",
            "Show a range in the window: switch to its sheet, select it and scroll it into view.",
            json!({ "type": "object", "properties": { "range": range, "sheet": sheet }, "required": ["range"] }),
        ),
        tool("undo", "Undo the workbook's last change (the window's or an agent's).", json!({ "type": "object", "properties": {} })),
        tool("redo", "Redo the last undone change.", json!({ "type": "object", "properties": {} })),
        tool(
            "save",
            "Save the workbook: to its own file, or to `path` (.xlsx, or .csv/.tsv for the shown sheet's values), which becomes its file. Returns a note when the format kept less than the workbook holds.",
            json!({ "type": "object", "properties": { "path": { "type": "string", "description": "Absolute path" } } }),
        ),
        tool(
            "open",
            "Open a workbook (.xlsx, .csv, .tsv) in the window in place of the current one, or a Markdown note (.md), whose tables open one sheet each and `save` writes back into the note in place. Unsaved changes to the current one are kept in a recovery copy, whose path is returned.",
            json!({ "type": "object", "properties": { "path": { "type": "string", "description": "Absolute path" } }, "required": ["path"] }),
        ),
    ]
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key).and_then(Value::as_str).ok_or_else(|| format!("missing string argument `{key}`"))
}

/// A JSON value as a person would type it into a cell.
fn typed(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

impl SpreadsheetApp {
    pub(crate) fn mcp_call(&mut self, call: McpToolCall) {
        let result = self.mcp_dispatch(&call.name, &call.arguments);
        let _ = call.reply.send(result);
    }

    /// The sheet a call means: a `Sheet!` prefix on its address, else its
    /// `sheet` argument, else the one the window shows.
    fn mcp_sheet(&self, args: &Value, prefix: Option<String>) -> Result<u32, String> {
        let named = prefix.or_else(|| args.get("sheet").and_then(Value::as_str).map(str::to_string));
        let Some(name) = named else {
            return Ok(self.sheet());
        };
        self.book
            .model
            .get_worksheets_properties()
            .iter()
            .position(|p| p.name.eq_ignore_ascii_case(&name))
            .map(|i| i as u32)
            .ok_or_else(|| format!("no sheet named `{name}`"))
    }

    /// A range argument with the sheet it is on.
    fn mcp_range(&self, args: &Value, key: &str) -> Result<(u32, [i32; 4]), String> {
        let (prefix, rest) = split_sheet(str_arg(args, key)?);
        let range = parse_range(rest)?;
        Ok((self.mcp_sheet(args, prefix)?, range))
    }

    /// Before a write: an edit open in the window goes in first, so the
    /// write does not land under it and get overwritten by its commit.
    fn mcp_commit(&mut self) -> Result<(), String> {
        if self.commit_edit() {
            Ok(())
        } else {
            Err("the cell being edited in the window does not accept its text; finish or cancel that edit first".into())
        }
    }

    fn mcp_dispatch(&mut self, name: &str, args: &Value) -> Result<Value, String> {
        match name {
            cce_ui::mcp::CONTEXT_TOOL => Ok(self.mcp_context()),
            "workbook" => Ok(self.mcp_workbook()),
            "read_range" => {
                let (sheet, range) = self.mcp_range(args, "range")?;
                let (used_r, used_c) = grid::used_extent(&self.book.model, sheet);
                let [r1, c1, mut r2, mut c2] = range;
                r2 = r2.min(used_r.max(r1));
                c2 = c2.min(used_c.max(c1));
                let cells = (r2 - r1 + 1) as usize * (c2 - c1 + 1) as usize;
                if cells > MAX_READ {
                    return Err(format!("{} is {cells} cells; read at most {MAX_READ} at a time", range_name([r1, c1, r2, c2])));
                }
                let m = &self.book.model;
                let grid_of = |f: &dyn Fn(i32, i32) -> String| -> Vec<Vec<String>> { (r1..=r2).map(|r| (c1..=c2).map(|c| f(r, c)).collect()).collect() };
                let values = grid_of(&|r, c| m.get_formatted_cell_value(sheet, r, c).unwrap_or_default());
                let mut out = json!({ "sheet": self.sheet_name(sheet), "range": range_name([r1, c1, r2, c2]), "values": values });
                if args.get("formulas").and_then(Value::as_bool).unwrap_or(false) {
                    out["formulas"] = json!(grid_of(&|r, c| m.get_cell_content(sheet, r, c).unwrap_or_default()));
                }
                Ok(out)
            }
            "write_cells" => {
                let (prefix, rest) = split_sheet(str_arg(args, "start")?);
                let (row, col) = parse_cell(rest)?;
                let sheet = self.mcp_sheet(args, prefix)?;
                let rows: Vec<Vec<String>> = args
                    .get("values")
                    .and_then(Value::as_array)
                    .ok_or("missing array argument `values`")?
                    .iter()
                    .map(|r| r.as_array().map(|cells| cells.iter().map(typed).collect()).ok_or("each row of `values` must be an array"))
                    .collect::<Result<_, _>>()?;
                let width = rows.iter().map(Vec::len).max().unwrap_or(0);
                if rows.is_empty() || width == 0 {
                    return Err("`values` is empty".into());
                }
                self.mcp_commit()?;
                let area = Area { sheet, row, column: col, width: width as i32, height: rows.len() as i32 };
                // IronCalc's paste ends by selecting what it pasted, which it
                // refuses unless the selection already starts there; so point
                // it there and give the person at the window theirs back.
                let m = &mut self.book.model;
                let before = m.get_selected_view();
                m.set_selected_sheet(sheet)?;
                m.set_selected_cell(row, col)?;
                let pasted = m.paste_csv_string(&area, &as_tsv(&rows)?);
                let _ = m.set_selected_sheet(before.sheet);
                let _ = m.set_selected_cell(before.row, before.column);
                let [r1, c1, r2, c2] = before.range;
                let _ = m.set_selected_range(r1, c1, r2, c2);
                pasted?;
                self.changed();
                let written = [row, col, row + rows.len() as i32 - 1, col + width as i32 - 1];
                Ok(json!({ "sheet": self.sheet_name(sheet), "written": range_name(written) }))
            }
            "clear_range" => {
                let (sheet, range) = self.mcp_range(args, "range")?;
                let (used_r, used_c) = grid::used_extent(&self.book.model, sheet);
                let [r1, c1, r2, c2] = range;
                let (r2, c2) = (r2.min(used_r), c2.min(used_c));
                self.mcp_commit()?;
                if r2 >= r1 && c2 >= c1 {
                    let area = Area { sheet, row: r1, column: c1, width: c2 - c1 + 1, height: r2 - r1 + 1 };
                    self.book.model.range_clear_contents(&area)?;
                    self.changed();
                }
                Ok(json!("cleared"))
            }
            "format_range" => {
                let (sheet, range) = self.mcp_range(args, "range")?;
                self.mcp_commit()?;
                let mut done = Vec::new();
                let m = &mut self.book.model;
                for (key, path) in [("bold", "font.b"), ("italic", "font.i"), ("underline", "font.u")] {
                    if let Some(b) = args.get(key).and_then(Value::as_bool) {
                        format::set(m, sheet, range, path, if b { "true" } else { "false" })?;
                        done.push(key);
                    }
                }
                for (key, path) in [("align", "alignment.horizontal"), ("number_format", "num_fmt"), ("font_color", "font.color"), ("fill_color", "fill.color")] {
                    if let Some(v) = args.get(key).and_then(Value::as_str) {
                        format::set(m, sheet, range, path, v)?;
                        done.push(key);
                    }
                }
                if let Some(b) = args.get("borders").and_then(Value::as_str) {
                    let kind = format::BORDERS.iter().find(|(id, _, _)| id.strip_prefix("border-") == Some(b)).map(|(_, _, k)| *k).ok_or_else(|| format!("unknown borders `{b}`"))?;
                    format::borders(m, sheet, range, kind)?;
                    done.push("borders");
                }
                if done.is_empty() {
                    return Err("nothing to change: give at least one of bold, italic, underline, align, number_format, font_color, fill_color, borders".into());
                }
                self.changed();
                Ok(json!({ "changed": done }))
            }
            "change_lines" => {
                let sheet = self.mcp_sheet(args, None)?;
                let insert = match str_arg(args, "action")? {
                    "insert" => true,
                    "delete" => false,
                    other => return Err(format!("unknown action `{other}`")),
                };
                let line = match str_arg(args, "lines")? {
                    "rows" => Line::Rows,
                    "columns" => Line::Cols,
                    other => return Err(format!("unknown lines `{other}`")),
                };
                let at = match args.get("at") {
                    Some(Value::Number(n)) => n.as_i64().map(|n| n as i32),
                    Some(Value::String(s)) if line == Line::Cols => crate::addr::column_number(s).or_else(|| s.parse().ok()),
                    Some(Value::String(s)) => s.parse().ok(),
                    _ => None,
                }
                .filter(|&n| n >= 1)
                .ok_or("`at` must be a row number, or a column letter or number")?;
                let count = args.get("count").and_then(Value::as_i64).unwrap_or(1).clamp(1, 10_000) as i32;
                self.mcp_commit()?;
                let range = match line {
                    Line::Rows => [at, 1, at + count - 1, 1],
                    Line::Cols => [1, at, 1, at + count - 1],
                };
                let used = grid::used_extent(&self.book.model, sheet);
                let m = &mut self.book.model;
                if insert {
                    ops::insert(m, sheet, range, line, false, used)?;
                } else {
                    ops::delete(m, sheet, range, line, used)?;
                }
                self.copied = None;
                self.changed();
                Ok(json!("done"))
            }
            "find" => {
                let query = str_arg(args, "query")?.to_lowercase();
                let limit = args.get("limit").and_then(Value::as_u64).map(|n| n.clamp(1, 1000) as usize).unwrap_or(50);
                let sheets: Vec<u32> = if args.get("sheet").is_some() {
                    vec![self.mcp_sheet(args, None)?]
                } else {
                    self.visible_sheets().into_iter().map(|(i, _)| i).collect()
                };
                let m = &self.book.model;
                let mut hits = Vec::new();
                'sheets: for sheet in sheets {
                    let ws = &m.get_model().workbook.worksheets[sheet as usize];
                    let mut cells: Vec<(i32, i32)> = ws.sheet_data.iter().flat_map(|(&r, row)| row.keys().map(move |&c| (r, c))).collect();
                    cells.sort_unstable();
                    for (r, c) in cells {
                        let value = m.get_formatted_cell_value(sheet, r, c).unwrap_or_default();
                        let content = m.get_cell_content(sheet, r, c).unwrap_or_default();
                        if value.to_lowercase().contains(&query) || content.to_lowercase().contains(&query) {
                            hits.push(json!({ "sheet": self.sheet_name(sheet), "cell": cell_name(r, c), "value": value, "content": content }));
                            if hits.len() >= limit {
                                break 'sheets;
                            }
                        }
                    }
                }
                Ok(json!(hits))
            }
            "add_sheet" => {
                self.mcp_commit()?;
                self.book.model.new_sheet()?;
                let sheet = self.book.model.get_selected_sheet();
                if let Some(name) = args.get("name").and_then(Value::as_str) {
                    self.book.model.rename_sheet(sheet, name)?;
                }
                self.changed();
                self.sync_sheet();
                Ok(json!({ "sheet": self.sheet_name(sheet) }))
            }
            "rename_sheet" => {
                let sheet = self.mcp_sheet(args, None)?;
                self.mcp_commit()?;
                self.book.model.rename_sheet(sheet, str_arg(args, "name")?)?;
                self.changed();
                Ok(json!({ "sheet": self.sheet_name(sheet) }))
            }
            "select" => {
                let (sheet, [r1, c1, r2, c2]) = self.mcp_range(args, "range")?;
                self.mcp_commit()?;
                let m = &mut self.book.model;
                m.set_selected_sheet(sheet)?;
                m.set_selected_cell(r1, c1)?;
                m.set_selected_range(r1, c1, r2, c2)?;
                self.reveal_selection();
                Ok(json!({ "sheet": self.sheet_name(sheet), "selected": range_name([r1, c1, r2, c2]) }))
            }
            "undo" | "redo" => {
                self.mcp_commit()?;
                let m = &mut self.book.model;
                let able = if name == "undo" { m.can_undo() } else { m.can_redo() };
                if !able {
                    return Err(format!("nothing to {name}"));
                }
                if name == "undo" { m.undo()? } else { m.redo()? };
                self.changed();
                self.reveal_selection();
                Ok(json!("done"))
            }
            "save" => {
                self.mcp_commit()?;
                let path = match args.get("path").and_then(Value::as_str) {
                    Some(p) => std::path::PathBuf::from(p),
                    None if self.book.kind != FileKind::Untitled => self.book.path.clone().ok_or("the workbook has no file; give a `path`")?,
                    None => return Err("the workbook has no file yet; give a `path`".into()),
                };
                if !path.is_absolute() {
                    return Err("give an absolute `path`".into());
                }
                let note = self.book.save_to(&path)?;
                self.dirty = false;
                self.set_status(format!("saved {}", self.book.display_name()), false);
                Ok(json!({ "file": path, "note": note }))
            }
            "open" => {
                let path = std::path::PathBuf::from(str_arg(args, "path")?);
                if !path.is_absolute() {
                    return Err("give an absolute `path`".into());
                }
                let book = Book::open(&path)?;
                // Commit or not, the edit is kept: it goes into the recovery copy.
                self.commit_edit();
                self.end_edit();
                let recovered = if self.dirty { Some(crate::book::save_recovery(&self.book)?) } else { None };
                self.load(book);
                Ok(json!({ "opened": path, "recovered": recovered }))
            }
            other => Err(format!("unknown tool `{other}`")),
        }
    }

    fn sheet_name(&self, sheet: u32) -> String {
        self.book.model.get_worksheets_properties().get(sheet as usize).map(|p| p.name.clone()).unwrap_or_default()
    }

    /// The `context` answer: `workbook`, plus the values of the selection's
    /// top-left `CONTEXT_ROWS` x `CONTEXT_COLS` cells.
    fn mcp_context(&self) -> Value {
        let m = &self.book.model;
        let v = m.get_selected_view();
        let [r1, c1, r2, c2] = ops::normalized(v.range);
        let rows: Vec<String> = (r1..=r2.min(r1 + CONTEXT_ROWS - 1))
            .map(|r| {
                (c1..=c2.min(c1 + CONTEXT_COLS - 1))
                    .map(|c| m.get_formatted_cell_value(v.sheet, r, c).unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join("\t")
            })
            .collect();
        let shown = rows.iter().any(|r| !r.trim().is_empty()).then(|| {
            let mut s = format!("Selected cells ({}), tab-separated:\n", range_name([r1, c1, r2, c2]));
            s.push_str(&rows.join("\n"));
            s
        });
        context_from(&self.mcp_workbook(), shown).into_result()
    }

    fn mcp_workbook(&self) -> Value {
        let m = &self.book.model;
        let sheets: Vec<Value> = m
            .get_worksheets_properties()
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let i = i as u32;
                let (r, c) = grid::used_extent(m, i);
                json!({
                    "name": p.name,
                    "visible": p.state == "visible",
                    "used": range_name([1, 1, r, c]),
                    "frozen_rows": m.get_frozen_rows_count(i).unwrap_or(0),
                    "frozen_columns": m.get_frozen_columns_count(i).unwrap_or(0),
                })
            })
            .collect();
        let v = m.get_selected_view();
        json!({
            "file": self.book.path,
            "unsaved": self.dirty,
            "sheets": sheets,
            "selected": {
                "sheet": self.sheet_name(v.sheet),
                "cell": cell_name(v.row, v.column),
                "range": range_name(ops::normalized(v.range)),
            },
            "editing": self.editing.as_ref().map(|e| cell_name(e.row, e.col)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_names_the_file_the_sheet_and_the_selection() {
        let workbook = json!({
            "file": "/home/u/Finance/budget.xlsx",
            "unsaved": true,
            "sheets": [{ "name": "2026" }],
            "selected": { "sheet": "2026", "cell": "B4", "range": "B4:D9" },
            "editing": null,
        });
        let c = context_from(&workbook, Some("Selected cells (B4:D9):\n1\t2".into())).into_result();
        assert_eq!(c["document"]["title"], "budget.xlsx");
        assert_eq!(c["selection"], json!(["2026!B4:D9"]));
        assert_eq!(c["dirty"], true);
        assert_eq!(c["view"]["sheet"], "2026");
        assert!(c["summary"].as_str().unwrap().ends_with("1\t2"));

        let untitled = context_from(&json!({ "file": null, "unsaved": false, "selected": {} }), None).into_result();
        assert_eq!(untitled["document"]["title"], "Untitled workbook");
        assert!(untitled.get("summary").is_none());
    }

    #[test]
    fn every_tool_is_classed() {
        // An unclassed tool makes the popup ask before every call.
        assert!(server().unclassed().is_empty(), "classify these in server(): {:?}", server().unclassed());
    }

    #[test]
    fn values_are_typed_as_a_person_would() {
        assert_eq!(typed(&json!(12.5)), "12.5");
        assert_eq!(typed(&json!(true)), "TRUE");
        assert_eq!(typed(&json!(null)), "");
        assert_eq!(typed(&json!("=A1*2")), "=A1*2");
    }

    #[test]
    fn tsv_quotes_what_would_split_a_cell() {
        let t = as_tsv(&[vec!["a\tb".into(), "two\nlines".into()], vec!["say \"hi\"".into()]]).unwrap();
        let mut m = ironcalc::base::UserModel::new_empty("t", "en", "UTC", "en").unwrap();
        let area = Area { sheet: 0, row: 1, column: 1, width: 2, height: 2 };
        m.paste_csv_string(&area, &t).unwrap();
        assert_eq!(m.get_formatted_cell_value(0, 1, 1).unwrap(), "a\tb");
        assert_eq!(m.get_formatted_cell_value(0, 1, 2).unwrap(), "two\nlines");
        assert_eq!(m.get_formatted_cell_value(0, 2, 1).unwrap(), "say \"hi\"");
    }

    fn app() -> SpreadsheetApp {
        let (tx, _rx) = calloop::channel::channel();
        SpreadsheetApp::with_book(Book::untitled(), None, cce_ui::create_font_system(), tx.into())
    }

    fn call(app: &mut SpreadsheetApp, name: &str, args: Value) -> Result<Value, String> {
        app.mcp_dispatch(name, &args)
    }

    #[test]
    fn every_listed_tool_is_dispatched() {
        let mut a = app();
        for t in tools() {
            if let Err(e) = call(&mut a, &t.name, json!({})) {
                assert!(!e.starts_with("unknown tool"), "{} is listed but not dispatched", t.name);
            }
        }
        assert!(call(&mut a, "nope", json!({})).unwrap_err().starts_with("unknown tool"));
    }

    #[test]
    fn an_agent_builds_a_small_sheet() {
        let mut a = app();
        let w = call(&mut a, "write_cells", json!({ "start": "A1", "values": [["item", "qty", "price", "total"], ["pens", 3, 1.5, "=B2*C2"], ["ink", 2, 4, "=B3*C3"], [null, null, "sum", "=SUM(D2:D3)"]] })).unwrap();
        assert_eq!(w["written"], "A1:D4");
        assert!(a.dirty);
        let r = call(&mut a, "read_range", json!({ "range": "C2:D4", "formulas": true })).unwrap();
        assert_eq!(r["values"], json!([["1.5", "4.5"], ["4", "8"], ["sum", "12.5"]]));
        assert_eq!(r["formulas"][2][1], "=SUM(D2:D3)");
        // Whole columns are cut to the used range.
        assert_eq!(call(&mut a, "read_range", json!({ "range": "A:B" })).unwrap()["range"], "A1:B4");
        call(&mut a, "format_range", json!({ "range": "A1:D1", "bold": true, "fill_color": "#F2CC60", "borders": "bottom" })).unwrap();
        assert!(crate::format::active(&a.book.model, 0, 1, 2).bold);
        call(&mut a, "change_lines", json!({ "action": "insert", "lines": "rows", "at": 2 })).unwrap();
        assert_eq!(call(&mut a, "read_range", json!({ "range": "D5", "formulas": true })).unwrap()["formulas"][0][0], "=SUM(D3:D4)");
        call(&mut a, "change_lines", json!({ "action": "insert", "lines": "columns", "at": "A", "count": 2 })).unwrap();
        let hits = call(&mut a, "find", json!({ "query": "INK" })).unwrap();
        assert_eq!(hits[0]["cell"], "C4");
        call(&mut a, "undo", json!({})).unwrap();
        assert_eq!(call(&mut a, "find", json!({ "query": "ink" })).unwrap()[0]["cell"], "A4");
        call(&mut a, "clear_range", json!({ "range": "A2:D2" })).unwrap();
        call(&mut a, "select", json!({ "range": "B3:C4" })).unwrap();
        assert_eq!(call(&mut a, "workbook", json!({})).unwrap()["selected"]["range"], "B3:C4");
    }

    #[test]
    fn sheets_are_named_by_argument_or_prefix() {
        let mut a = app();
        call(&mut a, "add_sheet", json!({ "name": "Data" })).unwrap();
        call(&mut a, "write_cells", json!({ "start": "Data!B2", "values": [[7]] })).unwrap();
        assert_eq!(call(&mut a, "read_range", json!({ "range": "B2", "sheet": "data" })).unwrap()["values"][0][0], "7");
        call(&mut a, "rename_sheet", json!({ "sheet": "Data", "name": "Q3 numbers" })).unwrap();
        assert_eq!(call(&mut a, "read_range", json!({ "range": "'Q3 numbers'!B2" })).unwrap()["sheet"], "Q3 numbers");
        assert!(call(&mut a, "read_range", json!({ "range": "Nope!A1" })).unwrap_err().contains("no sheet"));
        let wb = call(&mut a, "workbook", json!({})).unwrap();
        assert_eq!(wb["sheets"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn saving_and_opening_go_through_files() {
        let dir = std::env::temp_dir().join(format!("cce-spreadsheet-mcp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent.xlsx");
        let mut a = app();
        assert!(call(&mut a, "save", json!({})).unwrap_err().contains("path"));
        assert!(call(&mut a, "save", json!({ "path": "relative.xlsx" })).is_err());
        call(&mut a, "write_cells", json!({ "start": "A1", "values": [["kept"]] })).unwrap();
        call(&mut a, "save", json!({ "path": path })).unwrap();
        assert!(!a.dirty);
        call(&mut a, "write_cells", json!({ "start": "A2", "values": [["unsaved"]] })).unwrap();
        let other = dir.join("other.csv");
        std::fs::write(&other, "x,y\n").unwrap();
        let o = call(&mut a, "open", json!({ "path": other })).unwrap();
        assert!(o["recovered"].is_string());
        assert_eq!(call(&mut a, "read_range", json!({ "range": "B1" })).unwrap()["values"][0][0], "y");
        call(&mut a, "open", json!({ "path": path })).unwrap();
        // A2 was never saved: the used range is A1 alone.
        assert_eq!(call(&mut a, "read_range", json!({ "range": "A1:A2" })).unwrap()["values"], json!([["kept"]]));
        if let Some(p) = o["recovered"].as_str() {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn every_tool_has_an_object_schema() {
        let tools = tools();
        assert!(tools.len() >= 14);
        for t in &tools {
            assert_eq!(t.input_schema["type"], "object", "{}", t.name);
        }
    }
}
