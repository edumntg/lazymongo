# lazymongo — Product Requirements Document

**Version:** 0.1 (draft)
**Date:** 2026-08-20
**Status:** Proposed
**Author:** Eduardo Montilva (with Claude)

---

## 1. Overview

**lazymongo** is a fast, keyboard-driven terminal UI (TUI) for managing MongoDB databases — in the spirit of `lazygit`, `lazydocker`, and `k9s`. It lets developers and operators connect to any MongoDB deployment, browse databases and collections, run queries and aggregations, edit and delete documents, and inspect results as JSON or tables — all without leaving the terminal and without the memory bloat of Compass or a browser-based GUI.

### One-liner

> *lazygit for MongoDB: a single-binary, sub-10 MB-RSS terminal client that makes working with Mongo fast, safe, and pleasant.*

### Why now / why this

| Existing tool | Problem |
|---|---|
| MongoDB Compass | Electron app; 500 MB+ RAM, slow startup, needs a GUI environment |
| `mongosh` | Powerful but a REPL — no browsing, no visual results, high friction for exploration |
| VS Code MongoDB extension | Tied to an editor, heavy, limited querying UX |
| Studio 3T / NoSQLBooster | Paid, heavy desktop apps |
| Existing Mongo TUIs (e.g. small OSS projects) | Abandoned, incomplete, or lack aggregation/edit support |

There is no maintained, polished, state-of-the-art TUI for MongoDB. That's the gap lazymongo fills.

---

## 2. Goals & Non-Goals

### Goals

1. **Instant**: cold start to interactive in < 100 ms; every UI interaction < 16 ms frame budget.
2. **Tiny**: single static binary ≤ 10 MB; idle RSS ≤ 15 MB; browsing a 1M-doc collection stays ≤ 50 MB via cursor paging (never load full result sets).
3. **Complete daily-driver**: 90% of what a developer does in Compass — browse, query, aggregate, insert, update, delete, index inspection — doable in lazymongo.
4. **Safe by default**: destructive operations (delete, drop, update-many) always require explicit confirmation; read-only mode available.
5. **Discoverable**: every action reachable by mouse click, arrow keys, or shortcut; context-sensitive help bar always visible; `?` opens a full keymap.
6. **Portable**: macOS, Linux, Windows (x86_64 + arm64), works over SSH, in tmux, and in minimal terminals; degrades gracefully without truecolor or mouse.

### Non-Goals (v1)

- Schema visualization / analytics dashboards
- Server administration (user management, replica-set reconfig, sharding ops)
- Data import/export beyond simple JSON/CSV export of results
- Embedded scripting/REPL (we shell out to `mongosh` if the user wants that)
- GUI/web version

---

## 3. Target Users & Personas

1. **Backend developer (primary)** — lives in the terminal, queries Mongo dozens of times a day during development and debugging. Wants speed and keyboard flow.
2. **SRE / on-call engineer** — SSHes into a jump box during an incident; needs to inspect production data quickly. Wants low footprint, read-only mode, and zero install friction (single binary, `curl | sh` or `brew install`).
3. **Data-curious teammate** — occasionally checks data; needs discoverability (mouse support, visible hints, no memorized commands).

---

## 4. Technology Decision

### Requirements driving the choice

- Minimal memory footprint and binary size (hard requirement)
- Official, maintained MongoDB driver
- Mature TUI ecosystem with mouse + keyboard support
- Cross-platform single static binary

### Options considered

| | Go | Rust | Zig |
|---|---|---|---|
| Mongo driver | Official (`mongo-go-driver`) | **Official (`mongodb` crate, async)** | None — would need to implement wire protocol |
| TUI ecosystem | Bubble Tea / tview (excellent) | **ratatui + crossterm (excellent; powers gitui, bottom, atuin, yazi)** | Immature |
| Idle RSS | ~15–30 MB (GC runtime) | **~3–8 MB (no GC)** | ~2–5 MB |
| Binary size | ~15–25 MB | **~4–8 MB (release, stripped)** | small |
| Dev velocity | Fastest | Good | Slow (ecosystem gaps) |
| Memory predictability | GC pauses/overhead | **Deterministic, arena-friendly** | Deterministic |

### Decision: **Rust**

