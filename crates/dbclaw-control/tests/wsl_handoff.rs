//! The WSL hand-off: inside WSL, with no app listening on the Linux side,
//! the Linux `dbclaw-ctl` forwards to `dbclaw-ctl.exe` (WSL interop).
//! Simulated with `WSL_DISTRO_NAME` and a fake `dbclaw-ctl.exe` script.
#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::Command;

fn fake_windows_ctl(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dbclaw-wsl-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("dbclaw-ctl.exe");
    std::fs::write(&exe, "#!/bin/sh\necho \"forwarded: $*\"\nexit 3\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

fn ctl(dir: &std::path::Path, wsl: bool, extra: &[(&str, &str)], args: &[&str]) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_dbclaw-ctl"));
    cmd.args(args)
        .env("PATH", format!("{}:/usr/bin:/bin", dir.display()))
        .env("DBCLAW_CONTROL_SOCKET", dir.join("nobody-listens.sock"))
        .env_remove("DBCLAW_CTL_EXE")
        .env_remove("DBCLAW_WSL_HANDOFF");
    if wsl {
        cmd.env("WSL_DISTRO_NAME", "Test");
    } else {
        cmd.env_remove("WSL_DISTRO_NAME");
    }
    for (k, v) in extra {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

#[test]
fn forwards_to_windows_ctl_in_wsl() {
    let dir = fake_windows_ctl("fwd");
    let out = ctl(&dir, true, &[], &["filter", "text=x = 1"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "forwarded: filter text=x = 1");
    assert_eq!(out.status.code(), Some(3), "exit code is passed through");

    // mcp mode is forwarded too (stdio is inherited).
    let out = ctl(&dir, true, &[], &["mcp"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "forwarded: mcp");

    // DBCLAW_CTL_EXE wins over PATH.
    let other = fake_windows_ctl("explicit");
    let exe = other.join("dbclaw-ctl.exe");
    std::fs::write(&exe, "#!/bin/sh\necho explicit\n").unwrap();
    let out = ctl(&dir, true, &[("DBCLAW_CTL_EXE", exe.to_str().unwrap())], &["state"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "explicit");
}

#[test]
fn stays_local_when_not_wanted() {
    let dir = fake_windows_ctl("local");
    // Not in WSL.
    let out = ctl(&dir, false, &[], &["state"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot reach DB Claw"), "{out:?}");
    // Opted out.
    let out = ctl(&dir, true, &[("DBCLAW_WSL_HANDOFF", "0")], &["state"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot reach DB Claw"), "{out:?}");
    // Help never leaves.
    let out = ctl(&dir, true, &[], &["help"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("METHODS"));
}

#[test]
fn explains_when_windows_ctl_is_missing() {
    let dir = std::env::temp_dir().join(format!("dbclaw-wsl-empty-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = ctl(&dir, true, &[], &["state"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("dbclaw-ctl.exe wasn't found"), "{err}");
}
