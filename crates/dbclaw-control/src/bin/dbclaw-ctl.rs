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
use dbclaw_control::{Client, METHODS, SCREENSHOT, capture_window, default_socket_path, find_method};
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

fn screenshot(client: &mut Client, path: Option<PathBuf>, max_width: Option<u32>, ext: &str) -> Result<PathBuf> {
    let state = client.call("state", json!({}))?;
    // Give the app a frame to draw after the refresh triggered by `state`.
    std::thread::sleep(std::time::Duration::from_millis(60));
    let number = state["window"]["number"]
        .as_u64()
        .ok_or_else(|| anyhow!("the app did not report a window number"))?;
    let path = path.unwrap_or_else(|| {
        std::env::temp_dir().join(format!("dbclaw-{}.{ext}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)))
    });
    capture_window(number, &path, max_width.or(Some(DEFAULT_SHOT_WIDTH)))?;
    Ok(path)
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
    if args.first().map(String::as_str) == Some("--socket") {
        if args.len() < 2 {
            bail!("--socket needs a path");
        }
        socket = PathBuf::from(args.remove(1));
        args.remove(0);
    }
    let Some(cmd) = args.first().cloned() else {
        print!("{}", usage());
        return Ok(());
    };
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
            let path = screenshot(&mut client, rest.first().map(PathBuf::from), None, "png")?;
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
