# fez-mesh-controller

Independent management and monitoring of [MeshCore](https://meshcore.co.uk/) repeaters within
your configured regions, built on top of [`meshcore-rs`](https://crates.io/crates/meshcore-rs) —
no public/cloud infrastructure required, staying true to MeshCore's off-grid spirit.

Repeaters heard within your configured regions are what the controller is responsible for. Each
one can be handled at one of three levels:

- **tracked** — known and followed, no further action.
- **supervised** *(WIP)* — actively monitored via MeshCore telemetry, so configuration errors or
  failures can be caught directly from the traffic.
- **managed** *(WIP)* — fully controlled, including remote (re)configuration.

Today's `managed_repeaters` is the first building block toward this: a repeater you flag is
declared as a companion contact and highlighted throughout the tools — the seed of the *tracked*
tier, not the full three-level model yet.

All of this logic belongs to the **daemon**. A TUI is provided as a window onto what the daemon
already knows.

## Workspace structure

- **`core`** (`fez-mesh-controller-core`) — shared library, statically linked into both binaries: configuration (TOML), daemon ↔ CLI IPC protocol, MeshCore client and serializable data types.
- **`daemon`** (`fez-mesh-controller-daemon`) — service that connects to the MeshCore node (serial, TCP or BLE), maintains the network state and broadcasts it over a Unix socket.
- **`cli`** (`fez-mesh-controller`) — colorful, animated CLI/TUI (ratatui + crossterm + dialoguer) that talks to the daemon over that socket.

## Current status

The project is being built incrementally, starting with the monitoring side — important to have
a solid foundation before tackling active management. Implemented today:

- A real-time TUI dashboard: local node info, known contacts, a live event log, and the
  configured region hierarchy.
- A packet log: every raw RF packet the node hears, repeated relays grouped together, addressing
  info, and rows highlighted when a managed repeater is involved.
- An interactive setup wizard, including region hierarchy configuration.

Configuration-error/failure detection from traffic or telemetry, and remote repeater
configuration, are not implemented yet *(WIP)*.

## Prerequisites & installation

Requires a recent stable [Rust toolchain](https://rustup.rs/) and a MeshCore companion node
reachable over serial, TCP or BLE.

```sh
git clone https://github.com/florianmazen/fez-mesh-controller
cd fez-mesh-controller
cargo build --release
```

## Getting started

```sh
# 1. Runs the interactive setup wizard on first launch
cargo run -p fez-mesh-controller-cli

# 2. Starts the daemon (MeshCore node connection + IPC server)
cargo run -p fez-mesh-controller-daemon

# 3. Real-time dashboard (or `-- status` for a one-off snapshot)
cargo run -p fez-mesh-controller-cli
```

Configuration is stored in `~/.config/fez-mesh-controller/config.toml` (re-run the wizard with
`fez-mesh-controller setup`). See [`config.example.toml`](config.example.toml) for every
available parameter, with comments.

## CLI commands

The daemon must be running for any of these to work — they all talk to it over the IPC socket,
never directly to the node.

```sh
fez-mesh-controller                 # the real-time dashboard
fez-mesh-controller status          # one-off snapshot, then exit
fez-mesh-controller repeater list   # manage repeaters without the TUI
fez-mesh-controller-daemon --daemon # run the daemon in the background, with rotating logs
```

Run `fez-mesh-controller --help` or `fez-mesh-controller repeater --help` for the full command
reference.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
