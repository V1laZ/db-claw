//! `dbclaw-ctl` — control a running DB Claw app from the shell or as an MCP server.
//!
//! ```text
//! dbclaw-ctl state
//! dbclaw-ctl open_table connection=Shop name=customers
//! dbclaw-ctl filter text='country = "CZ" and vip = true'
//! dbclaw-ctl run text='select count(*) from orders'
//! dbclaw-ctl call edit_cell '{"row": 0, "column": "name", "value": "Eva"}'
//! dbclaw-ctl screenshot /tmp/dbclaw.png
//! dbclaw-ctl mcp            # MCP server on stdio
//! ```

use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
#[cfg(target_os = "macos")]
use dbclaw_control::capture_window;
use dbclaw_control::{Client, METHODS, SCREENSHOT, default_socket_path, find_method};
use serde_json::{Map, Value, json};

const DEFAULT_SHOT_WIDTH: u32 = 1600;

fn usage() -> String {
    let mut s = String::from(
        "dbclaw-ctl — control a running DB Claw app (start it with DBCLAW_CONTROL=1)\n\n\
         USAGE:\n  dbclaw-ctl [--socket PATH] <method> [key=value ...]\n  \
         dbclaw-ctl [--socket PATH] call <method> '<json params>'\n  \
         dbclaw-ctl screenshot [PATH]\n  dbclaw-ctl mcp\n  dbclaw-ctl help <method>\n\n\
         Values in key=value are parsed as JSON when possible (42, true, null, {...}), else used as strings.\n\nMETHODS:\n",
    );
    for m in METHODS.iter().chain(std::iter::once(&SCREENSHOT)) {
        let first = m.description.split(". ").next().unwrap_or(m.description);
        s.push_str(&format!("  {:<18} {}\n", m.name, first.trim_end_matches('.')));
    }
    s
}

fn method_help(name: &str) -> Result<String> {
    let m = find_method(name).ok_or_else(|| anyhow!("unknown method {name}"))?;
    let mut s = format!("{}\n\n{}\n", m.name, m.description);
    if !m.params.is_empty() {
        s.push_str("\nPARAMS:\n");
        for (n, t, d, req) in m.params {
            s.push_str(&format!("  {n:<12} {t:<12} {}{d}\n", if *req { "(required) " } else { "" }));
        }
    }
    Ok(s)
}

fn parse_kv(args: &[String]) -> Result<Value> {
    let mut map = Map::new();
    for a in args {
        let (k, v) = a.split_once('=').ok_or_else(|| anyhow!("expected key=value, got `{a}`"))?;
        let value = serde_json::from_str::<Value>(v).unwrap_or_else(|_| Value::String(v.to_string()));
        map.insert(k.to_string(), value);
    }
    Ok(Value::Object(map))
}

