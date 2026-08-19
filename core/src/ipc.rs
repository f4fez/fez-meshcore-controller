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
use crate::mesh::{
    ContactDto, MeshEventKind, NodeStatsDto, PacketLogEntry, RepeaterDetailCategory, SelfInfoDto,
    TelemetryDto,
};

/// IPC protocol version, to bump on incompatible changes.
pub const PROTOCOL_VERSION: u32 = 3;

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
    /// Sets, changes or clears a repeater's status in the config's
    /// managed-repeater list, identified by its public key prefix (hex).
    /// `status: None` removes the entry entirely; `Some(status)` creates it
    /// (if new) or updates its tier (if it already exists). If `status` is
    /// `Some(_)` and the contact isn't already registered in the companion,
    /// the daemon declares it first (using the full public key resolved
    /// from overheard RF log data, see
    /// [`crate::mesh::extract_discovered_node`]) — every tier requires the
    /// repeater to be a real companion contact, not just `Managed`. On
    /// success, an updated [`Snapshot`] is broadcast to all clients; on
    /// failure, a [`ServerMessage::Error`] is sent back to the requesting
    /// client only.
    SetRepeaterStatus {
        public_key_prefix_hex: String,
        name: String,
        status: Option<crate::config::RepeaterStatus>,
    },
    /// Declares a new contact directly from its full public key (hex),
    /// without requiring it to have been overheard on the mesh first —
    /// unlike [`ClientMessage::SetRepeaterStatus`], which can only resolve
    /// a full key for a node already known (registered or discovered). If
    /// `managed` is `true`, it's also added to the config's
    /// managed-repeater list with status `Managed` (this entry point
    /// doesn't expose the `Known`/`Supervised` tiers). On success, an
    /// updated [`Snapshot`] is broadcast to all clients; on failure, a
    /// [`ServerMessage::Error`] is sent back to the requesting client only.
    AddRepeater {
        public_key_hex: String,
        name: String,
        managed: bool,
    },
    /// Requests a fresh telemetry read from a known contact (typically a
    /// managed repeater), identified by its public key prefix (hex). A
    /// prior login is attempted automatically if the matching
    /// `managed_repeaters` config entry has a `password` set. On success,
    /// a [`ServerMessage::TelemetryResult`] is sent back to the requesting
    /// client only (mesh round trips are too slow to wait for a snapshot
    /// refresh); on failure, a [`ServerMessage::Error`] is sent instead.
    RequestTelemetry { public_key_prefix_hex: String },
    /// Requests status + telemetry + neighbours + region hierarchy from a
    /// known contact together, as one combined command (one login, then all
    /// four requests sequentially — see
    /// `daemon::mesh_task::request_repeater_detail`). Best-effort and
    /// progressive: each category is sent back as its own
    /// [`ServerMessage::RepeaterDetailCategory`] as soon as it's available,
    /// rather than waiting for all four — so the requesting client can
    /// render the popup incrementally instead of all at once. A
    /// [`ServerMessage::Error`] is only sent for a transport-level failure
    /// (e.g. the mesh connection task isn't running, or it goes silent for
    /// too long).
    RequestRepeaterDetail { public_key_prefix_hex: String },
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
    /// Direct reply to [`ClientMessage::RequestTelemetry`], sent only to
    /// the requesting connection (not broadcast).
    TelemetryResult {
        public_key_prefix_hex: String,
        telemetry: TelemetryDto,
    },
    /// One category of a [`ClientMessage::RequestRepeaterDetail`] fetch's
    /// outcome, sent to the requesting connection only (not broadcast) as
    /// soon as it's available — several of these arrive per request, one
    /// per category, in the order attempted (status, telemetry, neighbours,
    /// regions). See [`crate::mesh::RepeaterDetailDto::apply_category`] for
    /// how the client folds them into a single accumulated view.
    RepeaterDetailCategory {
        public_key_prefix_hex: String,
        category: RepeaterDetailCategory,
    },
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
    /// The node's last-known core/radio/packet stats, shown in the TUI's
    /// "Observer node" block — see
    /// [`crate::mesh::MeshClient::node_stats`].
    pub node_stats: Option<NodeStatsDto>,
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
        assert_roundtrips_as_single_line(&ClientMessage::SetRepeaterStatus {
            public_key_prefix_hex: "aabbccddeeff".to_string(),
            name: "Repeater".to_string(),
            status: Some(crate::config::RepeaterStatus::Known),
        });
        assert_roundtrips_as_single_line(&ClientMessage::SetRepeaterStatus {
            public_key_prefix_hex: "aabbccddeeff".to_string(),
            name: "Repeater".to_string(),
            status: None,
        });
        assert_roundtrips_as_single_line(&ClientMessage::RequestTelemetry {
            public_key_prefix_hex: "aabbccddeeff".to_string(),
        });
        assert_roundtrips_as_single_line(&ClientMessage::RequestRepeaterDetail {
            public_key_prefix_hex: "aabbccddeeff".to_string(),
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
        assert_roundtrips_as_single_line(&ServerMessage::TelemetryResult {
            public_key_prefix_hex: "aabbccddeeff".to_string(),
            telemetry: TelemetryDto {
                fetched_at_unix: 0,
                readings: vec![crate::telemetry::TelemetryReading {
                    channel: 1,
                    label: "Voltage".to_string(),
                    value: 3.71,
                    unit: "V".to_string(),
                }],
            },
        });
        assert_roundtrips_as_single_line(&ServerMessage::RepeaterDetailCategory {
            public_key_prefix_hex: "aabbccddeeff".to_string(),
            category: RepeaterDetailCategory::Telemetry(Ok(TelemetryDto {
                fetched_at_unix: 0,
                readings: vec![],
            })),
        });
        assert_roundtrips_as_single_line(&ServerMessage::RepeaterDetailCategory {
            public_key_prefix_hex: "aabbccddeeff".to_string(),
            category: RepeaterDetailCategory::Status(Err("timed out".to_string())),
        });
        assert_roundtrips_as_single_line(&ServerMessage::RepeaterDetailCategory {
            public_key_prefix_hex: "aabbccddeeff".to_string(),
            category: RepeaterDetailCategory::Regions(Ok(crate::mesh::RegionHierarchyDto {
                fetched_at_unix: 0,
                entries: vec![crate::mesh::RegionHierarchyEntryDto {
                    name: "World".to_string(),
                    depth: 0,
                    is_home: false,
                    flood_allowed: true,
                }],
                raw_text: "World F".to_string(),
            })),
        });
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
