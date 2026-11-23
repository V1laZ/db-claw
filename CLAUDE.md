# DB Claw: development notes

README.md is for users only (install, MCP setup, updates). Keep development details here.

Rust workspace, GPUI via `gpui-kit` 0.10 (gpui-pre 0.3.12, gpui-component).

## Layout

```text
crates/
  dbclaw-core      model, SQL helpers, filter language, mongosh parser/formatter,
                   completion, update manifest + signatures (no I/O)
  dbclaw-drivers   Postgres (tokio-postgres, simple-query text protocol), MongoDB, update client
  dbclaw-control   automation protocol, control server (Unix socket / Windows named pipe),
                   `dbclaw-ctl` CLI + MCP server, WSL hand-off
  dbclaw-app       the GPUI application (binary `dbclaw`)
  dbclaw-release   release tool: keygen, signed `latest.json` manifest, verify
docker/            seed data for docker-compose
packaging/         macOS dmg layout/background, Windows Inno Setup script, Linux AppImage files
scripts/           package-macos.sh, package-linux.sh, render-dmg-background.sh, setup-update-key.sh
```

## Run and test

```sh
cargo run --release -p dbclaw-app                  # the app
docker compose up -d                               # test databases with sample data
#   Postgres: postgres://dbclaw:dbclaw@localhost:54329/shop
#   Mongo:    mongodb://dbclaw:dbclaw@localhost:27019/shop?authSource=admin
cargo test --workspace                             # unit tests
DBCLAW_LIVE=1 cargo test -p dbclaw-drivers --test live   # against the docker databases
cargo clippy -p dbclaw-app -p dbclaw-core -p dbclaw-drivers
```

Useful env vars: `DBCLAW_CONFIG_DIR` (scratch config dir), `DBCLAW_CONTROL=1` (same as
`--control`), `DBCLAW_NO_UPDATE_CHECK=1`, `DBCLAW_UPDATE_URL`, `DBCLAW_UPDATE_AS_VERSION`
(pretend to be another version, for testing updates).

## Driving the UI for testing

Run with `DBCLAW_CONTROL=1 DBCLAW_CONFIG_DIR=<scratch>` and use `target/debug/dbclaw-ctl`
(`state`, `open_table`, `filter`, `follow_reference`, `click x= y=`, `drag x1= y1= x2= y2= [hold_ms=]`, `key`, `type`,
`screenshot <file>`). Screenshots are rendered by the app offscreen (macOS, Windows).

- The control server redraws the window after every request, which **hides "stale until
  something redraws" bugs**. To reproduce one: temporarily remove the post-request
  `window.refresh()` in `control.rs`, trigger with the low-level `click` (no redraw after the
  click), and capture with `screencapture -l <state.window.number>`.
- Synthetic keys can differ from a real keyboard; confirm keyboard behaviour manually.

## Design constraints

- **Snappiness:** the UI thread never waits on I/O. All I/O runs on the drivers' tokio runtime
  (`util::io`). Grid frame cost matters: measure before/after visual changes to the grid
  (a temporary `bench` control method timing `window.draw` over 600 frames worked well).
- **Database load** (the app must be safe on production servers):
  - pages use `LIMIT` (500 default) ordered by primary key
  - counts are limited to 3 s (Postgres falls back to the planner estimate)
  - grid queries run with a 30 s `statement_timeout`; consoles are unlimited but cancellable
  - Postgres: at most 3 browse connections + 1 for saving per database
  - Mongo reference inference: 20 sampled docs, ≤ 60 queries per collection, 4 concurrent,
    1 s `maxTimeMS`, real collections only (no views), cached 10 min
- **Saving:** Postgres saves in one transaction; each UPDATE/DELETE must hit exactly one row
  (primary key, or `ctid` without one).
- **Filter language** reference: `crates/dbclaw-core/src/filter/mod.rs`. The in-app cheat sheet
  is `filter::HELP`; a test checks every example parses.

## Gotchas

- `use gpui_kit::*` shadows `#[test]`: test modules must not `use super::*` in files that import it.
- **Grid** (`grid.rs`): columns have zero padding; each cell pads itself and draws the stripe,
  row line and hover, so rows end at the last column without extra elements per cell.
  This relies on gpui-component internals; re-check visually after upgrading it:
  - `render_tr` returns `border_b_0()`, which wins only because the table applies our style
    after its own border;
  - `main::tune_theme` clears the theme's `table_hover` (the table's own hover spans the full
    row) and cells paint `GridHover` instead; it must run after every theme sync;
  - `CELL_LINE_HEIGHT` assumes the default 32 px row height.
- **Mongo TLS:** the driver's rustls trusts only its bundled Mozilla roots, so company-CA
  servers failed with `UnknownIssuer`. `mongo::trust_system_roots` writes the OS trust store to
  `<cache dir>/DB Claw/system-ca.pem` and passes it as `tlsCAFile` (unless the URL sets one).
- **Windows installer:** a running `.exe` can't be replaced or deleted, and `dbclaw-ctl.exe mcp`
  keeps running while an agent is open. Restart Manager doesn't close windowless processes, so
  `dbclaw.iss` stops `dbclaw-ctl.exe` before install/uninstall (and closes `dbclaw.exe` before
  uninstall). `packaging/windows/test-installer.ps1` checks this in CI on every release build.
- The app quits when its window closes (`QuitMode::LastWindowClosed`, also on macOS).

## Releases

1. Bump `version` in the root `Cargo.toml`, write `.github/release-notes.md` (its first line is
   shown in the in-app update notification).
2. Push a `vX.Y.Z` tag. `.github/workflows/release.yml` builds macOS (.dmg + .app.tar.gz),
   Windows (Inno Setup installer) and Linux (AppImage), signs the update files into
   `latest.json` with the `UPDATE_SIGNING_KEY` secret, and publishes the release. A tag with a
   hyphen (`v0.3.0-beta.1`) becomes a pre-release (beta channel only).
3. Check that `https://dbclaw.vilaz.dev/v1/check?platform=macos-arm64&version=<previous>`
   offers the new version, and verify the signatures with `dbclaw-release verify`.

Updates: Ed25519 signatures over platform + version + SHA-256; the app trusts the keys in
`dbclaw_core::update::TRUSTED_KEYS`. The signing key was created with
`scripts/setup-update-key.sh`. The update server is maintained separately; the app depends only
on its `/v1/check` contract (`dbclaw_drivers::updates`).

Windows builds link the C runtime statically (`.cargo/config.toml`). Builds aren't code-signed
or notarized yet.
