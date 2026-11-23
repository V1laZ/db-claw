Select multiple rows: Shift+click for a range, ⌘/Ctrl+click to add rows.

## What's new

- **Multiple row selection** in tables and query results:
  - **Click** selects a row, **Shift+click** selects every row in between, **⌘+click** (Ctrl+click on Windows/Linux) adds or removes a row.
  - **Arrow keys** move a single-row selection; **Esc** clears it.
- **The whole selected row is highlighted**, not just the clicked cell.
- **Act on all selected rows**: **⌘⌫** (Ctrl+Backspace) or right-click → *Delete N rows* marks them for deletion (saved with ⌘S as before), and **⌘C** copies them as tab-separated lines that paste straight into a spreadsheet.
- Automation: `click` accepts `modifiers` (`shift`, `secondary`), and the table state lists the selected rows.

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
