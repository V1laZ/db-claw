Color-code connections and databases (e.g. red for production), and see only the databases you can access.

## What's new

- **Color tags**: give a connection a color in the connection dialog, or right-click it in the sidebar → **Color**. It shows as a bar next to the connection and everything under it, and as a colored line and tint on its tabs, so you always know whether you're looking at production or dev.
- **Per-database colors**: right-click a database → **Color** to override the connection's color, e.g. on a MongoDB admin connection that sees both dev and production databases. Tabs take the color of their database.
- **Only accessible databases** in the sidebar:
  - PostgreSQL lists the databases your user may connect to.
  - MongoDB lists the databases your user can read. Users with monitoring roles no longer see every database on the server.
- Automation: `add_connection` takes `color`, a new `set_color` method colors a connection or database, and the state reports the colors.

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
