//! The app side of the control channel: accept connections on the platform's
//! local IPC endpoint and answer JSON-line requests with a handler.
//!
//! - macOS / Linux: a Unix socket with `0600` permissions.
//! - Windows: a named pipe whose DACL allows only the current user, that
//!   rejects remote clients, and that refuses to start if another process
//!   already owns the name.
//!
//! [`serve`] must be called inside a tokio runtime; it binds synchronously
//! (so errors surface immediately) and accepts in a spawned task.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use tokio::io::{AsyncBufReadExt as _, AsyncRead, AsyncWrite, AsyncWriteExt as _};

use crate::{Request, Response};

/// Answers one request.
pub type Handler = Arc<dyn Fn(Request) -> Pin<Box<dyn Future<Output = Response> + Send>> + Send + Sync>;

/// Serve one connection: requests are handled concurrently, responses are
/// written as they complete (matched by `id`).
async fn serve_connection<S>(stream: S, handler: Handler)
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (read, write) = tokio::io::split(stream);
    let write = Arc::new(tokio::sync::Mutex::new(write));
    let mut lines = tokio::io::BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let (write, handler) = (write.clone(), handler.clone());
        tokio::spawn(async move {
            let resp = match serde_json::from_str::<Request>(&line) {
                Ok(req) => handler(req).await,
                Err(e) => Response::err(0, format!("invalid request: {e}")),
            };
            let mut out = serde_json::to_string(&resp).unwrap_or_default();
            out.push('\n');
            let _ = write.lock().await.write_all(out.as_bytes()).await;
        });
    }
}

/// Start serving on `endpoint` (see [`crate::default_socket_path`]).
#[cfg(unix)]
pub fn serve(endpoint: &Path, handler: Handler) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    use anyhow::{Context as _, bail};

    if let Some(dir) = endpoint.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if endpoint.exists() {
        if std::os::unix::net::UnixStream::connect(endpoint).is_ok() {
            bail!("another DB Claw instance is already listening on {}", endpoint.display());
        }
        std::fs::remove_file(endpoint)?;
    }
    let listener =
        tokio::net::UnixListener::bind(endpoint).with_context(|| format!("binding {}", endpoint.display()))?;
    std::fs::set_permissions(endpoint, std::fs::Permissions::from_mode(0o600))?;
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            tokio::spawn(serve_connection(stream, handler.clone()));
        }
    });
    Ok(())
}

/// Start serving on `endpoint` (see [`crate::default_socket_path`]).
#[cfg(windows)]
pub fn serve(endpoint: &Path, handler: Handler) -> Result<()> {
    use anyhow::anyhow;
    use tokio::net::windows::named_pipe::{NamedPipeServer, PipeMode, ServerOptions};

    use crate::winsec::OwnerOnly;

    let name = endpoint.to_string_lossy().to_string();
    let security = Arc::new(OwnerOnly::new()?);

    fn create(name: &str, first: bool, security: &OwnerOnly) -> std::io::Result<NamedPipeServer> {
        let mut attrs = security.attributes();
        unsafe {
            ServerOptions::new()
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .pipe_mode(PipeMode::Byte)
                .create_with_security_attributes_raw(name, &mut attrs as *mut _ as *mut std::ffi::c_void)
        }
    }

    // The first instance must be ours: if the name exists, someone else
    // (another DB Claw, or a squatter) owns it.
    let first = create(&name, true, &security).map_err(|e| {
        anyhow!("cannot create the control pipe {name}: {e} (is another DB Claw instance running?)")
    })?;
    tokio::spawn(async move {
        let mut server = first;
        loop {
            if server.connect().await.is_err() {
                // The client went away before we accepted; reuse the instance.
                let _ = server.disconnect();
                continue;
            }
            // Have the next instance ready before handing this one off.
            let next = match create(&name, false, &security) {
                Ok(next) => next,
                Err(_) => {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    continue;
                }
            };
            let connected = std::mem::replace(&mut server, next);
            tokio::spawn(serve_connection(connected, handler.clone()));
        }
    });
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub fn serve(_endpoint: &Path, _handler: Handler) -> Result<()> {
    anyhow::bail!("the control server is not supported on this platform")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Client;
    use serde_json::json;

    fn echo() -> Handler {
        Arc::new(|req: Request| {
            Box::pin(async move {
                if req.method == "fail" {
                    Response::err(req.id, "nope")
                } else {
                    Response::ok(req.id, json!({ "method": req.method, "params": req.params }))
                }
            })
        })
    }

    fn endpoint(tag: &str) -> std::path::PathBuf {
        let unique = format!("dbclaw-test-{tag}-{}", std::process::id());
        #[cfg(windows)]
        return std::path::PathBuf::from(format!(r"\\.\pipe\{unique}"));
        #[cfg(not(windows))]
        return std::env::temp_dir().join(format!("{unique}.sock"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn round_trip() {
        let path = endpoint("rt");
        serve(&path, echo()).unwrap();
        let p = path.clone();
        let (ok, err) = tokio::task::spawn_blocking(move || {
            let mut c = Client::connect(&p).unwrap();
            let ok = c.call("state", json!({ "a": 1 })).unwrap();
            let err = c.call("fail", json!({})).unwrap_err().to_string();
            (ok, err)
        })
        .await
        .unwrap();
        assert_eq!(ok, json!({ "method": "state", "params": { "a": 1 } }));
        assert_eq!(err, "nope");

        // Several clients at once.
        let mut joins = Vec::new();
        for i in 0..4 {
            let p = path.clone();
            joins.push(tokio::task::spawn_blocking(move || {
                Client::connect(&p).unwrap().call("ping", json!(i)).unwrap()
            }));
        }
        for (i, j) in joins.into_iter().enumerate() {
            assert_eq!(j.await.unwrap()["params"], json!(i));
        }
        #[cfg(unix)]
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn unix_socket_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let path = endpoint("perm");
        serve(&path, echo()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(serve(&path, echo()).is_err(), "a second server on the same socket must fail");
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread")]
    async fn windows_pipe_is_owner_only() {
        let path = endpoint("acl");
        serve(&path, echo()).unwrap();
        // Only one DACL entry: full access for the current user.
        let p = path.clone();
        let entries = tokio::task::spawn_blocking(move || {
            let f = std::fs::OpenOptions::new().read(true).write(true).open(&p).unwrap();
            let entries = crate::winsec::dacl_entries(&f).unwrap();
            let ours = crate::winsec::pipe_server_is_current_user(&f).unwrap();
            (entries, ours)
        })
        .await
        .unwrap();
        let me = crate::winsec::current_user_sid().unwrap();
        assert_eq!(entries.0, vec![(true, me)], "pipe DACL must allow only the current user");
        assert!(entries.1, "the pipe server runs as the current user");
        // A second server can't take over the name.
        assert!(serve(&path, echo()).is_err());
    }
}
