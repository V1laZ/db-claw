# DB Claw

A fast, native database manager for **PostgreSQL** and **MongoDB**.

- **Snappy.** Native app with a GPU-rendered UI; it never freezes while waiting on the database.
- **One filter language** for both databases, plus full SQL and mongosh consoles.
- **Edit in the grid**, follow foreign keys and MongoDB references with one click.
- **Built-in MCP server**, so AI agents like Claude Code can drive the app.

## What it's for

DB Claw is for **simple, everyday database work**: browsing tables and collections, filtering,
looking up related rows, quick edits and ad-hoc queries.

It is **not** a full database IDE. For migrations, schema design, import/export, backups or
administration, use a more robust tool like [DBeaver](https://dbeaver.io) (SQL databases) or
[Studio 3T](https://studio3t.com) (MongoDB).

## Install and set up

Download from the [latest release](../../releases/latest), then follow your system's section.
The apps aren't code-signed yet, so the first launch needs one extra click.

**About the MCP server:** the app only accepts automation when started with `--control`. It
listens on a private channel only your user account can reach. Agents can change data, so for
production databases connect with a **read-only database user** (the *Read-only* checkbox in the
connection dialog only blocks grid edits, not console queries).

### macOS (Apple Silicon)

**Install**

1. Download `db-claw-v…-macos-arm64.dmg`, open it and drag **DB Claw** to **Applications**.
2. Open DB Claw. macOS shows *"DB Claw" Not Opened*: click **Done**, go to
   **System Settings → Privacy & Security**, click **Open Anyway** and confirm.

**Use with Claude Code**

1. Quit DB Claw, then start it with automation on:
   ```sh
   open -a "DB Claw" --args --control
   ```
2. Add the MCP server to Claude Code (once):
   ```sh
   claude mcp add --scope user dbclaw -- "/Applications/DB Claw.app/Contents/MacOS/dbclaw-ctl" mcp
   ```
3. Restart Claude Code and run `/mcp`: **dbclaw** should show as connected.

### Windows (x64)

**Install**

1. Download and run `db-claw-v…-windows-x86_64-setup.exe`. It installs for your user only, no
   admin rights needed. Keep **Add dbclaw-ctl (automation CLI / MCP server) to PATH** ticked.
2. If SmartScreen warns about an unknown publisher: **More info → Run anyway**.

**Use with Claude Code**

1. Quit DB Claw, then start it with automation on (PowerShell):
   ```powershell
   & "$env:LOCALAPPDATA\Programs\DB Claw\dbclaw.exe" --control
   ```
2. Add the MCP server to Claude Code (once), in a **new** terminal:
   ```powershell
   claude mcp add --scope user dbclaw -- dbclaw-ctl mcp
   ```
3. Restart Claude Code and run `/mcp`: **dbclaw** should show as connected.

**Claude Code in WSL** controlling DB Claw on Windows: start the app as in step 1, then in WSL:
```sh
claude mcp add --scope user dbclaw -- dbclaw-ctl.exe mcp
```
If `dbclaw-ctl.exe` isn't found, run `wsl --shutdown` in PowerShell and reopen WSL (it picks up
the new PATH), or use the full path
`"/mnt/c/Users/<you>/AppData/Local/Programs/DB Claw/dbclaw-ctl.exe"`.

> On some company PCs, IT policy blocks programs in your user folder. Windows then says it
> *cannot access the specified device, path or file*. Ask IT to allow DB Claw.

### Linux (x86_64)

**Install**

1. Download `db-claw-v…-linux-x86_64.AppImage` and put it somewhere permanent, e.g.:
   ```sh
   mkdir -p ~/Applications
   mv ~/Downloads/db-claw-v*-linux-x86_64.AppImage ~/Applications/DBClaw.AppImage
   chmod +x ~/Applications/DBClaw.AppImage
   ```
2. Run it: `~/Applications/DBClaw.AppImage`. Needs a Vulkan-capable GPU driver.

**Use with Claude Code**

1. Start it with automation on:
   ```sh
   ~/Applications/DBClaw.AppImage --control
   ```
2. Add the MCP server to Claude Code (once):
   ```sh
   claude mcp add --scope user dbclaw -- ~/Applications/DBClaw.AppImage ctl mcp
   ```
3. Restart Claude Code and run `/mcp`: **dbclaw** should show as connected.

## Try it

Ask Claude Code something like:

> Use the dbclaw tools: show my connections, open the `orders` table, filter it to orders over
> 1000 and take a screenshot.

Without an AI agent, the same commands work from a terminal: `dbclaw-ctl state`,
`dbclaw-ctl help` (on Linux: `DBClaw.AppImage ctl …`).

## Updates

DB Claw checks for updates once a day and installs them when you click **Restart to update**
(to check now: **Check for updates** on the start screen, or **DB Claw → Check for Updates…** on
macOS). Updates are signed and verified before installing.

To turn automatic checks off, put `{"check_for_updates": false}` in `settings.json` next to
your saved connections:

| System | Folder |
|---|---|
| macOS | `~/Library/Application Support/DB Claw` |
| Windows | `%APPDATA%\DB Claw` |
| Linux | `~/.config/DB Claw` |

Saved connections (`connections.json`, including passwords) live in the same folder, inside
your user profile (on macOS and Linux readable only by your user).

## License

[MIT](LICENSE)
