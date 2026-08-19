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

use std::collections::hash_map::Entry;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use fez_mesh_controller_core::ipc::{MeshEvent, MqttBrokerStatus, Snapshot};
use fez_mesh_controller_core::mesh::{
    DeviceInfoDto, DiscoveredNode, NodeStatsDto, PacketLogEntry, TelemetryDto,
};
use fez_mesh_controller_core::Config;
use meshcore_rs::MeshCoreEvent;
use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::warn;

use crate::command::DaemonCommand;
use crate::repeater_db::RepeaterDb;

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
    /// carries the full public key needed to register them. This is a
    /// write-through, startup-hydrated mirror of [`Self::repeater_db`] (the
    /// actual source of truth) — see [`Self::upsert_discovered_node`].
    pub discovered_repeaters: RwLock<HashMap<String, DiscoveredNode>>,
    /// Persists [`Self::discovered_repeaters`] in SQLite, surviving daemon
    /// restarts.
    pub repeater_db: RepeaterDb,
    /// Rotating cache of raw packets (newest first), for the TUI's packet
    /// log page. Bounded to `packet_log_capacity` entries.
    pub packet_log: RwLock<VecDeque<PacketLogEntry>>,
    pub packet_log_capacity: usize,
    pub packet_log_tx: broadcast::Sender<PacketLogEntry>,
    next_packet_id: AtomicU64,
    /// Every raw event the mesh connection receives, broadcast for any
    /// number of MQTT broker worker tasks (see `crate::mqtt`) to forward.
    /// `Arc`-wrapped since `MeshCoreEvent` can be non-trivial and each
    /// configured broker holds its own clone of the receiver.
    pub raw_events_tx: broadcast::Sender<Arc<MeshCoreEvent>>,
    /// Live connection status of each configured MQTT broker, keyed by
    /// [`fez_mesh_controller_core::config::MqttBrokerConfig::name`] —
    /// updated by that broker's own worker task, read by
    /// [`crate::server::run`] to populate [`Snapshot::mqtt_brokers`].
    pub mqtt_broker_status: RwLock<HashMap<String, MqttBrokerStatus>>,
    /// Running broker worker tasks (see `crate::mqtt::spawn`), keyed the
    /// same way as [`Self::mqtt_broker_status`] — lets a config reload
    /// (`crate::reload`) abort a removed/changed broker's task and spawn
    /// its replacement.
    pub mqtt_broker_tasks: tokio::sync::Mutex<HashMap<String, tokio::task::AbortHandle>>,
    /// The connected device's model/firmware info, queried once per
    /// connection (see `crate::mesh_task::run`) — consumed by
    /// `crate::mqtt`'s status message, not exposed over IPC.
    pub device_info: RwLock<Option<DeviceInfoDto>>,
    /// Last-known core/radio/packet stats, queried once per connection and
    /// refreshed on demand (see `DaemonCommand::RefreshNodeStats`) — read
    /// by `crate::mqtt`'s status message and exposed over IPC via
    /// [`Snapshot::node_stats`].
    pub node_stats: RwLock<Option<NodeStatsDto>>,
    /// Last telemetry fetched on demand from each contact (see
    /// `DaemonCommand::RequestTelemetry`), keyed by public key prefix
    /// (hex) — read back into `ContactDto::last_telemetry` by
    /// `crate::mesh_task::build_snapshot_contacts`.
    pub telemetry: RwLock<HashMap<String, TelemetryDto>>,
}

