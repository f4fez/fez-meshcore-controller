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

use crate::config::RegionConfig;
use crate::mesh::{ContactDto, MeshEventKind, PacketLogEntry, SelfInfoDto};

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
    /// Declares a new contact directly from its full public key (hex),
    /// without requiring it to have been overheard on the mesh first —
    /// unlike [`ClientMessage::SetManagedRepeater`], which can only resolve
    /// a full key for a node already known (registered or discovered). If
    /// `managed` is `true`, it's also added to the config's
    /// managed-repeater list. On success, an updated [`Snapshot`] is
    /// broadcast to all clients; on failure, a [`ServerMessage::Error`] is
    /// sent back to the requesting client only.
    AddRepeater {
        public_key_hex: String,
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
    /// The current backlog of the raw packet log (TUI page F3), sent once
    /// right after [`ServerMessage::Snapshot`] on connect.
    PacketLog(Vec<PacketLogEntry>),
    /// A single new raw packet, pushed as it's captured. Boxed: `Snapshot`
    /// and `PacketLogEntry` are otherwise the largest variants by far,
    /// bloating every `ServerMessage` to their size regardless of which
    /// variant is actually in play.
    PacketLogEntry(Box<PacketLogEntry>),
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
    /// The cluster's configured region hierarchy (see [`crate::config::RegionConfig`]),
    /// shown as the dashboard's "Cluster" block and used to recognize
    /// packets' transport codes.
    pub regions: Vec<RegionConfig>,
    /// Configured "Hashtag Channel" names (see [`crate::config::Config::hashtag_channels`]),
    /// used alongside the well-known "Public" channel to decode `GroupText`
    /// messages in the packet log.
    pub hashtag_channels: Vec<String>,
    /// Configured MQTT brokers (see [`crate::config::Config::mqtt_brokers`])
    /// and their live connection status, shown in the TUI's "Observer node"
    /// block.
    pub mqtt_brokers: Vec<MqttBrokerStatusDto>,
}

/// A configured MQTT broker's name and live connection status — see
/// [`Snapshot::mqtt_brokers`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MqttBrokerStatusDto {
    /// Matches [`crate::config::MqttBrokerConfig::name`].
    pub name: String,
    pub status: MqttBrokerStatus,
}

/// Live connection status of a configured MQTT broker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MqttBrokerStatus {
    /// Not yet connected, or reconnecting after a drop.
    #[default]
    Connecting,
    Connected,
    /// A publish loop exited without a live connection (e.g. still waiting
    /// for the first successful connect on daemon startup, before the
    /// broker task's first status update).
    Disconnected,
    /// The last connection attempt failed; `reason` is the error, for
    /// display (e.g. "connection refused", a TLS handshake failure).
    Error {
        reason: String,
    },
}

/// Timestamped mesh event, as broadcast to clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshEvent {
    pub at_unix: i64,
    pub kind: MeshEventKind,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::PacketLogEntry;

    /// Every message must round-trip through JSON, and must serialize to a
    /// single line: the daemon <-> CLI transport is newline-delimited
    /// (`LinesCodec`), so an embedded `\n` would corrupt the stream.
    fn assert_roundtrips_as_single_line<T>(value: &T)
    where
        T: Serialize + for<'de> Deserialize<'de> + std::fmt::Debug,
    {
        let text = serde_json::to_string(value).expect("serialize");
        assert!(
            !text.contains('\n'),
            "message must serialize to a single line, got: {text}"
        );
        let _: T = serde_json::from_str(&text).expect("deserialize");
    }

    #[test]
    fn client_messages_roundtrip() {
        assert_roundtrips_as_single_line(&ClientMessage::RequestSnapshot);
        assert_roundtrips_as_single_line(&ClientMessage::RemoveContact {
            public_key_prefix_hex: "aabbccddeeff".to_string(),
        });
        assert_roundtrips_as_single_line(&ClientMessage::SetManagedRepeater {
            public_key_prefix_hex: "aabbccddeeff".to_string(),
            name: "Repeater".to_string(),
            managed: true,
        });
    }

    #[test]
    fn server_messages_roundtrip() {
        assert_roundtrips_as_single_line(&ServerMessage::Hello {
            version: PROTOCOL_VERSION,
        });
        assert_roundtrips_as_single_line(&ServerMessage::Snapshot(Snapshot::default()));
        assert_roundtrips_as_single_line(&ServerMessage::Event(MeshEvent {
            at_unix: 0,
            kind: MeshEventKind::Connected,
        }));
        assert_roundtrips_as_single_line(&ServerMessage::PacketLog(vec![]));
        assert_roundtrips_as_single_line(&ServerMessage::PacketLogEntry(Box::new(
            PacketLogEntry {
                id: 1,
                at_unix: 0,
                snr: 1.0,
                rssi: -90,
                header: None,
                payload_hex: "abcd".to_string(),
                payload_len: 2,
            },
        )));
        assert_roundtrips_as_single_line(&ServerMessage::Error("oops".to_string()));
    }

    #[test]
    fn snapshot_default_is_disconnected_and_empty() {
        let snapshot = Snapshot::default();
        assert!(!snapshot.mesh_connected);
        assert!(snapshot.self_info.is_none());
        assert!(snapshot.contacts.is_empty());
        assert!(snapshot.mqtt_brokers.is_empty());
    }

    #[test]
    fn mqtt_broker_status_variants_roundtrip() {
        assert_roundtrips_as_single_line(&MqttBrokerStatus::Connecting);
        assert_roundtrips_as_single_line(&MqttBrokerStatus::Connected);
        assert_roundtrips_as_single_line(&MqttBrokerStatus::Disconnected);
        assert_roundtrips_as_single_line(&MqttBrokerStatus::Error {
            reason: "connection refused".to_string(),
        });
    }

    #[test]
    fn mqtt_broker_status_default_is_connecting() {
        assert_eq!(MqttBrokerStatus::default(), MqttBrokerStatus::Connecting);
    }
}
