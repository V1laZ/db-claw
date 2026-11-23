Reorder tabs and connections by drag and drop; Windows updates no longer fail while an AI agent is connected.

## What's new

- **Drag and drop**: reorder open tabs by dragging them (drop on the empty tab bar to move one to the end), and reorder connections in the sidebar. The connection order is saved.
- `dbclaw-ctl` / MCP: a new `drag` method, so agents can reorder tabs too.

## Fixes

- **Windows**: updating, reinstalling or uninstalling failed with an access error while an AI agent (e.g. Claude Code) had the DB Claw MCP server running. The installer now stops `dbclaw-ctl.exe` first, and the uninstaller also closes the app. In Claude Code, run `/mcp` afterwards to reconnect.

  If you're on 0.2.2 or older, close Claude Code (or end `dbclaw-ctl.exe` in Task Manager) before this update. Later updates handle it themselves.

## Downloads

| Platform | File |
|---|---|
| macOS (Apple Silicon) | `db-claw-v…-macos-arm64.dmg`: open it and drag **DB Claw** to Applications |
| Windows x86_64 | `db-claw-v…-windows-x86_64-setup.exe` |
| Linux x86_64 | `db-claw-v…-linux-x86_64.AppImage`: `chmod +x` and run |

Already installed? DB Claw updates itself: you'll get a **Restart to update** notification within a day, or click **Check for updates** on the start screen.

## Notes

- **macOS**: the app isn't notarized, so the first launch of a downloaded copy shows *"DB Claw" Not Opened*. Click **Done**, open **System Settings → Privacy & Security**, click **Open Anyway** and confirm. Updates installed from inside the app don't ask again.
- **Windows**: SmartScreen may warn about an unknown publisher → *More info* → *Run anyway*.
- **Linux**: needs a Vulkan-capable GPU driver.
