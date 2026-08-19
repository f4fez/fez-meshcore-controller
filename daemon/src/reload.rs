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

//! Reloads `config.toml` on `SIGHUP`, without a full daemon restart.
//!
//! Only a subset of `Config` can be swapped in safely: `managed_repeaters`,
//! `regions` and `hashtag_channels` are already read live from
//! `AppState::config` wherever they're consumed (`mesh_task`/`server`), so
//! replacing the in-memory config is enough to apply them -- this also
//! covers `ManagedRepeater::status` (the managed/known/supervised tier)
//! automatically: no dedicated handling was needed when that field was
//! added, since the whole `Vec<ManagedRepeater>` is swapped in wholesale
//! regardless of which fields on each entry changed. `mqtt_brokers` is
//! handled explicitly here -- added/removed/changed brokers get their
//! worker task spawned/aborted. Everything else (the node connection, IPC
//! socket path, DB path, packet-log capacity, refresh interval, log
//! level/dir) is bound once at daemon startup; [`diff_config`] detects and
//! [`reload_once`] logs when one of those changed on disk, but doesn't
//! apply it -- a full restart is still required for those.

use std::sync::Arc;

use fez_mesh_controller_core::config::MqttBrokerConfig;
use fez_mesh_controller_core::ipc::MeshEvent;
use fez_mesh_controller_core::mesh::MeshEventKind;
use fez_mesh_controller_core::Config;
use tokio::signal::unix::{signal, SignalKind};
use tracing::{error, info, warn};

use crate::command::DaemonCommand;
use crate::state::AppState;

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Result of comparing an old and new [`Config`]: which MQTT brokers need
/// their worker task started/stopped/restarted, and which changed fields
/// aren't applied live and require a full daemon restart.
#[derive(Debug, Default, PartialEq)]
pub struct ConfigDiff {
    /// Human-readable "<field>: <old> -> <new>" for each changed field that
    /// isn't applied live.
    pub restart_required: Vec<String>,
    pub mqtt_added: Vec<MqttBrokerConfig>,
    pub mqtt_removed: Vec<String>,
    pub mqtt_changed: Vec<MqttBrokerConfig>,
}

/// Compares `old` and `new`. Pure/no I/O so it's exhaustively unit
/// testable; `reload_once` is the only caller that acts on the result.
pub fn diff_config(old: &Config, new: &Config) -> ConfigDiff {
    let mut restart_required = Vec::new();

    if old.connection != new.connection {
        restart_required.push(format!(
            "connection: {} -> {}",
            old.connection, new.connection
        ));
    }
    if old.daemon.socket_path != new.daemon.socket_path {
        restart_required.push(format!(
            "daemon.socket_path: {} -> {}",
            old.daemon.socket_path.display(),
            new.daemon.socket_path.display()
        ));
    }
    if old.daemon.refresh_interval_secs != new.daemon.refresh_interval_secs {
        restart_required.push(format!(
            "daemon.refresh_interval_secs: {} -> {}",
            old.daemon.refresh_interval_secs, new.daemon.refresh_interval_secs
        ));
    }
    if old.daemon.log_level != new.daemon.log_level {
        restart_required.push(format!(
            "daemon.log_level: {} -> {}",
            old.daemon.log_level, new.daemon.log_level
        ));
    }
    if old.daemon.log_dir != new.daemon.log_dir {
        restart_required.push(format!(
            "daemon.log_dir: {} -> {}",
            old.daemon.log_dir.display(),
            new.daemon.log_dir.display()
        ));
    }
    if old.daemon.packet_log_capacity != new.daemon.packet_log_capacity {
        restart_required.push(format!(
            "daemon.packet_log_capacity: {} -> {}",
            old.daemon.packet_log_capacity, new.daemon.packet_log_capacity
        ));
    }
    if old.daemon.db_path != new.daemon.db_path {
        restart_required.push(format!(
            "daemon.db_path: {} -> {}",
            old.daemon.db_path.display(),
            new.daemon.db_path.display()
        ));
    }

    let mut mqtt_added = Vec::new();
    let mut mqtt_changed = Vec::new();
    for broker in &new.mqtt_brokers {
        match old.mqtt_brokers.iter().find(|b| b.name == broker.name) {
            None => mqtt_added.push(broker.clone()),
            Some(old_broker) if old_broker != broker => mqtt_changed.push(broker.clone()),
            Some(_) => {}
        }
    }
    let mqtt_removed = old
        .mqtt_brokers
        .iter()
        .filter(|b| !new.mqtt_brokers.iter().any(|nb| nb.name == b.name))
        .map(|b| b.name.clone())
        .collect();

    ConfigDiff {
        restart_required,
        mqtt_added,
        mqtt_removed,
        mqtt_changed,
    }
}

