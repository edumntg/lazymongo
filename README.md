# lazymongo

*lazygit for MongoDB*: a fast, single-binary terminal UI for browsing, querying, and managing MongoDB — built in Rust with [ratatui](https://ratatui.rs) and the official MongoDB driver.

**Status:** v1 feature set (M0–M4 of [PRD.md](PRD.md)) implemented.

Measured on the current build: **3.4 MB release binary**, **~10 MB RSS** while browsing a 500-doc collection (PRD budgets: ≤10 MB / ≤50 MB). Cold start is instant; all MongoDB I/O runs off the render path.

## Run

```sh
cargo build --release
./target/release/lazymongo "mongodb://localhost:27017"    # or mongodb+srv://…
./target/release/lazymongo --readonly "mongodb+srv://…"   # block all writes
./target/release/lazymongo                                # uses saved connections (see below)
```

## Features

### Browse & query
- Explorer sidebar: databases with sizes, collections with estimated counts, `/` filter
- Cursor-batched browsing (50/batch), infinite scroll, sliding memory window (max 2000 docs)
- Foldable syntax-highlighted Extended JSON view; **table view** (`v`) with inferred
  columns, column navigation, and server-side sort (`s` or click a header)
- Filter bar with mongosh-style relaxed syntax (`{ status: 'active', age: { $gt: 21 } }`)
  and canonical Extended JSON (`$oid`, `$date`, …); per-collection **persisted history** (↑/↓)
- Structured query editor (`F`): filter / projection / sort / limit / skip
- Full-screen document view (`o`, or `Enter` on a table row)
- `x` **explain** (executionStats) with a COLLSCAN warning banner
- `y` copy doc to clipboard · `E` export loaded window as JSON or CSV

### Write operations (all confirmed, all logged)
- `e` edit document in a JSON editor — field-level diff shown before `replaceOne`
- `i` insert document · `d` delete document (confirm with preview)
- `D` delete-by-filter and `U` update-many: **dry-run count first**; delete-many
  requires typing the count
- `I` index view: create (JSON key spec) and drop indexes
- `N` create collection · `X` drop collection (type its name to confirm)
- `L` session operations log; every write auto-refreshes the open view
- `--readonly` (or per-connection `read_only`) blocks writes in the UI **and** at the I/O layer

### Aggregations (`a`)
- Full-screen JSON5 pipeline editor with **stage-by-stage preview**: select any stage
  and run the pipeline truncated to it (`Enter`), or `Ctrl-R` for the full pipeline
- Pipelines are persisted per collection across sessions

### Connections
- `~/.config/lazymongo/config.toml`:

```toml
[[connections]]
name = "local"
uri = "mongodb://localhost:27017"

[[connections]]
name = "prod"
uri_env = "MONGO_PROD_URI"   # secret stays in the environment
read_only = true             # RO badge + writes blocked
```

- Run `lazymongo` with no URI to get the connection picker
- Credentials are always redacted in the UI; server version + live ping in the status bar

Keyboard (arrows *and* vim keys) and mouse (click to focus/select/open/sort,
wheel scroll) everywhere; `?` shows the full keymap; panic-safe terminal restore.

## Development

```sh
# unit tests
cargo test --workspace

# integration + PTY smoke tests need a seeded MongoDB:
docker run -d --name lazymongo-test -p 27099:27017 mongo:7
LAZYMONGO_TEST_URI=mongodb://localhost:27099 cargo test -p lazymongo-core
cargo build && expect scripts/smoke.exp        # M0/M1: browse, fold, query
expect scripts/smoke-m2.exp                    # table view, query editor, explain
expect scripts/smoke-m3.exp                    # full write lifecycle
expect scripts/smoke-m4.exp <scratch-xdg-dir>  # picker, aggregation, persistence
```

Crate layout per the PRD: `lazymongo-core` (types, relaxed query parsing, and the
Mongo I/O actor — no TUI deps) and `lazymongo` (ratatui frontend, Elm-style
update loop).

### Not yet implemented (post-v1)
Themes/config beyond connections, in-results text search, `$EDITOR` integration,
non-interactive `--eval` mode, `killOp`-based cancellation, raw-BSON document
storage. See PRD.md for the roadmap.
