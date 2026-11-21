//! Automation server: lets scripts and AI agents drive the running app
//! through `dbclaw-ctl` (CLI / MCP). See `dbclaw_control` for the protocol.
//!
//! Enabled with `DBCLAW_CONTROL=1` or `--control`. Requests arrive on the I/O
//! runtime and are executed on the UI thread, exactly like user actions.

use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use dbclaw_control::{Request, Response};
use dbclaw_core::model::{ConnectionConfig, DbKind, ObjectInfo, split_url_password};
use dbclaw_drivers::Connection;
use futures::StreamExt as _;
use futures::channel::{mpsc, oneshot};
use gpui_kit::component::WindowExt as _;
use gpui_kit::*;
use serde_json::{Value, json};

use crate::console::ConsoleView;
use crate::state::LiveStatus;
use crate::table_view::TableView;
use crate::util::io;
use crate::workspace::{Tab, Workspace};

type Job = (Request, oneshot::Sender<Response>);

pub fn enabled() -> bool {
    std::env::var("DBCLAW_CONTROL").is_ok_and(|v| v != "0" && !v.is_empty())
        || std::env::args().any(|a| a == "--control")
}

/// Start listening and serve requests against `workspace` in `window`.
pub fn start(workspace: Entity<Workspace>, window: AnyWindowHandle, cx: &mut App) {
    // Animations wait on display-link frames, which macOS pauses for covered
    // windows; agents also want deterministic, instantly settled UI.
    cx.set_reduce_motion(true);
    let (tx, mut rx) = mpsc::unbounded::<Job>();
    if let Err(e) = listen(tx) {
        eprintln!("db-claw: control server not started: {e:#}");
        return;
    }
    let _ = window.update(cx, |_, window, cx| {
        window
            .spawn(cx, async move |cx| {
                while let Some((req, reply)) = rx.next().await {
                    let ctx = Ctx { workspace: workspace.clone() };
                    cx.spawn(async move |cx| {
                        let id = req.id;
                        let resp = match ctx.handle(&req.method, req.params, cx).await {
                            Ok(v) => Response::ok(id, v),
                            Err(e) => Response::err(id, format!("{e:#}")),
                        };
                        // Redraw even if the window is covered, so screenshots are current.
                        let _ = cx.update(|window, _| window.refresh());
                        let _ = reply.send(resp);
                    })
                    .detach();
                }
            })
            .detach();
    });
}

#[cfg(unix)]
fn listen(tx: mpsc::UnboundedSender<Job>) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

    let path = dbclaw_control::default_socket_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if path.exists() {
        if std::os::unix::net::UnixStream::connect(&path).is_ok() {
            bail!("another DB Claw instance is already listening on {}", path.display());
        }
        std::fs::remove_file(&path)?;
    }
    let listener = {
        let _guard = dbclaw_drivers::runtime().enter();
        tokio::net::UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?
    };
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    eprintln!("db-claw: control socket at {}", path.display());

    dbclaw_drivers::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            let tx = tx.clone();
            tokio::spawn(async move {
                let (read, write) = stream.into_split();
                let write = std::sync::Arc::new(tokio::sync::Mutex::new(write));
                let mut lines = tokio::io::BufReader::new(read).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let write = write.clone();
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let resp = match serde_json::from_str::<Request>(&line) {
                            Ok(req) => {
                                let (otx, orx) = oneshot::channel();
                                let id = req.id;
                                if tx.unbounded_send((req, otx)).is_err() {
                                    Response::err(id, "app is shutting down")
                                } else {
                                    orx.await.unwrap_or_else(|_| Response::err(id, "request dropped"))
                                }
                            }
                            Err(e) => Response::err(0, format!("invalid request: {e}")),
                        };
                        let mut out = serde_json::to_string(&resp).unwrap_or_default();
                        out.push('\n');
                        let _ = write.lock().await.write_all(out.as_bytes()).await;
                    });
                }
            });
        }
    });
    Ok(())
}

