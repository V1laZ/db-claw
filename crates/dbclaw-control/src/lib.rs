//! Automation protocol for a running DB Claw app.
//!
//! The app (started with `DBCLAW_CONTROL=1` or `--control`) listens on a
//! local endpoint only the current user can open — a Unix socket on macOS /
//! Linux, a named pipe on Windows. Messages are JSON lines:
//!
//! ```text
//! → {"id": 1, "method": "open_table", "params": {"connection": "Shop", "name": "customers"}}
//! ← {"id": 1, "result": {...}}            or   {"id": 1, "error": "..."}
//! ```
//!
//! [`METHODS`] documents every method; the `dbclaw-ctl` binary exposes them
//! as a CLI and as MCP tools.

use std::io::{BufRead as _, BufReader, Read, Write};
use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[cfg(feature = "server")]
pub mod server;
#[cfg(windows)]
pub mod winsec;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Request {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Response {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok(id: u64, result: Value) -> Self {
        Self { id, result: Some(result), error: None }
    }

    pub fn err(id: u64, error: impl Into<String>) -> Self {
        Self { id, result: None, error: Some(error.into()) }
    }
}

/// Default endpoint on Windows: `$DBCLAW_CONTROL_SOCKET`, else the named
/// pipe `\\.\pipe\dbclaw-control-<user SID>` (one per user).
#[cfg(windows)]
pub fn default_socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("DBCLAW_CONTROL_SOCKET") {
        return PathBuf::from(p);
    }
    let user = winsec::current_user_sid().unwrap_or_else(|_| std::env::var("USERNAME").unwrap_or_else(|_| "user".into()));
    PathBuf::from(format!(r"\\.\pipe\dbclaw-control-{user}"))
}

/// Default socket: `$DBCLAW_CONTROL_SOCKET`, else `<config dir>/DB Claw/control.sock`
/// (honouring `$DBCLAW_CONFIG_DIR`), or `/tmp/dbclaw-$USER.sock` when that path
/// is too long for a Unix socket.
#[cfg(not(windows))]
pub fn default_socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("DBCLAW_CONTROL_SOCKET") {
        return PathBuf::from(p);
    }
    let dir = match std::env::var("DBCLAW_CONFIG_DIR") {
        Ok(d) => PathBuf::from(d),
        Err(_) => dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("DB Claw"),
    };
    let path = dir.join("control.sock");
    // Unix socket paths are limited to ~104 bytes; fall back to /tmp.
    if path.as_os_str().len() >= 100 {
        let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
        return PathBuf::from(format!("/tmp/dbclaw-{user}.sock"));
    }
    path
}

/// Blocking client for the control endpoint.
pub struct Client {
    reader: BufReader<Box<dyn Read + Send>>,
    writer: Box<dyn Write + Send>,
    next_id: u64,
}

fn not_running(path: &std::path::Path) -> String {
    format!(
        "cannot reach DB Claw at {} — start the app with DBCLAW_CONTROL=1 (or --control)",
        path.display()
    )
}

impl Client {
    #[cfg(unix)]
    pub fn connect(path: &std::path::Path) -> Result<Client> {
        let stream = std::os::unix::net::UnixStream::connect(path).with_context(|| not_running(path))?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(600)))?;
        let writer = stream.try_clone()?;
        Ok(Client { reader: BufReader::new(Box::new(stream)), writer: Box::new(writer), next_id: 1 })
    }

    #[cfg(windows)]
    pub fn connect(path: &std::path::Path) -> Result<Client> {
        const ERROR_PIPE_BUSY: i32 = 231;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let pipe = loop {
            match std::fs::OpenOptions::new().read(true).write(true).open(path) {
                Ok(f) => break f,
                // All instances are momentarily taken: the server creates a new one.
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => return Err(anyhow::Error::new(e).context(not_running(path))),
            }
        };
        // Refuse to talk to a pipe served by another user (pipe squatting).
        if !winsec::pipe_server_is_current_user(&pipe)? {
            bail!("refusing to use {}: it is not served by DB Claw running as the current user", path.display());
        }
        let writer = pipe.try_clone()?;
        Ok(Client { reader: BufReader::new(Box::new(pipe)), writer: Box::new(writer), next_id: 1 })
    }

    #[cfg(not(any(unix, windows)))]
    pub fn connect(_path: &std::path::Path) -> Result<Client> {
        bail!("the DB Claw control server is not supported on this platform")
    }

    pub fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let req = Request { id, method: method.to_string(), params };
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes())?;
        self.writer.flush()?;
        let mut buf = String::new();
        loop {
            buf.clear();
            if self.reader.read_line(&mut buf)? == 0 {
                bail!("DB Claw closed the connection");
            }
            let resp: Response = serde_json::from_str(&buf).context("invalid response")?;
            if resp.id != id {
                continue;
            }
            return match (resp.result, resp.error) {
                (_, Some(e)) => Err(anyhow!(e)),
                (Some(r), None) => Ok(r),
                (None, None) => Ok(Value::Null),
            };
        }
    }
}