/// Reloads `config.toml` from `state.config_path`, applies what can be
/// applied live, logs what can't, and broadcasts
/// [`MeshEventKind::ConfigReloaded`] so IPC clients refresh. Leaves
/// `state.config` untouched on a parse/IO error.
pub async fn reload_once(state: &Arc<AppState>) {
    let new_config = match Config::load_from(&state.config_path) {
        Ok(config) => config,
        Err(err) => {
            error!(
                error = %err,
                path = %state.config_path.display(),
                "failed to reload config, keeping the running config"
            );
            return;
        }
    };

    let old_config = state.config.read().await.clone();
    let diff = diff_config(&old_config, &new_config);

    *state.config.write().await = new_config.clone();

    for name in &diff.mqtt_removed {
        if let Some(handle) = state.mqtt_broker_tasks.lock().await.remove(name) {
            handle.abort();
        }
        state.mqtt_broker_status.write().await.remove(name);
        info!(broker = %name, "MQTT broker removed on config reload");
    }
    for broker in diff.mqtt_changed.iter().chain(diff.mqtt_added.iter()) {
        if let Some(handle) = state.mqtt_broker_tasks.lock().await.remove(&broker.name) {
            handle.abort();
        }
        let handle = crate::mqtt::spawn(state.clone(), broker.clone());
        state
            .mqtt_broker_tasks
            .lock()
            .await
            .insert(broker.name.clone(), handle);
        info!(broker = %broker.name, "MQTT broker (re)started on config reload");
    }

    for field in &diff.restart_required {
        warn!(
            %field,
            "config field changed on disk but requires a daemon restart to take effect"
        );
    }

    let observer_resynced = new_config.daemon.observer_node_managed_config;
    if observer_resynced {
        let _ = state
            .command_tx
            .send(DaemonCommand::ResyncObserverConfig)
            .await;
    }

    let summary = format!(
        "managed_repeaters={}, regions={}, hashtag_channels={}, mqtt +{}/-{}/~{}, observer_resynced={}, restart_required={}",
        new_config.managed_repeaters.len(),
        new_config.regions.len(),
        new_config.hashtag_channels.len(),
        diff.mqtt_added.len(),
        diff.mqtt_removed.len(),
        diff.mqtt_changed.len(),
        observer_resynced,
        diff.restart_required.len(),
    );
    info!(%summary, "config reloaded");
    state.broadcast_event(MeshEvent {
        at_unix: now_unix(),
        kind: MeshEventKind::ConfigReloaded { summary },
    });
}

