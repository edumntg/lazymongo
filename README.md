# lazymongo

*lazygit for MongoDB*: a fast, single-binary terminal UI for browsing, querying, and managing MongoDB — built in Rust with [ratatui](https://ratatui.rs) and the official MongoDB driver.

**Status:** v1 feature set (M0–M4 of [PRD.md](PRD.md)) implemented.

Measured on the current build: **3.4 MB release binary**, **~10 MB RSS** while browsing a 500-doc collection (PRD budgets: ≤10 MB / ≤50 MB). Cold start is instant; all MongoDB I/O runs off the render path.

## Install & run

```sh
# from source
cargo install --path lazymongo          # installs to ~/.cargo/bin
# or build and copy the binary anywhere on your PATH:
cargo build --release && cp target/release/lazymongo ~/.local/bin/

lazymongo "mongodb://localhost:27017"    # or mongodb+srv://…
lazymongo --readonly "mongodb+srv://…"   # block all writes
lazymongo                                # saved-connections picker (see below)
```

Tagged releases (`git tag v0.1.0 && git push origin v0.1.0`) build signed-checksum
binaries for macOS (arm64/x86_64), Linux (musl, arm64/x86_64), and Windows via
`.github/workflows/release.yml`. A Homebrew formula template lives in
`packaging/homebrew/lazymongo.rb` — copy it into a `homebrew-tap` repo and fill
in the release checksums to enable `brew install <owner>/tap/lazymongo`.

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
- `/` **search** loaded results (live jump, `n`/`N` next/prev) — json and table views
- `Esc` **cancels** a running query or aggregation mid-flight
- `y` copy doc to clipboard · `E` export loaded window as JSON or CSV
- `Ctrl-P` / `:` **command palette**: every feature fuzzy-searchable, incl. theme switching
- `m` drops you into **mongosh** on the current connection (TUI suspends/resumes)

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
- `$out`/`$merge` write stages are refused in the preview (a preview must never write)

### Themes
Six built-in palettes: `dark` (default), `light`, `claude-dark`, `claude-light`,
`termius`, `high-contrast`. Switch via the command palette (persists), the
`--theme` flag, or `theme = "claude-dark"` in config.toml. The ANSI themes
inherit your terminal's scheme; the branded ones use truecolor.

### Connections
- **In-app manager**: press `C` anywhere (or launch with no URI) — add (`a`),
  edit (`e`), delete (`d`), connect (`Enter`). Changes are written to config.toml.
- Or edit `~/.config/lazymongo/config.toml` by hand:

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

# integration tests self-seed any MongoDB (CI uses a mongo:7 service container):
docker run -d --name lazymongo-test -p 27099:27017 mongo:7
LAZYMONGO_TEST_URI=mongodb://localhost:27099 cargo test -p lazymongo-core

# PTY smoke tests (drive the real binary via expect):
cargo build && expect scripts/smoke.exp        # M0/M1: browse, fold, query
expect scripts/smoke-m2.exp                    # table view, query editor, explain
expect scripts/smoke-m3.exp                    # full write lifecycle
expect scripts/smoke-m4.exp <scratch-xdg-dir>  # picker, aggregation, persistence
expect scripts/smoke-connmgr.exp <scratch-dir> # in-app connection manager
```

CI (`.github/workflows/ci.yml`) runs fmt, clippy `-D warnings`, and the full
test suite against a MongoDB service container on every push/PR, plus build +
unit tests on macOS and Windows.

Crate layout per the PRD: `lazymongo-core` (types, relaxed query parsing, and the
Mongo I/O actor — no TUI deps) and `lazymongo` (ratatui frontend, Elm-style
update loop).

### Not yet implemented (post-v1)
`$EDITOR` integration for document editing, non-interactive `--eval` mode,
server-side `killOp` on cancel (client-side cancellation works; the server op
is still bounded by maxTimeMS), OS-keychain secrets, schema sampling view,
raw-BSON document storage. See PRD.md for the roadmap.