impl AppState {
    /// Opens/migrates the SQLite repeater DB and hydrates
    /// [`Self::discovered_repeaters`] from it before returning, so every
    /// previously-heard repeater/room-server is already known immediately
    /// at startup, before the mesh connection even completes.
    pub async fn new(
        command_tx: mpsc::Sender<DaemonCommand>,
        config: Config,
        config_path: PathBuf,
    ) -> rusqlite::Result<Self> {
        let (events_tx, _rx) = broadcast::channel(256);
        let (packet_log_tx, _rx) = broadcast::channel(256);
        let (raw_events_tx, _rx) = broadcast::channel(256);
        let packet_log_capacity = config.daemon.packet_log_capacity.max(1);
        let repeater_db = RepeaterDb::open(config.daemon.db_path.clone()).await?;
        let discovered_repeaters = repeater_db.load_all().await?;
        Ok(Self {
            snapshot: RwLock::new(Snapshot::default()),
            events_tx,
            started_at: Instant::now(),
            command_tx,
            config: RwLock::new(config),
            config_path,
            discovered_repeaters: RwLock::new(discovered_repeaters),
            repeater_db,
            packet_log: RwLock::new(VecDeque::with_capacity(packet_log_capacity)),
            packet_log_capacity,
            packet_log_tx,
            next_packet_id: AtomicU64::new(1),
            raw_events_tx,
            mqtt_broker_status: RwLock::new(HashMap::new()),
            mqtt_broker_tasks: tokio::sync::Mutex::new(HashMap::new()),
            device_info: RwLock::new(None),
            node_stats: RwLock::new(None),
            telemetry: RwLock::new(HashMap::new()),
        })
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Broadcasts an event to connected IPC clients (silently dropped if
    /// nobody is listening).
    pub fn broadcast_event(&self, event: MeshEvent) {
        let _ = self.events_tx.send(event);
    }

    /// Broadcasts a raw mesh event to any MQTT broker worker tasks
    /// (silently dropped if none are configured/subscribed).
    pub fn broadcast_raw_event(&self, event: Arc<MeshCoreEvent>) {
        let _ = self.raw_events_tx.send(event);
    }

    /// Records a configured MQTT broker's current connection status, read
    /// back by [`crate::server::run`] to populate
    /// [`fez_mesh_controller_core::ipc::Snapshot::mqtt_brokers`].
    pub async fn set_mqtt_broker_status(&self, name: &str, status: MqttBrokerStatus) {
        self.mqtt_broker_status
            .write()
            .await
            .insert(name.to_string(), status);
    }

    /// Records a contact's last-fetched telemetry, read back into
    /// `ContactDto::last_telemetry` by `crate::mesh_task::build_snapshot_contacts`.
    pub async fn set_telemetry(&self, public_key_prefix_hex: &str, telemetry: TelemetryDto) {
        self.telemetry
            .write()
            .await
            .insert(public_key_prefix_hex.to_string(), telemetry);
    }

    /// Records the connected device's model/firmware info, read back by
    /// `crate::mqtt` when building the MQTT status message.
    pub async fn set_device_info(&self, info: Option<DeviceInfoDto>) {
        *self.device_info.write().await = info;
    }

    /// Records the node's latest stats, read back by `crate::mqtt`'s
    /// status message and [`crate::server::run`]'s `Snapshot` population.
    pub async fn set_node_stats(&self, stats: NodeStatsDto) {
        *self.node_stats.write().await = Some(stats);
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

    /// Records a newly-overheard node: persists to [`Self::repeater_db`]
    /// first (write-through -- logged but not fatal on failure, the
    /// in-memory mirror must stay usable even if the DB write fails, e.g.
    /// disk full), then merges it into [`Self::discovered_repeaters`]
    /// (matching the DB's own "don't erase a previously-known position
    /// with a sighting that didn't carry one" semantics, see
    /// [`RepeaterDb::upsert`]) rather than blindly overwriting the whole
    /// entry. Returns whether this is a genuinely new node, not just an
    /// updated last-seen timestamp for one already tracked.
    pub async fn upsert_discovered_node(&self, node: DiscoveredNode) -> bool {
        if let Err(err) = self.repeater_db.upsert(&node).await {
            warn!(error = %err, "failed to persist discovered node to the repeater database");
        }

        let mut discovered = self.discovered_repeaters.write().await;
        match discovered.entry(node.public_key_prefix_hex.clone()) {
            Entry::Occupied(mut entry) => {
                let existing = entry.get_mut();
                existing.name = node.name;
                existing.public_key_hex = node.public_key_hex;
                existing.adv_type = node.adv_type;
                existing.is_repeater = node.is_repeater;
                if node.lat != 0.0 || node.lon != 0.0 {
                    existing.lat = node.lat;
                    existing.lon = node.lon;
                }
                existing.last_snr = node.last_snr.or(existing.last_snr);
                existing.last_rssi = node.last_rssi.or(existing.last_rssi);
                existing.last_hop_count = node.last_hop_count.or(existing.last_hop_count);
                existing.last_seen_unix = node.last_seen_unix;
                false
            }
            Entry::Vacant(entry) => {
                entry.insert(node);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fez_mesh_controller_core::mesh::MeshEventKind;
    use fez_mesh_controller_core::{Config, ConnectionConfig, DaemonConfig};

    fn sample_config(packet_log_capacity: usize, db_path: PathBuf) -> Config {
        Config {
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
                db_path,
                observer_node_managed_config: true,
            },
            managed_repeaters: vec![],
            regions: vec![],
            hashtag_channels: vec![],
            mqtt_brokers: vec![],
        }
    }

    /// Each call gets its own isolated in-memory DB (`:memory:` opens a
    /// fresh, private database per connection) -- no cross-test state, no
    /// filesystem cleanup needed.
    async fn make_state(packet_log_capacity: usize) -> AppState {
        let (command_tx, _command_rx) = mpsc::channel(1);
        let config = sample_config(packet_log_capacity, PathBuf::from(":memory:"));
        AppState::new(
            command_tx,
            config,
            PathBuf::from("/tmp/fez-mesh-controller-test.toml"),
        )
        .await
        .expect("AppState::new with an in-memory DB should never fail")
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
            last_snr: None,
            last_rssi: None,
            last_hop_count: None,
        }
    }

    #[tokio::test]
    async fn next_packet_id_increments_starting_at_one() {
        let state = make_state(500).await;
        assert_eq!(state.next_packet_id(), 1);
        assert_eq!(state.next_packet_id(), 2);
        assert_eq!(state.next_packet_id(), 3);
    }

    #[tokio::test]
    async fn packet_log_capacity_is_at_least_one() {
        // A misconfigured `packet_log_capacity = 0` must not make the cache
        // unusable (and `VecDeque::with_capacity(0)` would be fine, but a
        // capacity of 0 in `record_packet`'s eviction check would drop
        // every entry immediately).
        let (command_tx, _rx) = mpsc::channel(1);
        let config = sample_config(0, PathBuf::from(":memory:"));
        let state = AppState::new(command_tx, config, PathBuf::from("/tmp/x.toml"))
            .await
            .expect("AppState::new with an in-memory DB should never fail");
        assert_eq!(state.packet_log_capacity, 1);
    }

    #[tokio::test]
    async fn app_state_new_hydrates_discovered_repeaters_from_the_db_path() {
        // Unlike the other tests, this one needs a real file (not
        // `:memory:`) since it opens the same DB twice, across two
        // separate `AppState::new` calls, to prove persistence.
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("repeaters.sqlite3");

        let (command_tx, _rx) = mpsc::channel(1);
        let first = AppState::new(
            command_tx,
            sample_config(500, db_path.clone()),
            PathBuf::from("/tmp/x.toml"),
        )
        .await
        .expect("first AppState::new");
        first
            .upsert_discovered_node(sample_node("aaaaaaaaaaaa", 1))
            .await;

        let (command_tx2, _rx2) = mpsc::channel(1);
        let second = AppState::new(
            command_tx2,
            sample_config(500, db_path),
            PathBuf::from("/tmp/x.toml"),
        )
        .await
        .expect("second AppState::new, same db_path");

        let discovered = second.discovered_repeaters.read().await;
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered["aaaaaaaaaaaa"].last_seen_unix, 1);
    }

    #[tokio::test]
    async fn record_packet_keeps_newest_first_up_to_capacity() {
        let state = make_state(2).await;

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
        let state = make_state(500).await;
        let mut rx = state.packet_log_tx.subscribe();

        state.record_packet(sample_packet(1)).await;

        let received = rx.try_recv().expect("should have received the packet");
        assert_eq!(received.id, 1);
    }

    #[tokio::test]
    async fn broadcast_event_without_subscribers_does_not_panic() {
        let state = make_state(500).await;
        state.broadcast_event(MeshEvent {
            at_unix: 0,
            kind: MeshEventKind::Connected,
        });
    }

    fn sample_raw_event() -> MeshCoreEvent {
        MeshCoreEvent {
            event_type: meshcore_rs::EventType::Connected,
            payload: meshcore_rs::EventPayload::None,
            attributes: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn broadcast_raw_event_without_subscribers_does_not_panic() {
        let state = make_state(500).await;
        state.broadcast_raw_event(Arc::new(sample_raw_event()));
    }

    #[tokio::test]
    async fn broadcast_raw_event_reaches_subscribers() {
        let state = make_state(500).await;
        let mut rx = state.raw_events_tx.subscribe();

        state.broadcast_raw_event(Arc::new(sample_raw_event()));

        let received = rx.try_recv().expect("should have received the event");
        assert_eq!(received.event_type, meshcore_rs::EventType::Connected);
    }

    #[tokio::test]
    async fn set_mqtt_broker_status_updates_and_overwrites() {
        let state = make_state(500).await;

        state
            .set_mqtt_broker_status("Home Assistant", MqttBrokerStatus::Connecting)
            .await;
        assert_eq!(
            state.mqtt_broker_status.read().await["Home Assistant"],
            MqttBrokerStatus::Connecting
        );

        state
            .set_mqtt_broker_status("Home Assistant", MqttBrokerStatus::Connected)
            .await;
        assert_eq!(
            state.mqtt_broker_status.read().await["Home Assistant"],
            MqttBrokerStatus::Connected
        );
    }

    #[tokio::test]
    async fn set_telemetry_updates_and_overwrites() {
        let state = make_state(500).await;
        let reading = |value: f64| fez_mesh_controller_core::telemetry::TelemetryReading {
            channel: 1,
            label: "Voltage".to_string(),
            value,
            unit: "V".to_string(),
        };

        state
            .set_telemetry(
                "aabbccddeeff",
                TelemetryDto {
                    fetched_at_unix: 1,
                    readings: vec![reading(3.7)],
                },
            )
            .await;
        assert_eq!(
            state.telemetry.read().await["aabbccddeeff"].readings[0].value,
            3.7
        );

        state
            .set_telemetry(
                "aabbccddeeff",
                TelemetryDto {
                    fetched_at_unix: 2,
                    readings: vec![reading(3.6)],
                },
            )
            .await;
        assert_eq!(
            state.telemetry.read().await["aabbccddeeff"].readings[0].value,
            3.6
        );
    }

    #[tokio::test]
    async fn set_device_info_updates_and_overwrites() {
        let state = make_state(500).await;
        assert!(state.device_info.read().await.is_none());

        state
            .set_device_info(Some(DeviceInfoDto {
                model: "Seeed Xiao-nrf52".to_string(),
                firmware_version: "v1.16.0 (Build: 06-Jun-2026)".to_string(),
            }))
            .await;
        assert_eq!(
            state.device_info.read().await.as_ref().unwrap().model,
            "Seeed Xiao-nrf52"
        );

        state.set_device_info(None).await;
        assert!(state.device_info.read().await.is_none());
    }

    #[tokio::test]
    async fn upsert_discovered_node_reports_new_vs_already_known() {
        let state = make_state(500).await;

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
    async fn upsert_discovered_node_refreshing_an_existing_entry_updates_last_seen() {
        let state = make_state(500).await;

        state
            .upsert_discovered_node(sample_node("aaaaaaaaaaaa", 1))
            .await;
        state
            .upsert_discovered_node(sample_node("bbbbbbbbbbbb", 2))
            .await;
        state
            .upsert_discovered_node(sample_node("aaaaaaaaaaaa", 3))
            .await;

        let discovered = state.discovered_repeaters.read().await;
        assert_eq!(discovered.len(), 2);
        assert_eq!(discovered["aaaaaaaaaaaa"].last_seen_unix, 3);
    }

    #[tokio::test]
    async fn upsert_discovered_node_does_not_clear_a_known_position_with_a_positionless_sighting() {
        let state = make_state(500).await;
        let mut with_position = sample_node("aaaaaaaaaaaa", 1);
        with_position.lat = 48.85;
        with_position.lon = 2.35;
        state.upsert_discovered_node(with_position).await;

        // Same node seen again, but this sighting's advert didn't carry a
        // position (lat/lon both 0.0, `DiscoveredNode`'s own "unknown"
        // sentinel) -- must not erase the previously-known one.
        state
            .upsert_discovered_node(sample_node("aaaaaaaaaaaa", 2))
            .await;

        let discovered = state.discovered_repeaters.read().await;
        assert_eq!(discovered["aaaaaaaaaaaa"].lat, 48.85);
        assert_eq!(discovered["aaaaaaaaaaaa"].lon, 2.35);
        assert_eq!(discovered["aaaaaaaaaaaa"].last_seen_unix, 2);
    }

    #[tokio::test]
    async fn upsert_discovered_node_keeps_previous_signal_fields_when_a_sighting_omits_them() {
        let state = make_state(500).await;
        let mut first = sample_node("aaaaaaaaaaaa", 1);
        first.last_snr = Some(4.0);
        first.last_rssi = Some(-90);
        first.last_hop_count = Some(2);
        state.upsert_discovered_node(first).await;

        // A later sighting without decoded signal/hop data must not blank
        // out the previously-known values.
        state
            .upsert_discovered_node(sample_node("aaaaaaaaaaaa", 2))
            .await;

        let discovered = state.discovered_repeaters.read().await;
        let node = &discovered["aaaaaaaaaaaa"];
        assert_eq!(node.last_snr, Some(4.0));
        assert_eq!(node.last_rssi, Some(-90));
        assert_eq!(node.last_hop_count, Some(2));
        assert_eq!(node.last_seen_unix, 2);
    }
}
