# fez-mesh-controller

Controller and monitor for a [MeshCore](https://meshcore.co.uk/) network, built on top of [`meshcore-rs`](https://crates.io/crates/meshcore-rs).

## Workspace structure

- **`core`** (`fez-mesh-controller-core`) — shared library, statically linked into both binaries: configuration (TOML), daemon ↔ CLI IPC protocol, MeshCore client and serializable data types.
- **`daemon`** (`fez-mesh-controller-daemon`) — service that connects to the MeshCore node (serial, TCP or BLE), maintains the network state and broadcasts it over a Unix socket.
- **`cli`** (`fez-mesh-controller`) — colorful, animated CLI/TUI (ratatui + crossterm + dialoguer) that talks to the daemon over that socket.

## Getting started

```sh
# 1. Runs the interactive setup wizard on first launch
cargo run -p fez-mesh-controller-cli

# 2. Starts the daemon (MeshCore node connection + IPC server)
cargo run -p fez-mesh-controller-daemon

# 3. Real-time dashboard (or `-- status` for a one-off snapshot)
cargo run -p fez-mesh-controller-cli
```

Configuration is stored in `~/.config/fez-mesh-controller/config.toml` (re-run the wizard with `fez-mesh-controller setup`). See [`config.example.toml`](config.example.toml) for every available parameter, with comments.

## Daemon command-line options

```sh
fez-mesh-controller-daemon [--config <CONFIG>] [--daemon]
```

- `--config <CONFIG>` — load configuration from this file instead of the default location.
- `--daemon` — detach from the terminal and run in the background. Logs then go to daily-rotating files under the configured `log_dir` (set via the setup wizard, default `~/.local/state/fez-mesh-controller/logs` on Linux) instead of the console.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
