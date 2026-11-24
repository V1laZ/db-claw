The sidebar shows only the database in your connection, unless you choose to show all.

## What's new

- **Only your connection's database by default**: a connection that names a database (e.g. `…/shop`) lists just that one in the sidebar. Tick **Show all databases** in the connection dialog to list every database you can access, e.g. for a MongoDB admin connection that sees dev and production. Connections without a database (MongoDB's `authSource` doesn't count) still list all.
- **PostgreSQL: only what you can read**: with all databases shown, PostgreSQL lists the databases that contain a table you can read (or that you own), and inside a database only the tables and views you have access to. No more tables that fail with *permission denied*.

## After updating

If your connection URL contains a database but you want to see the others too, edit the connection (right-click → *Edit connection…*) and tick **Show all databases**. Colors set on databases are kept.

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
