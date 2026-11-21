# DB Claw

A fast, native database manager for **PostgreSQL** and **MongoDB**, written in Rust with
[GPUI](https://gpui.rs) (Zed's GPU-accelerated UI framework) and
[GPUI Kit](https://gpui-kit.com) components.

- **Snappy by design.** The UI thread never waits on the network. All I/O runs on a dedicated tokio runtime,
  values are pre-rendered to text off the UI thread, and the grid is virtualized (only visible
  cells are laid out). Stale results are discarded when you click faster than the server answers.
- **One filter language for both databases.** Type `age > 30 and name ~ "jo"` above any table or
  collection. It compiles to a SQL `WHERE` clause or a Mongo filter.
- **Native consoles** for full SQL (multiple statements, transactions, notices) and mongosh syntax
  (`db.users.find({…}).sort({…})`, `aggregate`, `insertOne`, `updateMany`, …).
- **Editing in the grid.** Double-click a cell to edit it; deletions are staged too, and ⌘S saves everything at once
  (one transaction on Postgres). Insert rows through a form (SQL) or a document editor (Mongo).
- **Automation API.** `dbclaw-ctl` drives the running app from the shell or as an
  **MCP server**, so AI agents can use it.

## Quick start

```sh
cargo run --release -p dbclaw-app          # the app (binary: dbclaw)
```

Test databases with sample data (needs Docker):

```sh
docker compose up -d
# Postgres: postgres://dbclaw:dbclaw@localhost:54329/shop
# Mongo:    mongodb://dbclaw:dbclaw@localhost:27019/shop?authSource=admin
```

Add a connection with **⌘N**. Fill in host, port, database, user, password and optional
URL parameters (e.g. `sslmode=require`, `authSource=admin`), or switch to **URL** and paste a
connection string. The two modes convert into each other, a password inside a pasted URL
goes to the password field, and a URL pasted into the Host field is split into the fields.
MongoDB Atlas-style `mongodb+srv://` hosts are supported.

## Using it

| Where | How |
|---|---|
| Sidebar | Click a connection to connect, then a table / collection to open it. ⌘P searches tables. Right-click for consoles, refresh, edit, delete. |
| Filter bar (⌘F) | Universal filter language, applied with Enter. `?` shows the cheat sheet. Right-click a cell → *Filter by this value*. |
| Grid | Click a header to sort (server-side). Double-click / Enter / F2 edits a cell, ⌘⇧N sets NULL, ⌘⌫ marks a row for deletion, ⌘C copies, ⌘[ / ⌘] pages. |
| References | Foreign-key columns show a link icon in the header and an ↗ arrow in each cell. Click it to open the referenced row. Right-click a row → *Referenced by* opens the rows of other tables that point to it. On MongoDB, references are inferred: fields holding ObjectIds (or arrays of them, or DBRefs), including nested ones like `address.cityId` and `items.productId`, are matched to collections by name (`userId` → `users`) and confirmed by finding a sampled id in that collection; unusual names fall back to looking the id up in every collection. When one column holds several references (e.g. `route.fromCityId`, `route.toCityId`), its arrow opens a chooser listing each path and its target. |
| Saving | Edited cells turn yellow, deleted rows are struck through. ⌘S saves, *Discard* reverts. Postgres saves in one transaction and every UPDATE/DELETE must hit exactly one row (by primary key, or `ctid` for tables without one). |
| Insert | ⌘I or **+**: a form for SQL tables (empty = column default / NULL), a relaxed-JSON editor for Mongo. Right-click → *Duplicate row…* pre-fills it. |
| Autocomplete | Suggestions appear while you type; **Ctrl+Space** opens them on demand, Enter/Tab accepts. SQL console: keywords, functions, schemas, tables, and columns of the tables in the statement (also via aliases: `o.` → columns of `orders o`). Mongo console: `db.` → collections, `db.users.` → methods, `$` → operators, and field paths inside `{…}` (from a sample of the collection). Filter bar: columns (Mongo: nested paths too), then operators that fit the column type, then `and`/`or`. |
| Console (⌘T) | ⌘↵ runs the statement at the cursor (or the selection); ⇧⌘↵ runs everything; ⌘. cancels. In a Mongo console a bare `{…}` filter runs `find` on the current collection. |
| Tabs | ⌘W closes, ⌃Tab / ⌘⇧] switches, ⌘B toggles the sidebar. |

### Filter language

```text
status = "active" and age >= 18      =  !=  <  <=  >  >=
name ~ "jo"   name !~ "test"          contains / not contains (any case)
email ^= "admin"   email $= ".cz"     starts with / ends with (any case)
title like "foo%"   ilike "a_c"       SQL wildcards: % any run, _ one char
name matches "^J.*n$"   name = /^j/i  regular expression
tags in ["a", "b", 3]   not in [...]  one of / none of
deleted_at is null   is not null      age between 18 and 65
profile exists   profile not exists   field is present / absent
not (a = 1 or b = 2)                  and/&&  or/||  not/!  ( )
address.city = "Prague"   `odd name` = 1   created > date("2024-01-01")   _id = oid("…")
```

On Postgres, dotted paths into `json`/`jsonb` columns become `#>>` lookups, and array
columns use `= ANY(…)`. Values are inlined as untyped literals, so Postgres coerces
them to each column's type. Full reference: `crates/dbclaw-core/src/filter/mod.rs`.

## Load on the database

DB Claw is built to be safe to point at production servers:

- **Pages** are fetched with `LIMIT` (500 rows by default) and ordered by the primary key, so a
  page read is an index scan.
- **Row counts are time-limited to 3 s.** Postgres falls back to the planner's estimate
  (`of ~1,234,567`) or to `of many` for filters; MongoDB uses collection metadata for unfiltered
  counts and `maxTimeMS` for filtered ones.
- **Grid queries** run with a 30 s `statement_timeout`. Console queries are unlimited (you can
  cancel them with ⌘.).
- **Connections:** at most 3 browse connections plus 1 for saving per Postgres database; each
  console tab has its own session.
- **Inferring MongoDB references** samples 20 documents per collection and confirms guesses with
  `_id` lookups. It reads real collections only (never views), issues at most 60 queries per
  collection (4 at a time, 1 s server limit each), and caches results for 10 minutes. Measured
  with the MongoDB profiler on the sample database: opening a collection costs 10 queries, the
  next one 5, and a refresh 3, each under 1 ms.

## Automation: CLI and MCP

Start the app with the control server enabled:

```sh
DBCLAW_CONTROL=1 cargo run --release -p dbclaw-app     # or: dbclaw --control
```

It listens on a Unix socket readable only by your user
(`~/Library/Application Support/DB Claw/control.sock`, or `$DBCLAW_CONTROL_SOCKET`).
It's off by default because it can change data.

```sh
dbclaw-ctl state                                         # connections, tabs, errors…
dbclaw-ctl add_connection kind=postgres host=db.local port=5432 user=me password=… database=app
dbclaw-ctl open_table connection=postgres name=customers
dbclaw-ctl filter text='country = "CZ" and balance > 1000'
dbclaw-ctl sort column=balance dir=desc
dbclaw-ctl follow_reference row=0 column=customer_id     # foreign key → customers
dbclaw-ctl edit_cell row=0 column=name value='"Eva"'
dbclaw-ctl save
dbclaw-ctl run connection=mongo text='db.orders.aggregate([{$group: {_id: "$status", n: {$sum: 1}}}])'
dbclaw-ctl screenshot /tmp/dbclaw.png                    # works even when the window is covered
dbclaw-ctl help                                          # all methods; `help <method>` for params
```

Every method waits for its work to finish (page loaded, save committed, query done) and
returns JSON. Connections can be addressed by id, name, or any unique part of the name.
Low-level `key`, `type`, `action` and `click` methods reach anything the semantic methods don't.
In control mode, animations are off so the UI settles immediately (and renders even when the
window is covered).

**MCP:** `dbclaw-ctl mcp` serves the same methods as MCP tools over stdio, and `screenshot`
comes back as an image. This repository's `.mcp.json` registers it for Claude Code. For other
clients, run `dbclaw-ctl mcp` as a stdio server.

## Layout

```text
crates/
  dbclaw-core      model, SQL helpers, filter language, mongosh parser/formatter (no I/O)
  dbclaw-drivers   Postgres (tokio-postgres, simple-query text protocol) and MongoDB drivers
  dbclaw-control   automation protocol, `dbclaw-ctl` CLI + MCP server
  dbclaw-app       the GPUI application
docker/            seed data for docker-compose
```

## Tests

```sh
cargo test --workspace                                   # unit tests
docker compose up -d && DBCLAW_LIVE=1 cargo test -p dbclaw-drivers --test live
```

## Platform notes

Developed and tested on macOS. GPUI and every dependency also support Windows and Linux, and
key bindings use the platform modifier (⌘ on macOS, Ctrl elsewhere). The app hasn't been built
on Windows yet. The automation server currently needs Unix sockets (macOS/Linux), and
`screenshot` uses macOS `screencapture`.

Saved connections, including passwords, are stored in `connections.json` in the platform
config directory with `0600` permissions.
