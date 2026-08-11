// Copyright 2026 Florian MAZEN (F4FEZ)
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Line-delimited JSON protocol between the daemon and its clients (the
//! CLI/TUI) over a Unix socket.
//!
//! On connect, the daemon sends a [`ServerMessage::Hello`] followed by a
//! [`ServerMessage::Snapshot`] of the current state, then continuously
//! broadcasts mesh network events as [`ServerMessage::Event`]. The client can
//! request a fresh snapshot at any time with
//! [`ClientMessage::RequestSnapshot`].

use serde::{Deserialize, Serialize};

use crate::mesh::{ContactDto, MeshEventKind, SelfInfoDto};

/// IPC protocol version, to bump on incompatible changes.
pub const PROTOCOL_VERSION: u32 = 1;

/// Message sent by the CLI/TUI to the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    /// Explicit request for a full state snapshot.
    RequestSnapshot,
    /// Remove a contact from the node's own contact list, identified by its
    /// public key prefix (hex). On success, an updated [`Snapshot`] and a
    /// [`crate::mesh::MeshEventKind::ContactRemoved`] event are broadcast to
    /// all clients; on failure, a [`ServerMessage::Error`] is sent back to
    /// the requesting client only.
    RemoveContact { public_key_prefix_hex: String },
    /// Add or remove a contact from the config's managed-repeater list,
    /// identified by its public key prefix (hex). If `managed` is `true`
    /// and the contact isn't already registered in the companion, the
    /// daemon declares it first (using the full public key resolved from
    /// overheard RF log data, see
    /// [`crate::mesh::extract_discovered_node`]) — a managed repeater is
    /// always registered. On success, an updated [`Snapshot`] is broadcast
    /// to all clients; on failure, a [`ServerMessage::Error`] is sent back
    /// to the requesting client only.
    SetManagedRepeater {
        public_key_prefix_hex: String,
        name: String,
        managed: bool,
    },
}

/// Message sent by the daemon to a connected client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    /// Initial greeting, carrying the daemon's protocol version.
    Hello { version: u32 },
    /// Full snapshot of the mesh network state.
    Snapshot(Snapshot),
    /// Real-time mesh network event.
    Event(MeshEvent),
    /// Non-fatal error to report to the client (e.g. lost mesh connection).
    Error(String),
}

/// Full snapshot of the state known by the daemon at a given point in time.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Snapshot {
    /// Is the daemon currently connected to the local MeshCore node?
    pub mesh_connected: bool,
    pub self_info: Option<SelfInfoDto>,
    pub contacts: Vec<ContactDto>,
    /// Time elapsed since the daemon started, in seconds.
    pub uptime_secs: u64,
    /// Unix timestamp when this snapshot was generated.
    pub generated_at_unix: i64,
}

/// Timestamped mesh event, as broadcast to clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshEvent {
    pub at_unix: i64,
    pub kind: MeshEventKind,
}
