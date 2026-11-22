//! In-app updates.
//!
//! Release builds ask the update server (`dbclaw_core::update::DEFAULT_UPDATE_URL`)
//! a few seconds after start and then once a day. A newer version is
//! downloaded quietly and verified (SHA-256 + Ed25519 signature, see
//! `dbclaw_drivers::updates`); the user then gets a "Restart to update"
//! notification. Nothing is installed without that click.
//!
//! Installing, per platform:
//! - Windows: run the downloaded Inno Setup installer silently (per-user, no
//!   UAC); it replaces the files and starts the app again.
//! - macOS: unpack the new `.app` next to the running one, swap the two, and
//!   reopen it once this process has exited.
//! - Linux: replace the AppImage file and start it again.
//!
//! Installs that can't update themselves (the portable Windows zip, a
//! read-only `/Applications`, a plain Linux binary) get a "Download" link to
//! the release page instead.
//!
//! Environment overrides: `DBCLAW_UPDATE_URL` (another update server),
//! `DBCLAW_NO_UPDATE_CHECK=1` (no automatic checks), and for testing
//! `DBCLAW_UPDATE_AS_VERSION` (pretend to be another version).

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use dbclaw_core::update::{self, Offer};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::WindowExt as _;
use gpui_kit::*;

use crate::actions::CheckForUpdates;
use crate::util::io;

const FIRST_CHECK_AFTER: Duration = Duration::from_secs(5);
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
const RELEASES_PAGE: &str = "https://github.com/V1laZ/db-claw/releases/latest";

/// Identifies the update notification, so each new one replaces the last.
struct UpdateNote;

#[derive(Clone)]
struct Ready {
    offer: Offer,
    file: PathBuf,
}

enum Found {
    /// Downloaded and verified; installs on restart.
    Ready(Box<Ready>),
    /// Newer version exists, but this install can't replace itself.
    Manual(Offer),
}

enum Status {
    Idle,
    Busy,
    Ready(Box<Ready>),
}

struct Updater {
    window: AnyWindowHandle,
    status: Status,
    base_url: String,
    version: String,
    channel: String,
}

impl Global for Updater {}

