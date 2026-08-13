# CLAUDE.md

Project-specific context for Claude Code sessions on this repo — so it doesn't need to be
re-derived or re-verified each time.

## Project overview

fez-mesh-controller is independent management and monitoring for MeshCore repeaters within
configured regions — no public/cloud infrastructure, in keeping with MeshCore's off-grid
philosophy. Repeaters can be **tracked** (known/followed), **supervised** (telemetry-based
error/failure detection — WIP), or fully **managed** (remote (re)configuration — WIP). See
README.md for the user-facing description; this file covers what the README doesn't: internal
architecture, protocol details, and project-specific working conventions.

The project is being built incrementally: monitoring/visualization (the TUI, packet log,
dashboard) came first as the foundation; active management is the long-term goal and is largely
unimplemented today. **When a change moves a README-flagged "(WIP)" feature into reality, flag
this to the user and propose updating README.md — don't update it unprompted.**

## Architecture

Cargo workspace, 3 crates.

### `core` (`fez-mesh-controller-core`) — shared library
- `config.rs` — `Config`/`RegionConfig`/`ManagedRepeater`/`ConnectionConfig`/`DaemonConfig`, TOML load/save (`~/.config/fez-mesh-controller/config.toml`).
- `ipc.rs` — `ClientMessage`/`ServerMessage`/`Snapshot`: the daemon ↔ CLI wire protocol (see below).
- `mesh.rs` — `MeshClient` (wraps `meshcore-rs`), serializable DTOs (`ContactDto`, `SelfInfoDto`, `PacketLogEntry`, `PacketHeaderInfo`, `MeshEventKind`...), packet header decoding (`build_packet_log_entry`).
- `meshcore_crypto.rs` — firmware-compatible crypto primitives (region key derivation, transport code calculation). Pure functions, no domain types.
- `region.rs` — region business logic built on `meshcore_crypto`: precomputing region keys, matching a packet's transport code, flattening the region hierarchy for display.

### `daemon` (`fez-mesh-controller-daemon`) — where the actual management logic belongs
- `main.rs` — entrypoint; `--daemon` runs detached with daily-rotating logs.
- `state.rs` — `AppState`: shared `Snapshot`, `Config`, packet log cache.
- `mesh_task.rs` — owns the connection to the physical node (serial/TCP/BLE via `meshcore-rs`), reconnect loop, refreshes `AppState` on connect/events.
- `server.rs` — IPC server: accepts CLI connections, handshakes (`Hello` → `Snapshot` → `PacketLog` backlog), handles `ClientMessage`s, broadcasts events/packets.
- `lock.rs` — single-instance enforcement via a lock file.

### `cli` (`fez-mesh-controller`) — everything here talks to the daemon over IPC, never directly to the node
- `main.rs` — `clap` CLI: `setup`, `status`, `repeater {list,add,manage,unmanage,remove}`, or no subcommand for the TUI dashboard.
- `wizard.rs` — first-run/`setup` interactive config (`dialoguer`), including a small embedded `ratatui` raw-mode screen for arranging the region hierarchy (arrow-key indent/unindent, `Shift+↑/↓` to reorder).
- `status.rs`, `repeater.rs` — one-off IPC-backed subcommands.
- `ipc_client.rs` — IPC socket client (connect, handshake, send/recv).
- `tui/` — the dashboard: `mod.rs` (event loop), `app.rs` (`App` state, `apply_snapshot`), `ui.rs` (rendering, packet detail popup, Cluster block), `packet_group.rs` (grouping repeated relays, managed-repeater highlight logic).

## IPC protocol (`core/src/ipc.rs`)

JSON, one message per line (`LinesCodec`), over a Unix socket (`daemon.socket_path` in config).
On connect: daemon sends `Hello{version}` → `Snapshot` → `PacketLog` (backlog), then streams
`Event`/`PacketLogEntry` as they happen. `ClientMessage::RequestSnapshot` triggers a fresh
`Snapshot`; the CLI also sends this automatically whenever a `Connected` event arrives, so the
dashboard doesn't stay stale after a node reconnect.

## Build requirement: local `meshcore-rs` checkout

`Cargo.toml` has `[patch.crates-io] meshcore-rs = { path = "../meshcore-rs" }` — the workspace
needs a **sibling checkout** of `meshcore-rs` (not just the published crate), for RawData/LogData
decoding features not yet merged/released upstream. Without `../meshcore-rs` present next to
this repo, `cargo build` fails. Temporary (per the `Cargo.toml` comment) — drop the patch once
merged/published. Deliberately not documented in README.md (kept as an internal/dev detail).

