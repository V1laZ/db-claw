DB Claw now comes with proper installers and updates itself.

## What's new

- **Installers**: a `.dmg` for macOS, a per-user setup wizard for Windows (no admin rights, Start menu entry, uninstaller, `dbclaw-ctl` on `PATH`) and an AppImage for Linux.
- **In-app updates**: DB Claw checks for new versions daily, downloads them in the background and installs on **Restart to update**. Updates are signed and verified before anything is installed. Turn it off with `"check_for_updates": false` in `settings.json`.
- **New app icon.**
- **Automation on Windows**: `dbclaw-ctl` and the MCP server now work on Windows too (named pipe, owner-only), including from WSL.
- **Screenshots** for agents are rendered by the app itself (macOS and Windows), even when the window is covered.
- Windows binaries no longer need the Visual C++ runtime.

## Downloads

| Platform | File |
|---|---|
| macOS (Apple Silicon) | `db-claw-v…-macos-arm64.dmg`: open it and drag **DB Claw** to Applications |
| Windows x86_64 | `db-claw-v…-windows-x86_64-setup.exe` |
| Linux x86_64 | `db-claw-v…-linux-x86_64.AppImage`: `chmod +x` and run |

`*.app.tar.gz` and `latest.json` are used by in-app updates. `SHA256SUMS.txt` lists checksums.

## Notes

- **macOS**: the app isn't notarized, so the first launch shows *"DB Claw" Not Opened*. Click **Done**, open **System Settings → Privacy & Security**, click **Open Anyway** next to the DB Claw message and confirm. Or run `xattr -dr com.apple.quarantine "/Applications/DB Claw.app"`. Updates installed from inside the app don't ask again.
- **Windows**: SmartScreen may warn about an unknown publisher → *More info* → *Run anyway*.
- **Linux**: needs a Vulkan-capable GPU driver, plus `libxkbcommon`, `fontconfig` and `wayland`/`X11` libraries (present on typical desktops).
- Copies from 0.1.0 don't update themselves; install 0.2.0 once and later versions arrive automatically.