#[cfg(not(unix))]
fn listen(_tx: mpsc::UnboundedSender<Job>) -> Result<()> {
    bail!("the control server is not available on this platform yet")
}

/// The native window number (macOS), used by `dbclaw-ctl screenshot`.
fn window_number(window: &Window) -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        use raw_window_handle::RawWindowHandle;
        let handle = raw_window_handle::HasWindowHandle::window_handle(window).ok()?;
        if let RawWindowHandle::AppKit(h) = handle.as_raw() {
            let view: &objc2_app_kit::NSView = unsafe { h.ns_view.cast().as_ref() };
            return view.window().map(|w| w.windowNumber() as u64);
        }
        None
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        None
    }
}

const WAIT: Duration = Duration::from_secs(120);

#[derive(Clone)]
struct Ctx {
    workspace: Entity<Workspace>,
}

fn p_str<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn req_str<'a>(params: &'a Value, key: &str) -> Result<&'a str> {
    p_str(params, key).ok_or_else(|| anyhow!("missing parameter `{key}`"))
}

fn p_u64(params: &Value, key: &str) -> Option<u64> {
    params.get(key).and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
}

fn p_bool(params: &Value, key: &str) -> Option<bool> {
    params.get(key).and_then(|v| v.as_bool().or_else(|| v.as_str().map(|s| s == "true")))
}