## MeshCore protocol knowledge (verified against firmware source, not assumed)

All verified directly against `meshcore-dev/MeshCore` firmware source (via `gh search code` /
`WebFetch`, not inferred) — see file:line references below before trusting a related assumption
that isn't re-verified.

**Packet header**: route type (`Flood`/`Direct`/`TransportFlood`/`TransportDirect`), payload
type, payload version, an optional 4-byte transport code (only on `Transport*` routes), and a
path of hop hashes (`path_hash_size` bytes each, 1-4, varies per network — `Packet.h`).

**Address hashes are fixed at 1 byte, independent of `path_hash_size`** (a bug we hit and fixed:
`path_hash_size` only sizes the *path's* hop hashes, not payload addressing):
- `Req`/`Response`/`TextMsg`/`Path`: 1-byte dest hash + 1-byte src hash prefix the payload.
- `AnonReq`: 1-byte dest hash only (sender's full public key follows instead of a src hash).
- `GroupText`/`GroupData`: a 1-byte *channel* hash instead — a different hash space, never to be
  matched against a node's public key.
- `Advert`/`Ack`/others: no address hash at all.

**Region / transport code scheme** (`src/helpers/TransportKeyStore.cpp`,
`src/helpers/RegionMap.h`):
- A region's 16-byte key is `SHA256(region_name)[..16]` — deterministic, no secret provisioning
  (firmware comment: "calc key for publicly-known hashtag region name").
- The transport code is **not** a fixed per-region value: `HMAC-SHA256(region_key,
  payload_type_byte || payload)`, truncated to the first 2 bytes, computed **per packet**.
  `0x0000`/`0xFFFF` are reserved and nudged to `0x0001`/`0xFFFE`.
- Verified test vector (`core/src/meshcore_crypto.rs` tests, cross-checked independently via
  Python `hashlib`/`hmac`): region name `"TestRegion"`, payload_type `2` (TextMsg), payload
  `deadbeef` → key `fb705f20c71afb2cf417eb2edaba6e26`, code `a518`.
- The region *hierarchy* (parent-pointer tree, `RegionEntry{id, parent, flags, name}`) is an
  administrative/display concept only — it plays no role in the crypto above, which is keyed
  purely by region `name`.
- **`{0, 0}` (both transport code halves zero) is not a leftover/unset value** — it's the
  firmware's explicit "Share" marker: `isShare()` in `examples/simple_repeater/MyMesh.cpp`
  checks exactly this, comment: `{ 0, 0 } means 'send this nowhere'`. Also set explicitly by
  `BaseChatMesh::shareContactZeroHop()`. Means "don't repeat/flood this" — e.g. resharing a
  contact's advert to immediate neighbors only.
- "Cluster" is this project's own term — not a firmware concept (`gh search code "cluster"` on
  the firmware repo returns zero hits).

## Testing conventions

- Unit tests throughout, particularly `core`/`daemon` (see "Working conventions" below).
- `cli/src/tui/ui.rs` render tests use `ratatui::backend::TestBackend`: render into a buffer,
  then assert on both text content (`buffer[(x,y)].symbol()`) and cell styling
  (`buffer[(x,y)].fg`/`.bg`) — string-contains alone can't catch color/highlight bugs.
- For visual sanity checks during development (not left in the final tests): temporarily
  `eprintln!` the rendered `TestBackend` output, run with `cargo test -- --nocapture`, then
  remove the `eprintln!` before finishing.
- Crypto/protocol test vectors are cross-checked independently (e.g. via a throwaway Python
  script) rather than only self-consistently — catches bugs that a self-consistent Rust-only
  test would miss.

## Working conventions (project-specific — see the user's separate skill/framework for general workflow methodology)

- Add/update unit tests for every change in `core`/`daemon`, not just on request.
- Always confirm with the user before `git push`, even if a request's wording seems to imply it.
- Never add wizard/config parameters unprompted — ask first.
- Before considering a task done: full `cargo build --workspace`, `cargo test --workspace`,
  `cargo clippy --workspace --all-targets`, `cargo fmt --check`.
- README.md WIP markers: when a change implements something flagged `(WIP)` there, tell the user
  and propose the README update — don't edit it unprompted.

## Reference

- Firmware source of truth for protocol questions: https://github.com/meshcore-dev/MeshCore
  (use `gh search code "<term>" --repo meshcore-dev/MeshCore` to verify rather than assume).
- `meshcore-rs` (this project's Rust client library): local checkout at `../meshcore-rs`, see
  "Build requirement" above.