/// Listens for `SIGHUP` and reloads the config each time one is received,
/// for the daemon's entire lifetime. Runs as its own fire-and-forget task
/// rather than a branch of `run()`'s shutdown `select!` (`main.rs`), since
/// that select ends the daemon on whichever branch completes first and
/// `SIGHUP` must be handled repeatedly, not just once.
pub async fn watch(state: Arc<AppState>) {
    let mut hangup = match signal(SignalKind::hangup()) {
        Ok(sig) => sig,
        Err(err) => {
            error!(
                error = %err,
                "failed to install SIGHUP handler, config reload via signal is unavailable"
            );
            return;
        }
    };
    loop {
        hangup.recv().await;
        info!("SIGHUP received, reloading config");
        reload_once(&state).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fez_mesh_controller_core::config::{MqttAuthMethod, MqttTransportProtocol};
    use fez_mesh_controller_core::mesh::MeshEventKind;
    use fez_mesh_controller_core::{
        ConnectionConfig, DaemonConfig, ManagedRepeater, RegionConfig, RepeaterStatus,
    };
    use std::path::PathBuf;
    use tokio::sync::mpsc;

    fn sample_config() -> Config {
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
                packet_log_capacity: 500,
                db_path: PathBuf::from(":memory:"),
                observer_node_managed_config: false,
            },
            managed_repeaters: vec![],
            regions: vec![],
            hashtag_channels: vec![],
            mqtt_brokers: vec![],
        }
    }

    fn sample_broker(name: &str, host: &str) -> MqttBrokerConfig {
        MqttBrokerConfig {
            name: name.to_string(),
            host: host.to_string(),
            port: 1883,
            username: None,
            password: None,
            auth_method: MqttAuthMethod::Passwd,
            jwt_ttl_secs: 3600,
            jwt_audience: None,
            topic_prefix: "meshcore".to_string(),
            tls_enabled: false,
            tls_ca_cert: None,
            tls_client_cert: None,
            tls_client_key: None,
            status_refresh_interval_secs: 300,
            enable_high_level_messages: true,
            enable_packet_trafic_messages: true,
            packet_trafic_topic: "{prefix}/packets".to_string(),
            enable_raw_messages: false,
            raw_topic: "{prefix}/raw".to_string(),
            status_topic: "{prefix}/status".to_string(),
            transport_protocol: MqttTransportProtocol::Tcp,
            websocket_path: "/mqtt".to_string(),
        }
    }

    async fn make_state(config: Config, config_path: PathBuf) -> Arc<AppState> {
        let (command_tx, _command_rx) = mpsc::channel(8);
        Arc::new(
            AppState::new(command_tx, config, config_path)
                .await
                .expect("AppState::new with an in-memory DB should never fail"),
        )
    }

    #[test]
    fn diff_config_no_changes_is_empty() {
        let config = sample_config();
        assert_eq!(diff_config(&config, &config), ConfigDiff::default());
    }

    #[test]
    fn diff_config_flags_each_restart_required_field() {
        let old = sample_config();

        let mut new = old.clone();
        new.connection = ConnectionConfig::Ble {
            name: "other".to_string(),
        };
        assert_eq!(diff_config(&old, &new).restart_required.len(), 1);

        let mut new = old.clone();
        new.daemon.socket_path = PathBuf::from("/tmp/other.sock");
        assert_eq!(diff_config(&old, &new).restart_required.len(), 1);

        let mut new = old.clone();
        new.daemon.refresh_interval_secs = 30;
        assert_eq!(diff_config(&old, &new).restart_required.len(), 1);

        let mut new = old.clone();
        new.daemon.log_level = "debug".to_string();
        assert_eq!(diff_config(&old, &new).restart_required.len(), 1);

        let mut new = old.clone();
        new.daemon.log_dir = PathBuf::from("/tmp/other-logs");
        assert_eq!(diff_config(&old, &new).restart_required.len(), 1);

        let mut new = old.clone();
        new.daemon.packet_log_capacity = 1000;
        assert_eq!(diff_config(&old, &new).restart_required.len(), 1);

        let mut new = old.clone();
        new.daemon.db_path = PathBuf::from("/tmp/other.sqlite3");
        assert_eq!(diff_config(&old, &new).restart_required.len(), 1);
    }

    #[test]
    fn diff_config_does_not_flag_live_fields() {
        let old = sample_config();
        let mut new = old.clone();
        new.managed_repeaters.push(ManagedRepeater {
            name: "Repeater".to_string(),
            public_key_hex: "aa".repeat(32),
            password: None,
            status: RepeaterStatus::Managed,
        });
        new.regions.push(RegionConfig {
            name: "Region".to_string(),
            parent: None,
        });
        new.hashtag_channels.push("#test".to_string());
        assert!(diff_config(&old, &new).restart_required.is_empty());
    }

    #[test]
    fn diff_config_detects_added_removed_changed_brokers() {
        let mut old = sample_config();
        old.mqtt_brokers = vec![sample_broker("Home", "old.example.com")];
        let mut new = sample_config();
        new.mqtt_brokers = vec![
            sample_broker("Home", "new.example.com"),
            sample_broker("Backup", "backup.example.com"),
        ];

        let diff = diff_config(&old, &new);
        assert_eq!(diff.mqtt_added.len(), 1);
        assert_eq!(diff.mqtt_added[0].name, "Backup");
        assert_eq!(diff.mqtt_changed.len(), 1);
        assert_eq!(diff.mqtt_changed[0].name, "Home");
        assert!(diff.mqtt_removed.is_empty());
    }

    #[test]
    fn diff_config_detects_removed_broker() {
        let mut old = sample_config();
        old.mqtt_brokers = vec![sample_broker("Home", "old.example.com")];
        let new = sample_config();

        let diff = diff_config(&old, &new);
        assert_eq!(diff.mqtt_removed, vec!["Home".to_string()]);
        assert!(diff.mqtt_added.is_empty());
        assert!(diff.mqtt_changed.is_empty());
    }

    #[test]
    fn diff_config_leaves_unchanged_broker_alone() {
        let mut old = sample_config();
        old.mqtt_brokers = vec![sample_broker("Home", "example.com")];
        let new = old.clone();

        let diff = diff_config(&old, &new);
        assert!(diff.mqtt_added.is_empty());
        assert!(diff.mqtt_removed.is_empty());
        assert!(diff.mqtt_changed.is_empty());
    }

    #[tokio::test]
    async fn reload_once_swaps_config_and_broadcasts_event() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        let initial = sample_config();
        initial.save_to(&config_path).unwrap();
        let state = make_state(initial, config_path.clone()).await;

        let mut updated = sample_config();
        updated.managed_repeaters.push(ManagedRepeater {
            name: "Repeater".to_string(),
            public_key_hex: "bb".repeat(32),
            password: None,
            status: RepeaterStatus::Managed,
        });
        updated.save_to(&config_path).unwrap();

        let mut events_rx = state.events_tx.subscribe();
        reload_once(&state).await;

        assert_eq!(state.config.read().await.managed_repeaters.len(), 1);
        let event = events_rx.try_recv().expect("a ConfigReloaded event");
        assert!(matches!(event.kind, MeshEventKind::ConfigReloaded { .. }));
    }

    #[tokio::test]
    async fn reload_once_keeps_running_config_on_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        let initial = sample_config();
        initial.save_to(&config_path).unwrap();
        let state = make_state(initial, config_path.clone()).await;

        std::fs::write(&config_path, "not valid toml {{{").unwrap();

        let mut events_rx = state.events_tx.subscribe();
        reload_once(&state).await;

        assert_eq!(state.config.read().await.node_label, "test-node");
        assert!(events_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn reload_once_resyncs_observer_only_when_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        let mut config = sample_config();
        config.daemon.observer_node_managed_config = false;
        config.save_to(&config_path).unwrap();

        let (command_tx, mut command_rx) = mpsc::channel(8);
        let state = Arc::new(
            AppState::new(command_tx, config, config_path.clone())
                .await
                .unwrap(),
        );

        reload_once(&state).await;
        assert!(command_rx.try_recv().is_err());

        let mut enabled = sample_config();
        enabled.daemon.observer_node_managed_config = true;
        enabled.save_to(&config_path).unwrap();

        reload_once(&state).await;
        assert!(matches!(
            command_rx.try_recv(),
            Ok(DaemonCommand::ResyncObserverConfig)
        ));
    }
}
