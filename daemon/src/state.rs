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

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use fez_mesh_controller_core::ipc::{MeshEvent, Snapshot};
use fez_mesh_controller_core::mesh::{DiscoveredNode, PacketLogEntry};
use fez_mesh_controller_core::Config;
use tokio::sync::{broadcast, mpsc, RwLock};

use crate::command::DaemonCommand;

/// State shared between the mesh connection task and the IPC server.
pub struct AppState {
    pub snapshot: RwLock<Snapshot>,
    pub events_tx: broadcast::Sender<MeshEvent>,
    pub started_at: Instant,
    /// Forwards commands from IPC clients to the mesh connection task,
    /// which is the only one holding the live MeshCore connection.
    pub command_tx: mpsc::Sender<DaemonCommand>,
    /// The daemon's own copy of the config, mutated and persisted when a
    /// client adds/removes a managed repeater at runtime.
    pub config: RwLock<Config>,
    pub config_path: PathBuf,
    /// Nodes (repeaters/room servers) overheard but not (yet) a companion
    /// contact, keyed by 12-hex-char public key prefix. Populated from
    /// decoded RF log data, which (unlike the plain `Advertisement` push)
    /// carries the full public key needed to register them. Bounded to
    /// `discovered_repeaters_capacity` entries, least-recently-seen
    /// evicted first — see [`Self::upsert_discovered_node`].
    pub discovered_repeaters: RwLock<HashMap<String, DiscoveredNode>>,
    pub discovered_repeaters_capacity: usize,
    /// Rotating cache of raw packets (newest first), for the TUI's packet
    /// log page. Bounded to `packet_log_capacity` entries.
    pub packet_log: RwLock<VecDeque<PacketLogEntry>>,
    pub packet_log_capacity: usize,
    pub packet_log_tx: broadcast::Sender<PacketLogEntry>,
    next_packet_id: AtomicU64,
}