/// The version this build reports to the update server.
pub fn current_version() -> String {
    std::env::var("DBCLAW_UPDATE_AS_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string())
}

pub fn init(window: AnyWindowHandle, cx: &mut App) {
    let settings = crate::store::load_settings();
    let base_url = std::env::var("DBCLAW_UPDATE_URL").unwrap_or_else(|_| update::DEFAULT_UPDATE_URL.to_string());
    let automatic = settings.check_for_updates
        && std::env::var_os("DBCLAW_NO_UPDATE_CHECK").is_none()
        && (!cfg!(debug_assertions) || std::env::var_os("DBCLAW_UPDATE_URL").is_some());
    cx.set_global(Updater {
        window,
        status: Status::Idle,
        base_url,
        version: current_version(),
        channel: settings.update_channel,
    });
    cx.on_action(|_: &CheckForUpdates, cx| check(true, cx));
    if automatic {
        cx.spawn(async move |cx| {
            cx.background_executor().timer(FIRST_CHECK_AFTER).await;
            loop {
                cx.update(|cx| check(false, cx));
                cx.background_executor().timer(CHECK_EVERY).await;
            }
        })
        .detach();
    }
}

/// Look for an update. `manual` = the user asked, so report every outcome;
/// automatic checks only speak up when an update is ready.
pub fn check(manual: bool, cx: &mut App) {
    let Some(u) = cx.try_global::<Updater>() else { return };
    match &u.status {
        Status::Busy => {
            if manual {
                notify(Notification::info("Already checking for updates…"), cx);
            }
            return;
        }
        Status::Ready(ready) => {
            if manual {
                show_ready((**ready).clone(), cx);
            }
            return;
        }
        Status::Idle => {}
    }
    let (base, version, channel) = (u.base_url.clone(), u.version.clone(), u.channel.clone());
    cx.global_mut::<Updater>().status = Status::Busy;
    if manual {
        notify(Notification::info("Checking for updates…"), cx);
    }
    cx.spawn(async move |cx| {
        let result = find(base, version.clone(), channel, manual, cx).await;
        cx.update(|cx| {
            cx.global_mut::<Updater>().status = Status::Idle;
            match result {
                Ok(Some(Found::Ready(ready))) => {
                    cx.global_mut::<Updater>().status = Status::Ready(ready.clone());
                    show_ready(*ready, cx);
                }
                Ok(Some(Found::Manual(offer))) => show_manual(offer, cx),
                Ok(None) => {
                    if manual {
                        notify(Notification::success(format!("DB Claw {version} is the latest version.")), cx);
                    }
                }
                Err(e) => {
                    if manual {
                        notify(Notification::error(format!("Update failed: {e:#}")).autohide(false), cx);
                    } else {
                        eprintln!("db-claw: update check failed: {e:#}");
                    }
                }
            }
        });
    })
    .detach();
}

async fn find(base: String, version: String, channel: String, manual: bool, cx: &mut AsyncApp) -> Result<Option<Found>> {
    let Some(offer) =
        io(async move { dbclaw_drivers::updates::check(&base, &version, &channel).await }).await?
    else {
        return Ok(None);
    };
    if !can_self_update(&offer.kind) {
        return Ok(Some(Found::Manual(offer)));
    }
    if manual {
        let v = offer.version.clone();
        cx.update(|cx| notify(Notification::info(format!("Downloading DB Claw {v}…")).autohide(false), cx));
    }
    let o = offer.clone();
    let file = io(async move { dbclaw_drivers::updates::download(&o).await }).await?;
    Ok(Some(Found::Ready(Box::new(Ready { offer, file }))))
}

fn notify(note: Notification, cx: &mut App) {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let Some(window) = cx.try_global::<Updater>().map(|u| u.window) else { return };
    // A fresh key per toast: re-pushing a key that's being dismissed would
    // drop the new toast along with the old one.
    let key = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // Deferred: callers may be inside this window's update (a button click),
    // where updating the window again fails.
    cx.defer(move |cx| {
        if let Err(e) = window.update(cx, |_, window, cx| {
            window.remove_notification::<UpdateNote>(cx);
            window.push_notification(note.id1::<UpdateNote>(ElementId::Integer(key)), cx);
        }) {
            eprintln!("db-claw: can't show the update notification: {e:#}");
        }
    });
}

fn first_line(notes: &str) -> String {
    notes.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.trim_start_matches(['-', '*', ' ']).to_string())
        .unwrap_or_default()
}

fn show_ready(ready: Ready, cx: &mut App) {
    let summary = first_line(&ready.offer.notes);
    let note = Notification::new()
        .title(format!("DB Claw {} is ready to install", ready.offer.version))
        .message(if summary.is_empty() { "Restart DB Claw to finish updating.".to_string() } else { summary })
        .action(move |_, _, _| {
            let ready = ready.clone();
            Button::new("restart-to-update").primary().label("Restart to update").on_click(move |_, _, cx| {
                match install(&ready) {
                    Ok(()) => cx.quit(),
                    Err(e) => notify(
                        Notification::error(format!("Couldn't install the update: {e:#}")).autohide(false),
                        cx,
                    ),
                }
            })
        });
    notify(note, cx);
}

fn show_manual(offer: Offer, cx: &mut App) {
    let url = if offer.notes_url.starts_with("https://") { offer.notes_url.clone() } else { RELEASES_PAGE.to_string() };
    let note = Notification::new()
        .title(format!("DB Claw {} is available", offer.version))
        .message("This copy can't update itself; download the new version from GitHub.")
        .action(move |_, _, _| {
            let url = url.clone();
            Button::new("download-update").primary().label("Download").on_click(move |_, _, cx| cx.open_url(&url))
        });
    notify(note, cx);
}

// ---------------------------------------------------------------------------
// Installing

/// Can this install replace itself with an update of `kind`?
fn can_self_update(kind: &str) -> bool {
    install_target(kind).is_ok()
}

