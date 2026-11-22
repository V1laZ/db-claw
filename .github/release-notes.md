Grid polish and fixes for macOS.

## What's new

- **Tables end where your data ends**: stripes, row lines and the hover highlight stop at the last column, and the empty space below the last row stays plain.
- **Readable column headers**: the column name is the prominent part, the data type is dimmed.
- **Filter syntax help** is now grouped into sections with code examples and plain descriptions, instead of a wall of monospace text.
- Long values end in "…" instead of being cut off.

## Fixes

- **macOS**: closing the window now quits DB Claw. Before, the app kept running without a window and couldn't be reopened from the Dock.
- Faster grid drawing than in the first version of the new look (same speed as 0.2.0).

## Downloads

| Platform | File |
|---|---|
| macOS (Apple Silicon) | `db-claw-v…-macos-arm64.dmg`: open it and drag **DB Claw** to Applications |
| Windows x86_64 | `db-claw-v…-windows-x86_64-setup.exe` |
| Linux x86_64 | `db-claw-v…-linux-x86_64.AppImage`: `chmod +x` and run |

Already on 0.2.0? DB Claw updates itself: you'll get a **Restart to update** notification within a day, or use **DB Claw → Check for Updates…** right away.

## Notes

- **macOS**: the app isn't notarized, so the first launch of a downloaded copy shows *"DB Claw" Not Opened*. Click **Done**, open **System Settings → Privacy & Security**, click **Open Anyway** next to the DB Claw message and confirm. Or run `xattr -dr com.apple.quarantine "/Applications/DB Claw.app"`. Updates installed from inside the app don't ask again.
- **Windows**: SmartScreen may warn about an unknown publisher → *More info* → *Run anyway*.
- **Linux**: needs a Vulkan-capable GPU driver, plus `libxkbcommon`, `fontconfig` and `wayland`/`X11` libraries (present on typical desktops).