impl AppState {
    pub fn new(
        command_tx: mpsc::Sender<DaemonCommand>,
        config: Config,
        config_path: PathBuf,
    ) -> Self {
        let (events_tx, _rx) = broadcast::channel(256);
        let (packet_log_tx, _rx) = broadcast::channel(256);
        let packet_log_capacity = config.daemon.packet_log_capacity.max(1);
        let discovered_repeaters_capacity = config.daemon.discovered_nodes_capacity.max(1);
        Self {
            snapshot: RwLock::new(Snapshot::default()),
            events_tx,
            started_at: Instant::now(),
            command_tx,
            config: RwLock::new(config),
            config_path,
            discovered_repeaters: RwLock::new(HashMap::new()),
            discovered_repeaters_capacity,
            packet_log: RwLock::new(VecDeque::with_capacity(packet_log_capacity)),
            packet_log_capacity,
            packet_log_tx,
            next_packet_id: AtomicU64::new(1),
        }
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Broadcasts an event to connected IPC clients (silently dropped if
    /// nobody is listening).
    pub fn broadcast_event(&self, event: MeshEvent) {
        let _ = self.events_tx.send(event);
    }

    pub fn next_packet_id(&self) -> u64 {
        self.next_packet_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Records a new raw packet in the rotating cache (evicting the oldest
    /// if at capacity) and broadcasts it to connected clients.
    pub async fn record_packet(&self, entry: PacketLogEntry) {
        {
            let mut log = self.packet_log.write().await;
            if log.len() >= self.packet_log_capacity {
                log.pop_back();
            }
            log.push_front(entry.clone());
        }
        let _ = self.packet_log_tx.send(entry);
    }

    /// Records a newly-overheard node in the discovered-nodes cache
    /// (refreshing its entry if already known), evicting the
    /// least-recently-seen one if the cache is at capacity. Returns
    /// whether this is a genuinely new node, not just an updated
    /// last-seen timestamp for one already tracked.
    pub async fn upsert_discovered_node(&self, node: DiscoveredNode) -> bool {
        let mut discovered = self.discovered_repeaters.write().await;
        let is_new = !discovered.contains_key(&node.public_key_prefix_hex);

        if is_new && discovered.len() >= self.discovered_repeaters_capacity {
            if let Some(oldest_prefix) = discovered
                .iter()
                .min_by_key(|(_, n)| n.last_seen_unix)
                .map(|(prefix, _)| prefix.clone())
            {
                discovered.remove(&oldest_prefix);
            }
        }

        discovered.insert(node.public_key_prefix_hex.clone(), node);
        is_new
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fez_mesh_controller_core::mesh::MeshEventKind;
    use fez_mesh_controller_core::{Config, ConnectionConfig, DaemonConfig};

    fn make_state(packet_log_capacity: usize) -> AppState {
        let (command_tx, _command_rx) = mpsc::channel(1);
        let config = Config {
            node_label: "test-node".to_string(),
            connection: ConnectionConfig::Tcp {
                host: "127.0.0.1".to_string(),
                port: 5000,
            },
            daemon: DaemonConfig {
                socket_path: PathBuf::from("/tmp/fez-mesh-controller-test.sock"),
                refresh_interval_secs: 5,
                log_level: "info".to_string(),
                log_dir: PathBuf::from("/tmp/fez-mesh-controller-test/logs"),
                packet_log_capacity,
                discovered_nodes_capacity: 200,
            },
            managed_repeaters: vec![],
            regions: vec![],
        };
        AppState::new(
            command_tx,
            config,
            PathBuf::from("/tmp/fez-mesh-controller-test.toml"),
        )
    }

    fn sample_packet(id: u64) -> PacketLogEntry {
        PacketLogEntry {
            id,
            at_unix: 0,
            snr: 1.0,
            rssi: -90,
            header: None,
            payload_hex: String::new(),
            payload_len: 0,
        }
    }

    fn make_state_with_discovered_capacity(discovered_nodes_capacity: usize) -> AppState {
        let (command_tx, _command_rx) = mpsc::channel(1);
        let config = Config {
            node_label: "test-node".to_string(),
            connection: ConnectionConfig::Tcp {
                host: "127.0.0.1".to_string(),
                port: 5000,
            },
            daemon: DaemonConfig {
                socket_path: PathBuf::from("/tmp/fez-mesh-controller-test.sock"),
                refresh_interval_secs: 5,
                log_level: "info".to_string(),
                log_dir: PathBuf::from("/tmp/fez-mesh-controller-test/logs"),
                packet_log_capacity: 500,
                discovered_nodes_capacity,
            },
            managed_repeaters: vec![],
            regions: vec![],
        };
        AppState::new(
            command_tx,
            config,
            PathBuf::from("/tmp/fez-mesh-controller-test.toml"),
        )
    }

    fn sample_node(prefix: &str, last_seen_unix: i64) -> DiscoveredNode {
        DiscoveredNode {
            name: "Node".to_string(),
            public_key_hex: "aa".repeat(32),
            public_key_prefix_hex: prefix.to_string(),
            is_repeater: true,
            adv_type: 2,
            lat: 0.0,
            lon: 0.0,
            last_seen_unix,
        }
    }

    #[test]
    fn next_packet_id_increments_starting_at_one() {
        let state = make_state(500);
        assert_eq!(state.next_packet_id(), 1);
        assert_eq!(state.next_packet_id(), 2);
        assert_eq!(state.next_packet_id(), 3);
    }

    #[test]
    fn packet_log_capacity_is_at_least_one() {
        // A misconfigured `packet_log_capacity = 0` must not make the cache
        // unusable (and `VecDeque::with_capacity(0)` would be fine, but a
        // capacity of 0 in `record_packet`'s eviction check would drop
        // every entry immediately).
        let (command_tx, _rx) = mpsc::channel(1);
        let config = Config {
            node_label: "test-node".to_string(),
            connection: ConnectionConfig::Tcp {
                host: "127.0.0.1".to_string(),
                port: 5000,
            },
            daemon: DaemonConfig {
                socket_path: PathBuf::from("/tmp/fez-mesh-controller-test.sock"),
                refresh_interval_secs: 5,
                log_level: "info".to_string(),
                log_dir: PathBuf::from("/tmp/fez-mesh-controller-test/logs"),
                packet_log_capacity: 0,
                discovered_nodes_capacity: 200,
            },
            managed_repeaters: vec![],
            regions: vec![],
        };
        let state = AppState::new(command_tx, config, PathBuf::from("/tmp/x.toml"));
        assert_eq!(state.packet_log_capacity, 1);
    }

    #[tokio::test]
    async fn record_packet_keeps_newest_first_up_to_capacity() {
        let state = make_state(2);

        state.record_packet(sample_packet(1)).await;
        state.record_packet(sample_packet(2)).await;
        state.record_packet(sample_packet(3)).await;

        let log = state.packet_log.read().await;
        let ids: Vec<u64> = log.iter().map(|p| p.id).collect();
        // Oldest (id 1) evicted; newest (id 3) at the front.
        assert_eq!(ids, vec![3, 2]);
    }

    #[tokio::test]
    async fn record_packet_broadcasts_to_subscribers() {
        let state = make_state(500);
        let mut rx = state.packet_log_tx.subscribe();

        state.record_packet(sample_packet(1)).await;

        let received = rx.try_recv().expect("should have received the packet");
        assert_eq!(received.id, 1);
    }

    #[test]
    fn broadcast_event_without_subscribers_does_not_panic() {
        let state = make_state(500);
        state.broadcast_event(MeshEvent {
            at_unix: 0,
            kind: MeshEventKind::Connected,
        });
    }

    #[tokio::test]
    async fn upsert_discovered_node_reports_new_vs_already_known() {
        let state = make_state_with_discovered_capacity(200);

        assert!(
            state
                .upsert_discovered_node(sample_node("aaaaaaaaaaaa", 1))
                .await
        );
        // Same prefix seen again: not new, just refreshed.
        assert!(
            !state
                .upsert_discovered_node(sample_node("aaaaaaaaaaaa", 2))
                .await
        );

        let discovered = state.discovered_repeaters.read().await;
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered["aaaaaaaaaaaa"].last_seen_unix, 2);
    }

    #[tokio::test]
    async fn upsert_discovered_node_evicts_the_least_recently_seen_when_at_capacity() {
        let state = make_state_with_discovered_capacity(2);

        state
            .upsert_discovered_node(sample_node("aaaaaaaaaaaa", 1))
            .await;
        state
            .upsert_discovered_node(sample_node("bbbbbbbbbbbb", 2))
            .await;
        // At capacity: adding a third node must evict the oldest (prefix "aa...").
        state
            .upsert_discovered_node(sample_node("cccccccccccc", 3))
            .await;

        let discovered = state.discovered_repeaters.read().await;
        assert_eq!(discovered.len(), 2);
        assert!(!discovered.contains_key("aaaaaaaaaaaa"));
        assert!(discovered.contains_key("bbbbbbbbbbbb"));
        assert!(discovered.contains_key("cccccccccccc"));
    }

    #[tokio::test]
    async fn upsert_discovered_node_refreshing_an_existing_entry_does_not_evict() {
        let state = make_state_with_discovered_capacity(2);

        state
            .upsert_discovered_node(sample_node("aaaaaaaaaaaa", 1))
            .await;
        state
            .upsert_discovered_node(sample_node("bbbbbbbbbbbb", 2))
            .await;
        // Already at capacity, but this just refreshes an existing entry.
        state
            .upsert_discovered_node(sample_node("aaaaaaaaaaaa", 3))
            .await;

        let discovered = state.discovered_repeaters.read().await;
        assert_eq!(discovered.len(), 2);
        assert_eq!(discovered["aaaaaaaaaaaa"].last_seen_unix, 3);
    }

    #[test]
    fn discovered_repeaters_capacity_is_at_least_one() {
        let state = make_state_with_discovered_capacity(0);
        assert_eq!(state.discovered_repeaters_capacity, 1);
    }
}