/// Description of one method: used for CLI help and MCP tool definitions.
pub struct MethodSpec {
    pub name: &'static str,
    pub description: &'static str,
    /// JSON Schema properties: (name, type, description, required).
    pub params: &'static [(&'static str, &'static str, &'static str, bool)],
}

impl MethodSpec {
    pub fn input_schema(&self) -> Value {
        let mut props = serde_json::Map::new();
        let mut required = Vec::new();
        for (name, ty, desc, req) in self.params {
            let schema = match *ty {
                "any" => json!({ "description": desc }),
                "string|null" => json!({ "type": ["string", "null"], "description": desc }),
                "object" => json!({ "type": "object", "description": desc, "additionalProperties": true }),
                t => json!({ "type": t, "description": desc }),
            };
            props.insert(name.to_string(), schema);
            if *req {
                required.push(*name);
            }
        }
        json!({ "type": "object", "properties": props, "required": required })
    }
}

const TAB: (&str, &str, &str, bool) =
    ("tab", "integer", "Tab id (from `state`); defaults to the active tab", false);
const CONN: (&str, &str, &str, bool) =
    ("connection", "string", "Connection id or name (case-insensitive)", true);

pub static METHODS: &[MethodSpec] = &[
    MethodSpec {
        name: "state",
        description: "Snapshot of the whole app: saved connections and their status, open tabs (with filter, paging, row counts, pending changes, errors), the active tab and whether a dialog is open. Start here.",
        params: &[],
    },
    MethodSpec {
        name: "add_connection",
        description: "Save a new connection, given either a `url` or individual fields (`kind`, `host`, `port`, `user`, `database`, `options`). A password in the URL is stored separately. Connects right away unless connect=false.",
        params: &[
            ("url", "string", "postgres://user@host:5432/db or mongodb://user@host:27017/db?authSource=admin", false),
            ("kind", "string", "postgres or mongo (when not using `url`)", false),
            ("host", "string", "Host name (default localhost); Mongo replica sets: a:27017,b:27017", false),
            ("port", "integer", "Port (default 5432 / 27017)", false),
            ("user", "string", "User name", false),
            ("database", "string", "Database", false),
            ("options", "string", "Extra URL parameters, e.g. sslmode=require or authSource=admin", false),
            ("srv", "boolean", "Mongo: use mongodb+srv:// (DNS seed list)", false),
            ("name", "string", "Display name (derived from the URL if omitted)", false),
            ("password", "string", "Password (optional)", false),
            ("read_only", "boolean", "Disable editing in the data grid", false),
            ("connect", "boolean", "Connect right away (default true)", false),
        ],
    },
    MethodSpec {
        name: "remove_connection",
        description: "Delete a saved connection (the database is not touched). Closes its tabs.",
        params: &[CONN],
    },
    MethodSpec {
        name: "connect",
        description: "Connect to a saved connection and wait until it is connected or failed.",
        params: &[CONN],
    },
    MethodSpec {
        name: "disconnect",
        description: "Disconnect (closes the connection's tabs).",
        params: &[CONN],
    },
    MethodSpec {
        name: "list_databases",
        description: "List databases on a connected server.",
        params: &[CONN],
    },
    MethodSpec {
        name: "list_objects",
        description: "List tables/views (Postgres, with schema) or collections (Mongo) in a database.",
        params: &[CONN, ("database", "string", "Database name (defaults to the one in the URL)", false)],
    },
    MethodSpec {
        name: "open_table",
        description: "Open (or activate) the data tab of a table/view/collection and wait for the first page. Returns the tab with its first rows.",
        params: &[
            CONN,
            ("name", "string", "Table / collection name; `schema.table` also works", true),
            ("database", "string", "Database (defaults to the one in the URL)", false),
            ("schema", "string", "Postgres schema (default: public, or the only match)", false),
            ("rows", "integer", "How many rows to include in the reply (default 20)", false),
        ],
    },
    MethodSpec {
        name: "rows",
        description: "Read rows of a data or console tab as shown in the grid (pending edits applied, deleted rows flagged). Cells are strings, or null for NULL/missing.",
        params: &[TAB, ("offset", "integer", "First row on the current page (default 0)", false), ("limit", "integer", "Max rows (default 50)", false)],
    },
    MethodSpec {
        name: "filter",
        description: "Set the filter of a data tab using the universal filter language, e.g. `age > 30 and name ~ \"jo\" and tags in [\"a\"]`, and wait for results. Empty text clears it. Returns syntax errors with position.",
        params: &[TAB, ("text", "string", "Filter expression", true), ("rows", "integer", "Rows to include in the reply (default 20)", false)],
    },
    MethodSpec {
        name: "sort",
        description: "Sort a data tab by a column (server-side) and wait. Omit `column` to clear sorting.",
        params: &[TAB, ("column", "string", "Column name", false), ("dir", "string", "asc (default) or desc", false)],
    },
    MethodSpec {
        name: "page",
        description: "Go to a page and/or change the page size of a data tab and wait.",
        params: &[TAB, ("page", "integer", "0-based page index", false), ("page_size", "integer", "Rows per page (e.g. 100, 500, 1000, 5000)", false)],
    },
    MethodSpec {
        name: "refresh",
        description: "Reload the current page and row count of a data tab and wait.",
        params: &[TAB],
    },
    MethodSpec {
        name: "edit_cell",
        description: "Stage a cell edit (not saved until `save`). Postgres values are text in Postgres input format; Mongo values use relaxed JSON for non-strings (e.g. 42, true, ISODate(\"…\")). Use null for NULL.",
        params: &[
            TAB,
            ("row", "integer", "Row index on the current page (0-based)", true),
            ("column", "string", "Column / field name", true),
            ("value", "string|null", "New value, or null", true),
        ],
    },
    MethodSpec {
        name: "delete_row",
        description: "Stage deletion of a row (toggle with undo=true).",
        params: &[TAB, ("row", "integer", "Row index on the current page", true), ("undo", "boolean", "Unmark instead", false)],
    },
    MethodSpec {
        name: "save",
        description: "Save all staged edits/deletions of a data tab (one transaction on Postgres) and wait. Returns the summary or the error.",
        params: &[TAB],
    },
    MethodSpec {
        name: "discard",
        description: "Discard staged changes of a data tab.",
        params: &[TAB],
    },
    MethodSpec {
        name: "insert",
        description: "Insert immediately. Postgres: `values` maps column → text value (null = NULL; omitted columns use their default). Mongo: `document` in relaxed JSON / mongosh syntax.",
        params: &[TAB, ("values", "object", "Postgres column values", false), ("document", "string", "Mongo document", false)],
    },
    MethodSpec {
        name: "replace_document",
        description: "Mongo: replace the document at `row` with `document` (relaxed JSON; _id is kept) and wait.",
        params: &[TAB, ("row", "integer", "Row index on the current page", true), ("document", "string", "New document", true)],
    },
    MethodSpec {
        name: "follow_reference",
        description: "Postgres foreign keys: open the row referenced by `column` of `row` (outgoing), or the rows of another table that reference this row via an incoming `constraint` (names are listed in the tab's `foreign_keys`). Opens or reuses that table's tab, filtered, and waits for it.",
        params: &[
            TAB,
            ("row", "integer", "Row index on the current page", true),
            ("column", "string", "Foreign-key column to follow (outgoing)", false),
            ("constraint", "string", "Incoming foreign-key constraint name (referencing rows)", false),
            ("rows", "integer", "Rows to include in the reply (default 20)", false),
        ],
    },
    MethodSpec {
        name: "open_console",
        description: "Open a query console tab (SQL for Postgres, mongosh syntax for Mongo). Returns the tab.",
        params: &[
            CONN,
            ("database", "string", "Database (defaults to the one in the URL)", false),
            ("collection", "string", "Mongo: default collection for bare filters like {a: 1}", false),
        ],
    },
    MethodSpec {
        name: "run",
        description: "Run a query in a console tab and wait for the result. With `text`, the editor content is replaced and the whole text runs. Without it, runs the statement at the cursor (or everything with all=true). Returns result sets (columns + first rows), notices and errors.",
        params: &[
            TAB,
            ("text", "string", "Query text", false),
            ("all", "boolean", "Run the whole editor content", false),
            ("rows", "integer", "Rows per result set in the reply (default 50)", false),
            ("connection", "string", "If no console tab is given/active: open one on this connection", false),
            ("database", "string", "Database for a newly opened console", false),
        ],
    },
    MethodSpec {
        name: "cancel",
        description: "Cancel the running query of a console tab.",
        params: &[TAB],
    },
    MethodSpec {
        name: "activate_tab",
        description: "Switch to a tab.",
        params: &[("tab", "integer", "Tab id", true)],
    },
    MethodSpec {
        name: "close_tab",
        description: "Close a tab. Tabs with unsaved changes are kept unless force=true (changes are discarded).",
        params: &[TAB, ("force", "boolean", "Discard unsaved changes", false)],
    },
    MethodSpec {
        name: "key",
        description: "Low-level: send keystrokes to the focused element, space separated, e.g. \"cmd-t\" or \"down down enter\".",
        params: &[("keys", "string", "Keystrokes in gpui syntax (cmd-, shift-, alt-, ctrl- prefixes)", true)],
    },
    MethodSpec {
        name: "type",
        description: "Low-level: type text into the focused input.",
        params: &[("text", "string", "Text to type", true)],
    },
    MethodSpec {
        name: "action",
        description: "Low-level: dispatch a named action on the focused element, e.g. \"dbclaw::Refresh\", \"dbclaw::NewConsole\".",
        params: &[("name", "string", "Action name", true)],
    },
    MethodSpec {
        name: "click",
        description: "Low-level: click at window coordinates in points (state.window.width/height give the size; a screenshot may be scaled differently). Use count=2 for double-click; modifiers=shift / secondary select row ranges / extra rows in grids.",
        params: &[
            ("x", "number", "X in points from the window's left edge", true),
            ("y", "number", "Y in points from the top of the window content", true),
            ("button", "string", "left (default), right or middle", false),
            ("count", "integer", "Click count (2 = double-click)", false),
            ("modifiers", "string", "Keys held while clicking: shift, secondary (Cmd on macOS, Ctrl elsewhere), ctrl, alt, cmd; combine with +", false),
        ],
    },
    MethodSpec {
        name: "drag",
        description: "Low-level: press the left mouse button at (x1, y1), move to (x2, y2) and release, e.g. to reorder tabs or sidebar connections by drag and drop. Window coordinates in points.",
        params: &[
            ("x1", "number", "Start X in points", true),
            ("y1", "number", "Start Y in points", true),
            ("x2", "number", "End X in points", true),
            ("y2", "number", "End Y in points", true),
            ("hold_ms", "integer", "Wait this long before releasing (e.g. to screenshot the drop marker)", false),
        ],
    },
    MethodSpec {
        name: "close_dialog",
        description: "Close the open dialog (if any) and dismiss notifications.",
        params: &[],
    },
];

