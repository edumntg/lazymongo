# lazymongo

*lazygit for MongoDB*: a fast, single-binary terminal UI for browsing and querying MongoDB — built in Rust with [ratatui](https://ratatui.rs) and the official MongoDB driver.

**Status:** M1 (read-only daily driver). See [PRD.md](PRD.md) for the full roadmap.

Measured on the current build: **2.9 MB binary**, **~7 MB RSS** while browsing a 500-doc collection. Cold start is instant; all MongoDB I/O runs off the render path.

## Run

```sh
cargo build --release
./target/release/lazymongo "mongodb://localhost:27017"   # URI optional, this is the default
```

## What works today (M0 + M1)

- Connect via connection string (TLS/auth per driver support), credentials redacted in the UI
- Explorer sidebar: databases with sizes, collections with estimated counts, `/` filter
- Document browsing with cursor batching (50/batch), infinite scroll, and a sliding
  memory window (max 2000 docs held; older batches evicted)
- Foldable, syntax-highlighted Extended JSON view (fold any object/array, per document)
- Filter query bar with mongosh-style relaxed syntax: `{ status: 'active', age: { $gt: 21 } }`
  plus canonical Extended JSON (`{ _id: { $oid: "…" } }`), and query history (↑/↓)
- Full keyboard (arrows *and* vim keys) + mouse (click to focus/select/open, wheel scroll)
- Always-visible context help bar; `?` opens the complete keymap
- Connection health: server version + live ping in the status bar
- Panic-safe terminal restore; stale query results dropped via generation tokens

## Keys (essentials)

| Key | Action |
|---|---|
| `Tab` / `1` `2` `3` | Switch pane (Explorer / Results / Query) |
| `↑↓` / `j k` | Move |
| `Enter` | Expand db / open collection / fold-unfold document node |
| `/` | Filter sidebar |
| `3` then type, `Enter` | Run a filter query |
| `r` | Refresh |
| `?` | Full keymap |
| `q` / `Ctrl-C` | Quit |

## Development

```sh
# unit tests
cargo test --workspace

# integration test + PTY smoke test need a seeded MongoDB:
docker run -d --name lazymongo-test -p 27099:27017 mongo:7
# (seed it — see scripts/smoke.exp expectations, or use your own data)
LAZYMONGO_TEST_URI=mongodb://localhost:27099 cargo test -p lazymongo-core
cargo build && expect scripts/smoke.exp mongodb://localhost:27099
```

Crate layout per the PRD: `lazymongo-core` (types, relaxed query parsing, and the
Mongo I/O actor — no TUI deps) and `lazymongo` (ratatui frontend, Elm-style
update loop).
