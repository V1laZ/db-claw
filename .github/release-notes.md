First release of **DB Claw**, a fast native database manager for **PostgreSQL** and **MongoDB**, built in Rust with GPUI.

## Highlights

- **Snappy UI**: all I/O runs off the UI thread and the grid is virtualized.
- **One filter language for both databases**: `status = "paid" and total > 100 and address.city ~ "pra"`.
- **Native consoles**: SQL (multiple statements, transactions, notices) and mongosh syntax (`db.users.find(…)`, `aggregate`, `updateMany`, …).
- **Editing in the grid**: inline cell edits and staged deletes, saved with ⌘S in one transaction on Postgres; row form and document editor for inserts.
- **References**: follow Postgres foreign keys (both directions), and inferred MongoDB ObjectId references, including nested ones.
- **Autocomplete** in the SQL console, the Mongo console and the filter bar (Ctrl+Space).
- **Safe on production**: time-limited counts, statement timeouts on grid queries, and light, cached reference inference.
- **Automation**: `dbclaw-ctl` drives the running app from the shell or as an **MCP server** for AI agents (macOS/Linux).

## Downloads

| Platform | File |
|---|---|
| macOS (Apple Silicon) | `db-claw-v…-macos-arm64.zip`: unzip and move **DB Claw.app** to Applications |
| Linux x86_64 | `db-claw-v…-linux-x86_64.tar.gz`: run `./dbclaw` |
| Windows x86_64 | `db-claw-v…-windows-x86_64.zip`: run `dbclaw.exe` |

Every archive also contains `dbclaw-ctl` and the README. `SHA256SUMS.txt` lists checksums.

## Notes

- **macOS**: the app isn't notarized. On first launch, right-click **DB Claw.app** → **Open**, or run
  `xattr -dr com.apple.quarantine "DB Claw.app"`.
- **Windows**: SmartScreen may warn about an unknown publisher → *More info* → *Run anyway*.
- **Linux**: needs a Vulkan-capable GPU driver, plus `libxkbcommon`, `fontconfig` and `wayland`/`X11` libraries (present on typical desktops).
- Windows and Linux builds are new and less tested than macOS. Please report anything odd.
- The automation server (`DBCLAW_CONTROL=1`) and `dbclaw-ctl` are available on macOS and Linux only for now.
