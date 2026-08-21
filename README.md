# lazymongo

> **lazygit for MongoDB** — a fast, keyboard-driven terminal UI for browsing, querying, and managing MongoDB. Single binary, ~3 MB, ~10 MB of RAM.

[![CI](https://github.com/edumntg/lazymongo/actions/workflows/ci.yml/badge.svg)](https://github.com/edumntg/lazymongo/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
![Rust](https://img.shields.io/badge/built%20with-Rust-orange)

```
┌ lazymongo  prod (mongodb+srv://***@cluster0...)  •  MongoDB 7.0  •  12ms ─────────────────────┐
│ ╭ 1 Explorer ───────────╮ ╭ 2 Results ─ app_db.users [json] ~48.2k docs (60 loaded+) ────────╮ │
│ │ ▾ app_db        1.2GB │ │ ▸ [1] { _id: ObjectId("66c3…"), name: "Ada", status: "active" } │ │
│ │    users        ~48.2k│ │ ▾ [2] {                                                         │ │
│ │    orders       ~301k │ │     name: "Grace Hopper",                                       │ │
│ │    events       ~9.1M │ │     age: 36,                                                    │ │
│ │ ▸ analytics_db        │ │   ▸ address: {…3}                                               │ │
│ ╰───────────────────────╯ ╰──────────────────────────────────────────────────────────────────╯ │
│ ╭ 3 Query (find filter) ────────────────────────────────────────────────────────────────────╮  │
│ │ filter> { status: 'active', age: { $gt: 21 } }                                            │  │
│ ╰────────────────────────────────────────────────────────────────────────────────────────────╯ │
│  ↵ run   v table   F query   x explain   e edit   d delete   a aggregate   ? help   q quit     │
└─────────────────────────────────────────────────────────────────────────────────────────────────┘
```

## Why

| | Compass | mongosh | **lazymongo** |
|---|---|---|---|
| Memory | 500 MB+ (Electron) | light | **~10 MB** |
| Startup | seconds | instant | **instant** |
| Works over SSH / in tmux | ❌ | ✅ | ✅ |
| Visual browsing, folding, tables | ✅ | ❌ | ✅ |
| Mouse + keyboard | ✅ | ❌ | ✅ |

lazymongo streams documents through cursor batches with a hard memory cap, so
opening a 100M-document collection costs the same as opening a tiny one. All
MongoDB I/O runs off the render path — the UI never blocks on the network.

## Install

**From a release** (macOS arm64/x86_64 · Linux musl arm64/x86_64 · Windows):

```sh
# grab the archive for your platform from the releases page, then:
tar xzf lazymongo-*-<your-target>.tar.gz
mv lazymongo-*/lazymongo ~/.local/bin/    # or anywhere on your PATH
```

**From source** (Rust 1.80+):

```sh
git clone https://github.com/edumntg/lazymongo && cd lazymongo
cargo install --path lazymongo            # installs to ~/.cargo/bin
```

## Quick start

```sh
lazymongo "mongodb://localhost:27017"     # any mongodb:// or mongodb+srv:// URI
lazymongo --readonly "mongodb+srv://…"    # hard-block all writes (great for prod)
lazymongo --theme claude-dark             # pick a theme
lazymongo                                 # saved-connections picker (see Connections)
```

Then: arrow keys or `j`/`k` to move, `Enter` to expand a database / open a
collection / fold a document, and **`?` for the full keymap at any time**.
Everything is also reachable by mouse (click, click headers to sort, scroll)
and through the fuzzy **command palette** (`Ctrl-P` or `:`).

## Features

### Browse & query
- Explorer sidebar with db sizes and collection counts (streamed in the
  background — expanding a db is instant even on huge remote clusters)
- Instant open on any collection size: a small first batch paints immediately,
  then pages of 50 stream in as you scroll (2,000-doc memory window)
- **JSON view** with folding and syntax highlighting, or **table view** (`v`)
  with inferred columns and server-side sort (`s` / click a header)
- Filters in relaxed **mongosh syntax**: `{ status: 'active', age: { $gt: 21 } }`,
  plus Extended JSON (`$oid`, `$date`, …); per-collection persisted history (`↑`/`↓`)
- Structured query editor (`F`): filter / projection / sort / **limit** / **skip**
- `x` **explain** with executionStats and a COLLSCAN warning banner
- `/` search the loaded results (live jump, `n`/`N`) · `Esc` cancels a running query
- `o` full-screen document view · `y` copy to clipboard · `E` export JSON/CSV

### Write operations — always confirmed, always logged
- `e` edit (JSON editor with a field-level diff before `replaceOne`) · `i` insert
- `d` delete one (with preview) · `D` **bulk delete** by filter · `U` **bulk update**
  by filter — both run a `countDocuments` **dry run first**; delete-many requires
  typing the count
- `I` indexes: list, create, drop · `N`/`X` create / drop collection (type its name)
- `L` session operations log · `--readonly` blocks writes in the UI **and** at the I/O layer

### Aggregations (`a`)
- Full-screen JSON5 pipeline editor with **stage-by-stage preview**: select any
  stage and run the pipeline truncated to it (`Enter`), or `Ctrl-R` for all of it
- Pipelines persist per collection across sessions
- `$out`/`$merge` are refused in the preview — a preview must never write

### Quality of life
- **Command palette** (`Ctrl-P` / `:`): every action fuzzy-searchable by name
- **`m` drops you into mongosh** on the current connection; exit to return
- **6 themes**: `dark` · `light` · `claude-dark` · `claude-light` · `termius` ·
  `high-contrast` — switch live from the palette (persists), `--theme`, or config
- Credentials always redacted on screen; live server version + ping in the status bar
- Panic-safe terminal restore; stale results dropped via generation tokens

## Connections

Press `C` anywhere (or launch with no URI) for the connection manager —
add / edit / delete / connect without touching a file. It persists to
`~/.config/lazymongo/config.toml`, which you can also edit by hand:

```toml
theme = "claude-dark"

[[connections]]
name = "local"
uri = "mongodb://localhost:27017"

[[connections]]
name = "prod"
uri_env = "MONGO_PROD_URI"   # secret stays in your environment, not this file
read_only = true             # RO badge + all writes blocked
```

## Key bindings (essentials — `?` shows everything)

| Key | Action |
|---|---|
| `Tab` / `1` `2` `3` | Switch pane (explorer / results / query) |
| `↑↓` `jk` / mouse wheel | Move |
| `Enter` | Expand db · open collection · fold/unfold |
| `3` + type + `Enter` | Run a filter |
| `F` | Query editor (projection / sort / limit / skip) |
| `v` · `o` · `x` | Table view · doc view · explain |
| `e` `i` `d` `D` `U` | Edit · insert · delete · bulk delete · bulk update |
| `a` · `I` · `L` · `m` | Aggregations · indexes · ops log · mongosh |
| `Ctrl-P` / `:` | Command palette |
| `C` · `r` · `?` · `q` | Connections · refresh · help · quit |

## How it works

Two crates: `lazymongo-core` owns a single async **I/O actor** (the official
MongoDB Rust driver on tokio) that the UI talks to over channels — commands in,
events out, with generation tokens so stale results can never render. The
`lazymongo` binary is a [ratatui](https://ratatui.rs) frontend with an
Elm-style update loop. No network call ever runs on the render path, every
query is cancellable, and documents live in a bounded sliding window so memory
stays flat regardless of collection size.

## Development

```sh
cargo test --workspace                       # unit tests

# integration tests self-seed any empty MongoDB:
docker run -d --name lazymongo-test -p 27099:27017 mongo:7
LAZYMONGO_TEST_URI=mongodb://localhost:27099 cargo test -p lazymongo-core

# PTY end-to-end tests (drive the real binary via expect):
cargo build && expect scripts/smoke.exp mongodb://localhost:27099
```

CI runs fmt, clippy `-D warnings`, and the full suite against a MongoDB
service container on every push; tagged `v*` releases build binaries for all
platforms with checksums.

## Roadmap

`$EDITOR` integration for document editing · non-interactive `--eval` mode for
scripting · server-side `killOp` on cancel · OS-keychain secrets · schema
sampling view. Issues and PRs welcome.

## License

[MIT](LICENSE)