fn writable_dir(dir: &Path) -> bool {
    let probe = dir.join(format!(".dbclaw-write-test-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// What gets replaced: the install folder (Windows), the `.app` bundle
/// (macOS) or the AppImage file (Linux).
fn install_target(kind: &str) -> Result<PathBuf> {
    let exe = std::env::current_exe()?.canonicalize()?;
    match (std::env::consts::OS, kind) {
        ("windows", "installer") => {
            let dir = exe.parent().context("no install folder")?;
            // Only installed copies have the uninstaller; a portable copy
            // would otherwise get a second, installed one.
            if !dir.join("unins000.exe").exists() {
                bail!("not an installed copy");
            }
            Ok(dir.to_path_buf())
        }
        ("macos", "app-tarball") => {
            let bundle = exe
                .ancestors()
                .find(|p| p.extension().is_some_and(|e| e == "app"))
                .context("not running from an .app bundle")?;
            let parent = bundle.parent().context("no folder around the app")?;
            if !writable_dir(parent) {
                bail!("{} isn't writable", parent.display());
            }
            Ok(bundle.to_path_buf())
        }
        ("linux", "appimage") => {
            let image = PathBuf::from(std::env::var_os("APPIMAGE").context("not running as an AppImage")?);
            let parent = image.parent().context("no folder around the AppImage")?;
            if !image.is_file() || !writable_dir(parent) {
                bail!("{} can't be replaced", image.display());
            }
            Ok(image)
        }
        _ => Err(anyhow!("no self-update for `{kind}` on this platform")),
    }
}

/// Install the update; the caller quits the app right after.
fn install(ready: &Ready) -> Result<()> {
    let target = install_target(&ready.offer.kind)?;
    platform_install(&ready.file, &target)
}

#[cfg(target_os = "windows")]
fn platform_install(file: &Path, _install_dir: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt as _;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    // The installer closes this process if it's still around (Restart
    // Manager) and starts the new version when done.
    std::process::Command::new(file)
        .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CLOSEAPPLICATIONS", "/UPDATE"])
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .context("starting the installer")?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn platform_install(file: &Path, bundle: &Path) -> Result<()> {
    let parent = bundle.parent().context("no folder around the app")?;
    let staging = parent.join(format!(".dbclaw-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir(&staging)?;
    let result = (|| -> Result<()> {
        let status = std::process::Command::new("/usr/bin/tar")
            .arg("-xzf")
            .arg(file)
            .arg("-C")
            .arg(&staging)
            .status()
            .context("running tar")?;
        if !status.success() {
            bail!("unpacking the update failed");
        }
        let new = std::fs::read_dir(&staging)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .find(|p| p.extension().is_some_and(|e| e == "app") && p.join("Contents/MacOS/dbclaw").is_file())
            .context("the update doesn't contain DB Claw.app")?;
        let old = staging.join("previous.app");
        std::fs::rename(bundle, &old).context("moving the current app aside")?;
        if let Err(e) = std::fs::rename(&new, bundle) {
            let _ = std::fs::rename(&old, bundle);
            return Err(e).context("moving the new app into place");
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    relaunch_when_gone(r#"open "$1"; rm -rf "$2""#, &[bundle, &staging])
}

#[cfg(target_os = "linux")]
fn platform_install(file: &Path, image: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let name = image.file_name().context("bad AppImage path")?.to_string_lossy();
    let tmp = image.with_file_name(format!(".{name}.update"));
    std::fs::copy(file, &tmp).context("copying the new AppImage")?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    // The running image stays mounted; renaming over it is safe.
    std::fs::rename(&tmp, image).context("replacing the AppImage")?;
    relaunch_when_gone(r#"nohup "$1" >/dev/null 2>&1 &"#, &[image])
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn platform_install(_file: &Path, _target: &Path) -> Result<()> {
    bail!("self-update isn't supported on this platform")
}

/// Run `script` (with `args` as `$1`, `$2`, …) once this process has exited.
#[cfg(unix)]
fn relaunch_when_gone(script: &str, args: &[&Path]) -> Result<()> {
    use std::os::unix::process::CommandExt as _;
    let pid = std::process::id();
    std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("while kill -0 {pid} 2>/dev/null; do sleep 0.2; done; {script}"))
        .arg("sh")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .context("scheduling the restart")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn first_line_skips_headings() {
        assert_eq!(super::first_line("## What's new\n\n- Faster grids\n- More"), "Faster grids");
        assert_eq!(super::first_line(""), "");
    }
}