/// Handled by `dbclaw-ctl` itself (not the app): capture the app window.
pub static SCREENSHOT: MethodSpec = MethodSpec {
    name: "screenshot",
    description: "Capture an image of the DB Claw window. The app renders it itself, so it works when the window is covered and needs no screen-recording permission (macOS and Windows; not on Linux yet).",
    params: &[
        ("path", "string", "Where to save the image, .png or .jpg (CLI only; MCP returns the image)", false),
        ("max_width", "integer", "Downscale to at most this width in pixels (default 1600)", false),
    ],
};

pub fn find_method(name: &str) -> Option<&'static MethodSpec> {
    METHODS.iter().chain(std::iter::once(&SCREENSHOT)).find(|m| m.name == name)
}

/// Capture the window with the given macOS window number to `path`.
#[cfg(not(target_os = "macos"))]
pub fn capture_window(_window_number: u64, _path: &std::path::Path, _max_width: Option<u32>) -> Result<()> {
    bail!("screenshots are only supported on macOS for now")
}

/// Capture the window with the given macOS window number to `path`.
#[cfg(target_os = "macos")]
pub fn capture_window(window_number: u64, path: &std::path::Path, max_width: Option<u32>) -> Result<()> {
    let jpeg = path.extension().is_some_and(|e| e == "jpg" || e == "jpeg");
    let status = std::process::Command::new("/usr/sbin/screencapture")
        .args(["-x", "-o", "-t", if jpeg { "jpg" } else { "png" }, "-l", &window_number.to_string()])
        .arg(path)
        .status()
        .context("running screencapture")?;
    if !status.success() || !path.exists() {
        bail!("screencapture failed — grant Screen Recording permission to your terminal");
    }
    let mut sips_args: Vec<String> = Vec::new();
    if let Some(w) = max_width {
        sips_args.extend(["-Z".into(), w.to_string()]);
    }
    if jpeg {
        sips_args.extend(["-s", "format", "jpeg", "-s", "formatOptions", "low"].map(String::from));
    }
    if !sips_args.is_empty() {
        let _ = std::process::Command::new("/usr/bin/sips")
            .args(&sips_args)
            .arg(path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_are_objects() {
        for m in METHODS {
            let s = m.input_schema();
            assert_eq!(s["type"], "object", "{}", m.name);
        }
        assert!(find_method("screenshot").is_some());
        assert!(find_method("nope").is_none());
        let names: std::collections::HashSet<_> = METHODS.iter().map(|m| m.name).collect();
        assert_eq!(names.len(), METHODS.len(), "duplicate method names");
    }
}