impl Ctx {
    fn ws<R>(
        &self,
        cx: &mut AsyncWindowContext,
        f: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) -> R,
    ) -> Result<R> {
        let ws = self.workspace.clone();
        cx.update(|window, cx| ws.update(cx, |w, cx| f(w, window, cx)))
    }

    async fn wait_until(
        &self,
        cx: &mut AsyncWindowContext,
        timeout: Duration,
        mut done: impl FnMut(&mut Window, &mut App) -> bool,
    ) -> Result<()> {
        let start = Instant::now();
        loop {
            if cx.update(|w, cx| done(w, cx))? {
                return Ok(());
            }
            if start.elapsed() > timeout {
                bail!("timed out after {:?}", timeout);
            }
            cx.background_executor().timer(Duration::from_millis(15)).await;
        }
    }

    /// Resolve a connection by id, exact name, or case-insensitive name/prefix.
    fn resolve_conn(&self, cx: &mut AsyncWindowContext, key: &str) -> Result<ConnectionConfig> {
        self.ws(cx, |ws, _, cx| {
            let conns = &ws.state().read(cx).connections;
            let k = key.to_lowercase();
            conns
                .iter()
                .find(|c| c.id == key || c.name == key)
                .or_else(|| conns.iter().find(|c| c.name.to_lowercase() == k))
                .or_else(|| {
                    let m: Vec<_> = conns.iter().filter(|c| c.name.to_lowercase().starts_with(&k)).collect();
                    (m.len() == 1).then(|| m[0])
                })
                .or_else(|| {
                    let m: Vec<_> = conns.iter().filter(|c| c.name.to_lowercase().contains(&k)).collect();
                    (m.len() == 1).then(|| m[0])
                })
                .cloned()
                .ok_or_else(|| {
                    let names: Vec<&str> = conns.iter().map(|c| c.name.as_str()).collect();
                    anyhow!("no connection `{key}`; saved: {}", names.join(", "))
                })
        })?
    }

    /// Connect if needed and return the live connection.
    async fn live(&self, cx: &mut AsyncWindowContext, key: &str) -> Result<(ConnectionConfig, Connection)> {
        let config = self.resolve_conn(cx, key)?;
        let id = config.id.clone();
        self.ws(cx, |ws, _, cx| {
            ws.state().update(cx, |s, cx| {
                if matches!(s.status(&id), Some(LiveStatus::Failed(_))) {
                    s.live.remove(&id);
                }
                s.connect(&id, cx);
            })
        })?;
        let ws = self.workspace.clone();
        let id2 = id.clone();
        self.wait_until(cx, Duration::from_secs(60), move |_, cx| {
            !matches!(ws.read(cx).state().read(cx).status(&id2), Some(LiveStatus::Connecting))
        })
        .await?;
        let status = self.ws(cx, |ws, _, cx| ws.state().read(cx).status(&id).cloned())?;
        match status {
            Some(LiveStatus::Connected(c)) => Ok((config, c)),
            Some(LiveStatus::Failed(e)) => bail!("connection failed: {e}"),
            _ => bail!("not connected"),
        }
    }

    fn tab(&self, cx: &mut AsyncWindowContext, params: &Value) -> Result<Tab> {
        let id = p_u64(params, "tab");
        self.ws(cx, |ws, _, _| {
            let ix = match id {
                Some(id) => ws.tab_index(id).ok_or_else(|| anyhow!("no tab with id {id}"))?,
                None if ws.tabs().is_empty() => bail!("no tabs are open"),
                None => ws.active_index(),
            };
            Ok(ws.tabs()[ix].clone())
        })?
    }

    fn table(&self, cx: &mut AsyncWindowContext, params: &Value) -> Result<Entity<TableView>> {
        match self.tab(cx, params)? {
            Tab::Table(t) => Ok(t),
            Tab::Console(_) => bail!("that tab is a console, not a data tab"),
        }
    }

    async fn wait_table(&self, cx: &mut AsyncWindowContext, t: &Entity<TableView>) -> Result<()> {
        let t = t.clone();
        self.wait_until(cx, WAIT, move |_, cx| !t.read(cx).is_busy()).await
    }

    fn table_json(&self, cx: &mut AsyncWindowContext, t: &Entity<TableView>, rows: usize) -> Result<Value> {
        let id = t.entity_id().as_u64();
        cx.update(|_, cx| {
            let mut v = t.read(cx).snapshot(0, rows, cx);
            v["tab"] = json!(id);
            v
        })
    }

    async fn wait_save(&self, cx: &mut AsyncWindowContext, t: &Entity<TableView>, seq: u64) -> Result<Value> {
        let t2 = t.clone();
        self.wait_until(cx, WAIT, move |_, cx| {
            t2.read(cx).last_save().is_some_and(|(s, _)| *s >= seq)
        })
        .await?;
        let result = cx.update(|_, cx| t.read(cx).last_save().map(|(_, r)| r.clone()))?;
        match result {
            Some(Ok(summary)) => {
                self.wait_table(cx, t).await?;
                Ok(json!({ "saved": summary }))
            }
            Some(Err(e)) => bail!("save failed: {e}"),
            None => bail!("save did not finish"),
        }
    }

    async fn handle(&self, method: &str, mut params: Value, cx: &mut AsyncWindowContext) -> Result<Value> {
        coerce_params(method, &mut params);
        match method {
            "state" => self.state(cx),
            "add_connection" => {
                let raw_url = req_str(&params, "url")?;
                let (url, url_pw) = split_url_password(raw_url);
                let kind = DbKind::from_url(&url)
                    .ok_or_else(|| anyhow!("URL must start with postgres:// or mongodb://"))?;
                let name = p_str(&params, "name")
                    .map(str::to_string)
                    .unwrap_or_else(|| crate::connection_dialog::suggest_name(&url));
                let config = ConnectionConfig {
                    id: uuid::Uuid::new_v4().to_string(),
                    name,
                    kind,
                    url,
                    password: p_str(&params, "password").map(str::to_string).or(url_pw),
                    read_only: p_bool(&params, "read_only").unwrap_or(false),
                };
                let id = config.id.clone();
                self.ws(cx, |ws, _, cx| ws.state().update(cx, |s, cx| s.upsert(config, cx)))?;
                if p_bool(&params, "connect").unwrap_or(true) {
                    if let Err(e) = self.live(cx, &id).await {
                        return Ok(json!({ "id": id, "status": "failed", "error": format!("{e:#}") }));
                    }
                    return Ok(json!({ "id": id, "status": "connected" }));
                }
                Ok(json!({ "id": id, "status": "disconnected" }))
            }
            "remove_connection" => {
                let c = self.resolve_conn(cx, req_str(&params, "connection")?)?;
                self.ws(cx, |ws, _, cx| ws.state().update(cx, |s, cx| s.remove(&c.id, cx)))?;
                Ok(json!({ "removed": c.name }))
            }
            "connect" => {
                let (config, conn) = self.live(cx, req_str(&params, "connection")?).await?;
                Ok(json!({ "id": config.id, "status": "connected", "server": conn.server_version(), "default_database": conn.default_database() }))
            }
            "disconnect" => {
                let c = self.resolve_conn(cx, req_str(&params, "connection")?)?;
                self.ws(cx, |ws, _, cx| ws.state().update(cx, |s, cx| s.disconnect(&c.id, cx)))?;
                Ok(json!({ "status": "disconnected" }))
            }
            "list_databases" => {
                let (_, conn) = self.live(cx, req_str(&params, "connection")?).await?;
                let dbs = io(async move { conn.list_databases().await }).await?;
                Ok(json!(dbs))
            }
            "list_objects" => {
                let (_, conn) = self.live(cx, req_str(&params, "connection")?).await?;
                let db = p_str(&params, "database").map(str::to_string).or_else(|| conn.default_database());
                let db = db.ok_or_else(|| anyhow!("pass `database`"))?;
                let objs = io(async move { conn.list_objects(&db).await }).await?;
                Ok(json!(objs.iter().map(object_json).collect::<Vec<_>>()))
            }
            "open_table" => self.open_table(&params, cx).await,
            "rows" => {
                let tab = self.tab(cx, &params)?;
                let offset = p_u64(&params, "offset").unwrap_or(0) as usize;
                let limit = p_u64(&params, "limit").unwrap_or(50) as usize;
                match tab {
                    Tab::Table(t) => {
                        self.wait_table(cx, &t).await?;
                        cx.update(|_, cx| t.read(cx).snapshot(offset, limit, cx)["grid"].clone())
                    }
                    Tab::Console(c) => cx.update(|_, cx| c.read(cx).snapshot(offset + limit, cx)["result_sets"].clone()),
                }
            }
            "filter" => {
                let t = self.table(cx, &params)?;
                let text = params.get("text").and_then(Value::as_str).unwrap_or("").to_string();
                self.wait_table(cx, &t).await?;
                cx.update(|window, cx| t.update(cx, |t, cx| t.set_filter_text(&text, window, cx)))?;
                if let Some(e) = cx.update(|_, cx| t.read(cx).filter_error())? {
                    bail!("filter error: {e}");
                }
                self.wait_table(cx, &t).await?;
                self.table_json(cx, &t, p_u64(&params, "rows").unwrap_or(20) as usize)
            }
            "sort" => {
                let t = self.table(cx, &params)?;
                let column = p_str(&params, "column").map(str::to_string);
                let desc = p_str(&params, "dir").is_some_and(|d| d.eq_ignore_ascii_case("desc"));
                cx.update(|window, cx| t.update(cx, |t, cx| t.sort_by(column, desc, window, cx)))?
                    .map_err(|e| anyhow!(e))?;
                self.wait_table(cx, &t).await?;
                self.table_json(cx, &t, 20)
            }
            "page" => {
                let t = self.table(cx, &params)?;
                let (page, size) = (p_u64(&params, "page"), p_u64(&params, "page_size"));
                cx.update(|_, cx| t.update(cx, |t, cx| t.set_paging(page, size, cx)))?;
                self.wait_table(cx, &t).await?;
                self.table_json(cx, &t, 20)
            }
            "refresh" => {
                let t = self.table(cx, &params)?;
                cx.update(|_, cx| t.update(cx, |t, cx| t.reload(true, cx)))?;
                self.wait_table(cx, &t).await?;
                self.table_json(cx, &t, 20)
            }
            "edit_cell" => {
                let t = self.table(cx, &params)?;
                let row = p_u64(&params, "row").ok_or_else(|| anyhow!("missing `row`"))? as usize;
                let column = req_str(&params, "column")?.to_string();
                let value = match params.get("value") {
                    None => bail!("missing `value` (use null for NULL)"),
                    Some(Value::Null) => None,
                    Some(Value::String(s)) => Some(s.clone()),
                    Some(other) => Some(other.to_string()),
                };
                self.wait_table(cx, &t).await?;
                cx.update(|window, cx| t.update(cx, |t, cx| t.stage_edit(row, &column, value.as_deref(), window, cx)))?
                    .map_err(|e| anyhow!(e))?;
                let rowv = cx.update(|_, cx| t.read(cx).snapshot(row, 1, cx))?;
                Ok(json!({ "pending_changes": rowv["pending_changes"], "row": rowv["grid"]["rows"][0] }))
            }
            "delete_row" => {
                let t = self.table(cx, &params)?;
                let row = p_u64(&params, "row").ok_or_else(|| anyhow!("missing `row`"))? as usize;
                let undo = p_bool(&params, "undo").unwrap_or(false);
                cx.update(|_, cx| t.update(cx, |t, cx| t.mark_deleted(row, !undo, cx)))?
                    .map_err(|e| anyhow!(e))?;
                let v = cx.update(|_, cx| t.read(cx).snapshot(0, 0, cx))?;
                Ok(json!({ "pending_changes": v["pending_changes"] }))
            }
            "save" => {
                let t = self.table(cx, &params)?;
                self.wait_table(cx, &t).await?;
                let seq = cx.update(|window, cx| t.update(cx, |t, cx| t.start_save(window, cx)))?
                    .map_err(|e| anyhow!(e))?;
                match seq {
                    None => Ok(json!({ "saved": "nothing to save" })),
                    Some(seq) => self.wait_save(cx, &t, seq).await,
                }
            }
            "discard" => {
                let t = self.table(cx, &params)?;
                cx.update(|window, cx| t.update(cx, |t, cx| t.discard(window, cx)))?;
                Ok(json!({ "discarded": true }))
            }
            "insert" => {
                let t = self.table(cx, &params)?;
                self.wait_table(cx, &t).await?;
                let values = params.get("values").and_then(Value::as_object).cloned();
                let document = p_str(&params, "document").map(str::to_string);
                let seq = cx
                    .update(|window, cx| t.update(cx, |t, cx| t.start_insert(values.as_ref(), document.as_deref(), window, cx)))?
                    .map_err(|e| anyhow!(e))?;
                self.wait_save(cx, &t, seq).await
            }
            "replace_document" => {
                let t = self.table(cx, &params)?;
                self.wait_table(cx, &t).await?;
                let row = p_u64(&params, "row").ok_or_else(|| anyhow!("missing `row`"))? as usize;
                let doc = req_str(&params, "document")?.to_string();
                let seq = cx
                    .update(|window, cx| t.update(cx, |t, cx| t.start_replace_document(row, &doc, window, cx)))?
                    .map_err(|e| anyhow!(e))?;
                self.wait_save(cx, &t, seq).await
            }
            "open_console" => {
                let (config, conn) = self.live(cx, req_str(&params, "connection")?).await?;
                let c = self.open_console(cx, &config, &conn, &params)?;
                let id = c.entity_id().as_u64();
                let mut v = cx.update(|_, cx| c.read(cx).snapshot(0, cx))?;
                v["tab"] = json!(id);
                Ok(v)
            }
            "run" => self.run(&params, cx).await,
            "cancel" => match self.tab(cx, &params)? {
                Tab::Console(c) => {
                    cx.update(|_, cx| c.update(cx, |c, cx| c.cancel(cx)))?;
                    Ok(json!({ "cancelled": true }))
                }
                _ => bail!("that tab is not a console"),
            },
            "activate_tab" => {
                let id = p_u64(&params, "tab").ok_or_else(|| anyhow!("missing `tab`"))?;
                self.ws(cx, |ws, window, cx| match ws.tab_index(id) {
                    Some(ix) => {
                        ws.activate(ix, window, cx);
                        Ok(json!({ "active_tab": id }))
                    }
                    None => Err(anyhow!("no tab with id {id}")),
                })?
            }
            "close_tab" => {
                let tab = self.tab(cx, &params)?;
                let force = p_bool(&params, "force").unwrap_or(false);
                let id = Workspace::tab_id(&tab);
                if let Tab::Table(t) = &tab {
                    let dirty = cx.update(|_, cx| t.read(cx).has_changes(cx))?;
                    if dirty && !force {
                        bail!("tab has unsaved changes; save, discard or pass force=true");
                    }
                    if dirty {
                        cx.update(|window, cx| t.update(cx, |t, cx| t.discard(window, cx)))?;
                    }
                }
                self.ws(cx, |ws, window, cx| ws.remove_tab(tab_entity_id(&tab), window, cx))?;
                Ok(json!({ "closed": id }))
            }
            "key" => {
                let keys = req_str(&params, "keys")?.to_string();
                cx.update(move |window, cx| -> Result<()> {
                    let os = cx.platform_info().os;
                    for k in keys.split_whitespace() {
                        let ks = Keystroke::parse_with_os(k, os)
                            .map_err(|e| anyhow!("bad keystroke `{}`", e.keystroke))?;
                        window.dispatch_keystroke(ks, cx);
                    }
                    Ok(())
                })??;
                Ok(json!({ "ok": true }))
            }
            "type" => {
                let text = req_str(&params, "text")?.to_string();
                cx.update(|window, cx| {
                    for ch in text.chars() {
                        let s = ch.to_string();
                        let ks = Keystroke { modifiers: Modifiers::default(), key: s.clone(), key_char: Some(s) };
                        window.dispatch_keystroke(ks, cx);
                    }
                })?;
                Ok(json!({ "ok": true }))
            }
            "action" => {
                let name = req_str(&params, "name")?.to_string();
                cx.update(|window, cx| -> Result<()> {
                    let action = cx.build_action(&name, None).map_err(|e| anyhow!("{e}"))?;
                    window.dispatch_action(action, cx);
                    Ok(())
                })??;
                Ok(json!({ "ok": true }))
            }
            "click" => {
                let x = params.get("x").and_then(Value::as_f64).ok_or_else(|| anyhow!("missing `x`"))?;
                let y = params.get("y").and_then(Value::as_f64).ok_or_else(|| anyhow!("missing `y`"))?;
                let button = match p_str(&params, "button") {
                    Some("right") => MouseButton::Right,
                    Some("middle") => MouseButton::Middle,
                    _ => MouseButton::Left,
                };
                let count = p_u64(&params, "count").unwrap_or(1).clamp(1, 3) as usize;
                let position = point(px(x as f32), px(y as f32));
                cx.update(|window, cx| {
                    window.dispatch_event(
                        PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers: Modifiers::default() }),
                        cx,
                    );
                    for n in 1..=count {
                        window.dispatch_event(
                            PlatformInput::MouseDown(MouseDownEvent { button, position, modifiers: Modifiers::default(), click_count: n, first_mouse: false }),
                            cx,
                        );
                        window.dispatch_event(
                            PlatformInput::MouseUp(MouseUpEvent { button, position, modifiers: Modifiers::default(), click_count: n }),
                            cx,
                        );
                    }
                })?;
                Ok(json!({ "ok": true }))
            }
            "close_dialog" => {
                cx.update(|window, cx| {
                    window.close_dialog(cx);
                    window.clear_notifications(cx);
                })?;
                Ok(json!({ "ok": true }))
            }
            other => bail!("unknown method `{other}`"),
        }
    }

    fn state(&self, cx: &mut AsyncWindowContext) -> Result<Value> {
        cx.update(|window, cx| {
            let number = window_number(window);
            let size = window.viewport_size();
            let dialog = window.has_active_dialog(cx);
            let ws = self.workspace.read(cx);
            let state = ws.state().read(cx);
            let connections: Vec<Value> = state
                .connections
                .iter()
                .map(|c| {
                    let (status, error, server) = match state.status(&c.id) {
                        None => ("disconnected", None, None),
                        Some(LiveStatus::Connecting) => ("connecting", None, None),
                        Some(LiveStatus::Connected(conn)) => ("connected", None, Some(conn.server_version())),
                        Some(LiveStatus::Failed(e)) => ("failed", Some(e.to_string()), None),
                    };
                    json!({
                        "id": c.id, "name": c.name, "kind": c.kind, "url": c.url,
                        "read_only": c.read_only, "status": status, "error": error, "server": server,
                    })
                })
                .collect();
            let active = ws.tabs().get(ws.active_index()).map(Workspace::tab_id);
            let tabs: Vec<Value> = ws
                .tabs()
                .iter()
                .map(|t| {
                    let mut v = match t {
                        Tab::Table(t) => {
                            let mut v = t.read(cx).snapshot(0, 0, cx);
                            let cols: Vec<Value> = v["grid"]["columns"]
                                .as_array()
                                .map(|a| a.iter().map(|c| c["name"].clone()).collect())
                                .unwrap_or_default();
                            v["columns"] = json!(cols);
                            v.as_object_mut().map(|o| o.remove("grid"));
                            v
                        }
                        Tab::Console(c) => {
                            let mut v = c.read(cx).snapshot(0, cx);
                            let summary: Vec<Value> = v["result_sets"]
                                .as_array()
                                .map(|a| a.iter().map(|s| json!({"status": s["status"], "row_count": s["row_count"]})).collect())
                                .unwrap_or_default();
                            v["result_sets"] = json!(summary);
                            v
                        }
                    };
                    v["tab"] = json!(Workspace::tab_id(t));
                    v["active"] = json!(Some(Workspace::tab_id(t)) == active);
                    v
                })
                .collect();
            json!({
                "app": { "name": "DB Claw", "version": env!("CARGO_PKG_VERSION"), "pid": std::process::id() },
                "window": { "number": number, "width": f32::from(size.width), "height": f32::from(size.height) },
                "dialog_open": dialog,
                "connections": connections,
                "tabs": tabs,
                "active_tab": active,
            })
        })
    }

    async fn open_table(&self, params: &Value, cx: &mut AsyncWindowContext) -> Result<Value> {
        let (config, conn) = self.live(cx, req_str(params, "connection")?).await?;
        let mut name = req_str(params, "name")?.to_string();
        let mut schema = p_str(params, "schema").map(str::to_string);
        if schema.is_none() && conn.kind() == DbKind::Postgres
            && let Some((s, n)) = name.clone().split_once('.') {
                schema = Some(s.to_string());
                name = n.to_string();
            }
        let db = p_str(params, "database")
            .map(str::to_string)
            .or_else(|| conn.default_database())
            .ok_or_else(|| anyhow!("pass `database`"))?;
        let c2 = conn.clone();
        let db2 = db.clone();
        let objects = io(async move { c2.list_objects(&db2).await }).await?;
        let matches: Vec<&ObjectInfo> = objects
            .iter()
            .filter(|o| o.object.name == name || o.object.name.eq_ignore_ascii_case(&name))
            .filter(|o| schema.as_ref().is_none_or(|s| &o.object.schema == s))
            .collect();
        let info = match matches.as_slice() {
            [one] => *one,
            [] => bail!("no table or collection `{name}` in database `{db}`"),
            many => many
                .iter()
                .find(|o| o.object.schema == "public")
                .copied()
                .ok_or_else(|| {
                    let s: Vec<String> = many.iter().map(|o| o.object.display_name()).collect();
                    anyhow!("ambiguous `{name}`: {} — pass `schema`", s.join(", "))
                })?,
        };
        let object = info.object.clone();
        let conn_id = config.id.clone();
        let t = self.ws(cx, |ws, window, cx| {
            ws.open_object(&conn_id, &object, window, cx);
            ws.find_table(&conn_id, &object, cx)
        })?
        .ok_or_else(|| anyhow!("could not open the tab"))?;
        self.wait_table(cx, &t).await?;
        self.table_json(cx, &t, p_u64(params, "rows").unwrap_or(20) as usize)
    }

    fn open_console(
        &self,
        cx: &mut AsyncWindowContext,
        config: &ConnectionConfig,
        conn: &Connection,
        params: &Value,
    ) -> Result<Entity<ConsoleView>> {
        let db = p_str(params, "database")
            .map(str::to_string)
            .or_else(|| conn.default_database())
            .unwrap_or_default();
        let coll = p_str(params, "collection").map(str::to_string);
        let id = config.id.clone();
        self.ws(cx, |ws, window, cx| {
            ws.open_console(&id, db, coll, window, cx);
            ws.last_console()
        })?
        .ok_or_else(|| anyhow!("could not open a console"))
    }

    async fn run(&self, params: &Value, cx: &mut AsyncWindowContext) -> Result<Value> {
        let console = match (p_u64(params, "tab"), p_str(params, "connection")) {
            (Some(_), _) => match self.tab(cx, params)? {
                Tab::Console(c) => c,
                Tab::Table(_) => bail!("that tab is a data tab; open a console with open_console"),
            },
            (None, conn_key) => {
                let active = self.ws(cx, |ws, _, _| ws.tabs().get(ws.active_index()).cloned())?;
                match (active, conn_key) {
                    (Some(Tab::Console(c)), None) => c,
                    (_, Some(key)) => {
                        let (config, conn) = self.live(cx, key).await?;
                        self.open_console(cx, &config, &conn, params)?
                    }
                    _ => bail!("no console tab is active — pass `tab` or `connection`"),
                }
            }
        };
        let text = p_str(params, "text").map(str::to_string);
        let all = p_bool(params, "all").unwrap_or(false);
        let run_id = cx
            .update(|window, cx| console.update(cx, |c, cx| c.start_run(text.as_deref(), all, window, cx)))?
            .map_err(|e| anyhow!(e))?;
        let c2 = console.clone();
        self.wait_until(cx, Duration::from_secs(600), move |_, cx| {
            let c = c2.read(cx);
            !c.is_running() || c.run_id() != run_id
        })
        .await?;
        let rows = p_u64(params, "rows").unwrap_or(50) as usize;
        let id = console.entity_id().as_u64();
        let mut v = cx.update(|_, cx| console.read(cx).snapshot(rows, cx))?;
        v["tab"] = json!(id);
        v.as_object_mut().map(|o| o.remove("text"));
        Ok(v)
    }
}

fn tab_entity_id(tab: &Tab) -> EntityId {
    match tab {
        Tab::Table(t) => t.entity_id(),
        Tab::Console(c) => c.entity_id(),
    }
}

fn object_json(o: &ObjectInfo) -> Value {
    json!({
        "database": o.object.database,
        "schema": o.object.schema,
        "name": o.object.name,
        "kind": format!("{:?}", o.object.kind),
        "estimated_rows": o.estimated_rows,
    })
}

/// CLI users pass `key=value`, where numbers parse as JSON numbers; turn
/// scalars back into strings for parameters documented as strings.
fn coerce_params(method: &str, params: &mut Value) {
    let Some(spec) = dbclaw_control::find_method(method) else { return };
    let Some(obj) = params.as_object_mut() else { return };
    for (name, ty, _, _) in spec.params {
        if !ty.starts_with("string") {
            continue;
        }
        if let Some(v) = obj.get_mut(*name)
            && (v.is_number() || v.is_boolean()) {
                *v = Value::String(v.to_string());
            }
    }
}