- **TUI:** [`ratatui`](https://ratatui.rs) + `crossterm` (keyboard, mouse, resize events; Windows support)
- **Driver:** official `mongodb` Rust crate on `tokio` (single-threaded runtime flavor to keep threads/RSS minimal)
- **BSON/JSON:** `bson` + `serde_json`; custom pretty-printer with syntax highlighting for Extended JSON
- **Editor widget:** `tui-textarea` (or in-house) for query/aggregation input with bracket matching
- **Config:** TOML via `serde` + `toml`
- **Error handling:** `anyhow`/`thiserror`
- **Release:** `cargo-dist` → static binaries (musl on Linux), Homebrew tap, Scoop, `.deb`/`.rpm`, `cargo install lazymongo`

Zig is rejected because writing and maintaining a MongoDB wire-protocol client from scratch is a project-killing scope addition. Go is a strong runner-up but loses on the explicit "super low memory" requirement (GC runtime baseline) and binary size.

---

## 5. Product Requirements

### 5.1 Connection management

- **FR-1** Connect via full connection string (`mongodb://`, `mongodb+srv://`), including TLS, auth mechanisms (SCRAM, X.509, AWS IAM via driver), and replica sets.
- **FR-2** Saved connections in config file (`~/.config/lazymongo/config.toml`), listed on a start screen; select with arrows/click/number keys.
- **FR-3** Secrets never stored in plaintext by default: reference env vars (`${MONGO_PROD_URI}`) or OS keychain; prompt-on-connect supported.
- **FR-4** Per-connection **read-only flag** (blocks all write operations at the app layer) and **color tag** (e.g., production connections render the status bar in red).
- **FR-5** Connection health indicator (latency ping, server version, replica-set role) in the status bar; automatic reconnect with backoff on dropped connections.

### 5.2 Navigation & browsing

- **FR-6** Three-pane layout (à la lazygit): sidebar with databases → collections tree; main pane with results; bottom query/command bar. Panes focusable via `Tab`/`Shift-Tab`, number keys (`1`/`2`/`3`), or mouse click.
- **FR-7** Sidebar shows databases with collection counts and sizes; collections expandable/collapsible (`Enter`/`→`/`←`/click). Fuzzy filter with `/`.
- **FR-8** Selecting a collection immediately shows the first page of documents (default `find({}).limit(50)`), with total estimated count in the header.
- **FR-9** Infinite scroll via cursor batching: `j`/`k`/arrows/`PgUp`/`PgDn`/mouse wheel; next batch fetched on demand, previous batches evicted beyond a configurable window (memory cap).
- **FR-10** Collection metadata view: indexes (with usage stats where available), storage stats, validation rules.

### 5.3 Querying

- **FR-11** Query bar accepts **MongoDB Extended JSON filters** (`{ "status": "active", "age": { "$gt": 21 } }`) with relaxed parsing (unquoted keys, single quotes allowed — same leniency as `mongosh`).
- **FR-12** Structured query editor (toggle with `q`): separate fields for filter, projection, sort, limit, skip — each a small editable input.
- **FR-13** Query history per collection (persisted, searchable with fuzzy find); `↑`/`↓` in the query bar cycles history.
- **FR-14** Saved/named queries in config; runnable from a picker.
- **FR-15** `Explain` on any query (`x`): renders the winning plan as a collapsible tree with stage timings, docs examined vs. returned, and index used — with a red flag on COLLSCAN.
- **FR-16** Hard client-side guardrails: default `limit`, max batch size, and a configurable query timeout (`maxTimeMS`); a running query shows a spinner and is cancellable with `Esc` (kills the server-side operation via `killOp` when possible).

### 5.4 Aggregations

- **FR-17** Aggregation editor: full-screen multi-line JSON editor for pipelines with bracket matching, syntax highlighting, and stage-aware formatting.
- **FR-18** **Stage-by-stage preview**: pipeline shown as a vertical list of stages; selecting stage *n* runs the pipeline truncated to that stage (with a sample limit) so users can debug pipelines incrementally.
- **FR-19** Pipelines saved/loaded as named snippets; import/export as plain JSON files (compatible with Compass pipeline export).
- **FR-20** Same guardrails as queries (timeout, sample limits, cancellation), plus `allowDiskUse` toggle.

### 5.5 Results viewing

- **FR-21** **JSON view** (default): pretty-printed Extended JSON, syntax highlighted, collapsible objects/arrays (`Enter`/click on a node), one document per "card" with visual separators.
- **FR-22** **Table view** (`v` toggles): columns inferred from the union of top-level fields in the current batch; nested objects summarized (`{…3 fields}`); columns sortable (client-side for the page, or re-issues query with server sort), resizable, and hideable. Dot-notation column expansion for nested fields.
- **FR-23** **Single-document view** (`Enter` on a row): full-screen scrollable document with fold/unfold, field-path breadcrumb, and copy helpers.
- **FR-24** Copy to clipboard: current document (`y`), current field value (`Y`), current page as JSON array; export current result set to a JSON or CSV file (streaming, bounded memory).
- **FR-25** Large values (long strings, binary, huge arrays) truncated in list views with expand-on-demand; binary shown as type + size, never dumped raw.
- **FR-26** In-results search/highlight (`/` in the results pane) across the loaded window.

### 5.6 Writing data (CRUD)

- **FR-27** **Edit document** (`e`): opens the document in an in-app JSON editor; on save, validates JSON, computes and shows a field-level diff, and applies via `replaceOne`/`updateOne` on `_id`. Optionally opens `$EDITOR` instead (config flag).
- **FR-28** **Insert document** (`i`): blank or template (copy of selected doc without `_id`) in the editor.
- **FR-29** **Delete document** (`d`): confirmation modal showing the doc's `_id` and a summary; `D` for delete-by-current-filter with a mandatory "type the count to confirm" step.
- **FR-30** **Update many** (`u`): filter + update-document editor, with a mandatory dry-run preview (`matchedCount` via `countDocuments`) before execution.
- **FR-31** Collection ops: create collection, drop collection, create/drop index — each behind explicit confirmation modals; drop requires typing the collection name.
- **FR-32** All write results (matched/modified/deleted counts, write errors) reported in a non-blocking toast + a session "operations log" panel (`L`) for audit/undo-reference.

### 5.7 Input model (keyboard + mouse)

- **FR-33** Full mouse support: click to focus panes and select rows, click to expand tree/JSON nodes, scroll wheel everywhere, click buttons in modals, drag to resize pane splits.
- **FR-34** Vim-flavored default keymap (`j/k/h/l`, `gg/G`, `/` search) **plus** arrow keys / PgUp / PgDn / Home / End always working — no modal editing required to be productive.
- **FR-35** Always-visible bottom help bar showing context-relevant shortcuts (like lazygit); `?` opens the full, searchable keymap overlay.
- **FR-36** Command palette (`Ctrl-P` / `:`) with fuzzy-matched actions ("drop index", "export page", "switch connection", …) — every feature reachable from it.
- **FR-37** Fully remappable keys via config file.

### 5.8 Configuration & theming

- **FR-38** TOML config: connections, keymap, theme, defaults (page size, timeouts, editor choice, confirm behaviors).
- **FR-39** Built-in themes (dark, light, high-contrast) + user-definable; respects `NO_COLOR`; degrades to 16-color terminals.
- **FR-40** CLI flags mirror config for scripting/quick use: `lazymongo "mongodb://…" --db app --collection users --readonly`.

### 5.9 Non-interactive escape hatches

- **FR-41** `lazymongo --eval '<filter-json>' --collection users --format json|csv` for quick one-shot queries pipeable to `jq` (keeps lazymongo useful in scripts). *(Stretch for v1.)*

---

## 6. Non-Functional Requirements

| ID | Requirement | Target |
|---|---|---|
| NFR-1 | Cold start (binary exec → interactive UI) | < 100 ms (excl. network connect, which is async/non-blocking) |
| NFR-2 | Idle memory (connected, one collection open) | ≤ 15 MB RSS |
| NFR-3 | Memory browsing arbitrarily large collections | ≤ 50 MB RSS (bounded document window + eviction) |
| NFR-4 | Binary size (release, stripped) | ≤ 10 MB |
| NFR-5 | UI responsiveness | Input-to-render < 16 ms; all Mongo I/O off the render path (async, never blocks the event loop) |
| NFR-6 | Compatibility | MongoDB 4.4+ (server), incl. Atlas, DocumentDB best-effort; terminals: iTerm2, Terminal.app, Alacritty, kitty, WezTerm, Windows Terminal, tmux/ssh |
| NFR-7 | Safety | No destructive op without confirmation; read-only mode enforced app-wide; no telemetry, no network calls except to the user's MongoDB |
| NFR-8 | Reliability | Panics never corrupt the terminal (panic hook restores terminal state); dropped connections surface an error and offer reconnect, never crash |
| NFR-9 | Accessibility | Full functionality without mouse; without truecolor; screen layout usable at 80×24 minimum |
| NFR-10 | Quality | CI on macOS/Linux/Windows; integration tests against real MongoDB in containers; TUI snapshot tests (`insta` + ratatui `TestBackend`) |

---

## 7. UX Sketch

```
┌ Connections ─── prod-atlas (RO) ● 12ms ─ MongoDB 7.0 ─ replSet/PRIMARY ──────┐
│ ┌─ 1 Explorer ──────────┐ ┌─ 2 Results  users  ~48,201 docs  50/batch ─────┐ │
│ │ ▾ app_db        1.2GB │ │ ▾ { _id: ObjectId("66c3…"),                    │ │
│ │   ▸ users       48.2k │ │     name: "Ada Lovelace",                      │ │
│ │   ▸ orders     301.5k │ │     status: "active",                          │ │
│ │   ▸ events       9.1M │ │     age: 36, … }                               │ │
│ │ ▸ analytics_db        │ │ ▸ { _id: ObjectId("66c4…"), … }                │ │
│ │ ▸ local               │ │ ▸ { _id: ObjectId("66c5…"), … }                │ │
│ └───────────────────────┘ └────────────────────────────────────────────────┘ │
│ ┌─ 3 Query ─────────────────────────────────────────────────────────────────┐│
│ │ filter> { status: "active", age: { $gt: 21 } }                            ││
│ └────────────────────────────────────────────────────────────────────────────┘│
│ ↵ run  v table  e edit  i insert  d delete  a aggregate  x explain  ? help    │
└────────────────────────────────────────────────────────────────────────────────┘
```

---

## 8. Architecture Notes

- **Pattern:** Elm-style unidirectional loop (`Model` → `update(msg)` → `view`) — the ratatui-recommended architecture. One render task; all driver I/O in tokio tasks that send `Msg`s back over a channel. No shared mutable state between UI and I/O.
- **Memory discipline:**
  - Documents held as raw BSON (`RawDocumentBuf`) and rendered lazily; pretty JSON strings built only for visible rows.
  - Fixed-size sliding window of batches per collection view; eviction beyond the window.
  - Streaming export (never materialize a full result set).
- **Cancellation:** every server operation carries a generation token; stale results are dropped; `Esc` aborts the tokio task and issues `killOp` when the server permits.
- **Crate layout:** `lazymongo-core` (state, actions, mongo client wrapper — no TUI deps, fully unit-testable) + `lazymongo-tui` (rendering, input) + thin `main`.

---

## 9. Milestones

| Milestone | Scope | Definition of done |
|---|---|---|
| **M0 — Skeleton** (wk 1–2) | Rust workspace, ratatui app shell, event loop, panic-safe terminal handling, connect via CLI arg, list DBs/collections | Can browse a live cluster's namespaces |
| **M1 — Read MVP** (wk 3–5) | Document browsing with cursor paging, JSON view with folding, filter query bar, keyboard + mouse nav, help bar | Daily-usable read-only client |
| **M2 — Query power** (wk 6–8) | Structured query editor, sort/projection/limit, table view, single-doc view, query history, explain, copy/export | Replaces Compass for exploration |
| **M3 — Write ops** (wk 9–11) | Edit/insert/delete with confirmations, update-many with dry run, index & collection ops, read-only mode, ops log | Replaces Compass for daily CRUD |
| **M4 — Aggregations** (wk 12–14) | Pipeline editor, stage-by-stage preview, snippets, saved connections & config, themes | Feature-complete v1.0 |
| **v1.0 release** | cargo-dist packaging (brew, scoop, deb/rpm, `cargo install`), docs site, demo GIFs, benchmark table vs. Compass | Public launch |

---

## 10. Success Metrics

- Startup < 100 ms and idle RSS < 15 MB verified in CI (regression-gated benchmarks).
- A developer can go from `brew install lazymongo` to running their first query in < 60 seconds without reading docs.
- GitHub traction as proxy for product-market fit: 1k stars in 3 months post-launch.
- Zero data-loss incidents attributable to missing confirmations (safety model holds).

## 11. Risks & Mitigations

| Risk | Mitigation |
|---|---|
| Rust dev velocity slower than Go | Elm architecture keeps code simple; core/tui split keeps logic testable; scope discipline via milestones |
| `mongodb` crate gaps (e.g., some admin commands) | Fall back to `run_command` with raw BSON — everything is a command in Mongo |
| Table view on wildly heterogeneous schemas | Infer columns per batch, cap column count, always offer JSON view as source of truth |
| Terminal quirk matrix (mouse in tmux, Windows conhost) | crossterm abstracts most; CI + manual test matrix; all mouse features have key equivalents (NFR-9) |
| Users running destructive ops on prod | Read-only connections, red prod tagging, type-to-confirm for mass ops, ops log |