/// Save a screenshot of the app window. The app renders its own window
/// (offscreen, so it works when covered and needs no permissions) and sends
/// the image over the control channel. On macOS, an app without that ability
/// is captured with `screencapture` instead.
fn screenshot(client: &mut Client, path: Option<PathBuf>, max_width: Option<u32>, ext: &str) -> Result<PathBuf> {
    let path = path.unwrap_or_else(|| {
        std::env::temp_dir().join(format!(
            "dbclaw-{}.{ext}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ))
    });
    let max_width = max_width.or(Some(DEFAULT_SHOT_WIDTH));
    let format = if matches!(ext, "jpg" | "jpeg") { "jpeg" } else { "png" };
    match client.call("capture", json!({ "max_width": max_width, "format": format })) {
        Ok(image) => {
            let data = image["data"].as_str().ok_or_else(|| anyhow!("the app sent no image"))?;
            let bytes = base64::engine::general_purpose::STANDARD.decode(data).context("decoding the image")?;
            std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
            Ok(path)
        }
        #[cfg(target_os = "macos")]
        Err(e) if e.to_string().contains("unknown method") => {
            let state = client.call("state", json!({}))?;
            std::thread::sleep(std::time::Duration::from_millis(60));
            let number = state["window"]["number"]
                .as_u64()
                .ok_or_else(|| anyhow!("the app did not report a window number"))?;
            capture_window(number, &path, max_width)?;
            Ok(path)
        }
        Err(e) => Err(e),
    }
}

fn main() {
    if let Err(e) = real_main() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut socket = default_socket_path();
    let mut explicit_socket = false;
    if args.first().map(String::as_str) == Some("--socket") {
        if args.len() < 2 {
            bail!("--socket needs a path");
        }
        socket = PathBuf::from(args.remove(1));
        args.remove(0);
        explicit_socket = true;
    }
    let Some(cmd) = args.first().cloned() else {
        print!("{}", usage());
        return Ok(());
    };
    // Inside WSL with the app running on Windows: forward to dbclaw-ctl.exe.
    #[cfg(target_os = "linux")]
    if !explicit_socket && !matches!(cmd.as_str(), "help" | "--help" | "-h") && wsl::should_hand_off(&socket) {
        if let Some(exe) = wsl::windows_ctl() {
            std::process::exit(wsl::run(&exe, &args)?);
        }
        if !socket.exists() {
            bail!(
                "no DB Claw is listening inside WSL ({}), and dbclaw-ctl.exe wasn't found to reach the Windows \
                 app. Put the folder with dbclaw-ctl.exe on your Windows PATH or set DBCLAW_CTL_EXE.",
                socket.display()
            );
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = explicit_socket;
    let rest = &args[1..];
    match cmd.as_str() {
        "help" | "--help" | "-h" => {
            match rest.first() {
                Some(m) => print!("{}", method_help(m)?),
                None => print!("{}", usage()),
            }
            Ok(())
        }
        "mcp" => mcp::serve(socket),
        "screenshot" => {
            let mut client = Client::connect(&socket)?;
            let target = rest.first().map(PathBuf::from);
            let ext = target
                .as_ref()
                .and_then(|p| p.extension())
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_else(|| "png".into());
            let path = screenshot(&mut client, target, None, &ext)?;
            println!("{}", path.display());
            Ok(())
        }
        "call" => {
            let method = rest.first().ok_or_else(|| anyhow!("call <method> [json]"))?;
            let params = match rest.get(1) {
                Some(j) => serde_json::from_str(j).context("params must be JSON")?,
                None => json!({}),
            };
            let mut client = Client::connect(&socket)?;
            let result = client.call(method, params)?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
        method => {
            if find_method(method).is_none() {
                bail!("unknown method `{method}` — run `dbclaw-ctl help`");
            }
            let params = parse_kv(rest)?;
            let mut client = Client::connect(&socket)?;
            let result = client.call(method, params)?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
    }
}

/// WSL → Windows hand-off. A Linux process in WSL2 can't open Windows named
/// pipes, but it can run Windows executables (WSL interop), whose stdio is
/// bridged. So when no DB Claw listens inside WSL, the Linux CLI runs
/// `dbclaw-ctl.exe` with the same arguments; it connects to the Windows app's
/// pipe as the Windows user. Set `DBCLAW_WSL_HANDOFF=0` to disable.
#[cfg(target_os = "linux")]
mod wsl {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use anyhow::{Context as _, Result};

    pub fn is_wsl() -> bool {
        std::env::var_os("WSL_DISTRO_NAME").is_some()
            || Path::new("/proc/sys/fs/binfmt_misc/WSLInterop").exists()
            || std::fs::read_to_string("/proc/sys/kernel/osrelease")
                .is_ok_and(|r| r.to_lowercase().contains("microsoft"))
    }

    /// In WSL, hand off unless an app inside WSL is listening.
    pub fn should_hand_off(socket: &Path) -> bool {
        if std::env::var("DBCLAW_WSL_HANDOFF").is_ok_and(|v| v == "0") || !is_wsl() {
            return false;
        }
        std::os::unix::net::UnixStream::connect(socket).is_err()
    }

    /// `$DBCLAW_CTL_EXE`, else `dbclaw-ctl.exe` on the PATH (WSL appends the
    /// Windows PATH by default).
    pub fn windows_ctl() -> Option<PathBuf> {
        if let Some(p) = std::env::var_os("DBCLAW_CTL_EXE").map(PathBuf::from) {
            return p.is_file().then_some(p);
        }
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join("dbclaw-ctl.exe"))
            .find(|p| p.is_file())
    }

    /// `/home/me/shot.png` → `\\wsl.localhost\Ubuntu\home\me\shot.png`
    fn to_windows_path(path: &str) -> Result<String> {
        let abs = std::path::absolute(path)?;
        let out = Command::new("wslpath").arg("-w").arg(&abs).output().context("running wslpath")?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    pub fn run(exe: &Path, args: &[String]) -> Result<i32> {
        let mut args = args.to_vec();
        // A screenshot path is written by the Windows process.
        if args.first().map(String::as_str) == Some("screenshot") && args.len() > 1 {
            args[1] = to_windows_path(&args[1])?;
        }
        let status = Command::new(exe)
            .args(&args)
            .status()
            .with_context(|| format!("running {}", exe.display()))?;
        Ok(status.code().unwrap_or(1))
    }
}

/// Minimal MCP (Model Context Protocol) server over stdio.
mod mcp {
    use super::*;

    const PROTOCOL_VERSION: &str = "2025-06-18";

    pub fn serve(socket: PathBuf) -> Result<()> {
        let stdin = std::io::stdin();
        let mut stdout = std::io::stdout();
        let mut client: Option<Client> = None;
        for line in stdin.lock().lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let msg: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(e) => {
                    write(&mut stdout, json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}}))?;
                    continue;
                }
            };
            let Some(id) = msg.get("id").cloned() else {
                continue; // notification
            };
            let method = msg["method"].as_str().unwrap_or_default();
            let reply = match method {
                "initialize" => {
                    let requested = msg["params"]["protocolVersion"].as_str().unwrap_or(PROTOCOL_VERSION);
                    json!({
                        "protocolVersion": requested,
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "dbclaw", "version": env!("CARGO_PKG_VERSION") },
                        "instructions": "Controls a running DB Claw database manager (PostgreSQL + MongoDB). Call `state` first. Data tabs support filter/sort/page/edit_cell/delete_row/save; console tabs support run. Edits are staged until `save`. Use `screenshot` to see the UI."
                    })
                }
                "ping" => json!({}),
                "tools/list" => {
                    let tools: Vec<Value> = METHODS
                        .iter()
                        .chain(std::iter::once(&SCREENSHOT))
                        .map(|m| json!({ "name": m.name, "description": m.description, "inputSchema": m.input_schema() }))
                        .collect();
                    json!({ "tools": tools })
                }
                "tools/call" => {
                    let name = msg["params"]["name"].as_str().unwrap_or_default().to_string();
                    let args = msg["params"].get("arguments").cloned().unwrap_or(json!({}));
                    call_tool(&socket, &mut client, &name, args)
                }
                other => {
                    write(&mut stdout, json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {other}")}}))?;
                    continue;
                }
            };
            write(&mut stdout, json!({"jsonrpc": "2.0", "id": id, "result": reply}))?;
        }
        Ok(())
    }

    fn call_tool(socket: &std::path::Path, client: &mut Option<Client>, name: &str, args: Value) -> Value {
        let result = (|| -> Result<Value> {
            if find_method(name).is_none() {
                bail!("unknown tool {name}");
            }
            // (Re)connect lazily so the app may be started after the MCP server.
            if client.is_none() {
                *client = Some(Client::connect(socket)?);
            }
            let c = client.as_mut().unwrap();
            if name == "screenshot" {
                let max = args["max_width"].as_u64().map(|w| w as u32);
                let path = screenshot(c, None, max, "jpg")?;
                let bytes = std::fs::read(&path)?;
                let _ = std::fs::remove_file(&path);
                return Ok(json!({
                    "content": [{ "type": "image", "mimeType": "image/jpeg",
                                  "data": base64::engine::general_purpose::STANDARD.encode(bytes) }]
                }));
            }
            let out = c.call(name, args)?;
            Ok(json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&out)? }] }))
        })();
        match result {
            Ok(v) => v,
            Err(e) => {
                // A dead socket: drop the client so the next call reconnects.
                if e.to_string().contains("closed the connection") || e.to_string().contains("Broken pipe") {
                    *client = None;
                }
                json!({ "content": [{ "type": "text", "text": format!("{e:#}") }], "isError": true })
            }
        }
    }

    fn write(out: &mut std::io::Stdout, v: Value) -> Result<()> {
        let mut s = serde_json::to_string(&v)?;
        s.push('\n');
        out.write_all(s.as_bytes())?;
        out.flush()?;
        Ok(())
    }
}
