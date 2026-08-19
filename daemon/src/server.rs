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

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use fez_mesh_controller_core::ipc::{ClientMessage, ServerMessage, PROTOCOL_VERSION};
use futures::{SinkExt, StreamExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};
use tracing::{debug, info, warn};

use crate::command::DaemonCommand;
use crate::state::AppState;

/// How long an IPC client waits for a command's outcome before giving up
/// (e.g. the mesh connection task is stuck reconnecting).
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
/// A telemetry request involves a login plus a real multi-hop mesh round
/// trip -- far slower than the companion-local commands `COMMAND_TIMEOUT`
/// was sized for.
const TELEMETRY_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
/// A repeater-detail request is a login plus four *sequential* mesh round
/// trips (status, telemetry, neighbours, regions -- see
/// `mesh_task::request_repeater_detail`): three with a 10s budget each
/// (`core::mesh::MESH_REQUEST_TIMEOUT`) plus the region-hierarchy fetch's
/// own 15s reply wait (no tag correlation, see
/// `MeshClient::request_region_hierarchy`); 90s covers that worst case
/// (login ~20s + 3*10s + 15s + logout) with margin. Categories stream back
/// progressively (see [`dispatch_repeater_detail`]), so in practice this is
/// only ever hit as a safety net if the mesh task goes silent mid-fetch —
/// it isn't how long the user actually waits for the first result.
const REPEATER_DETAIL_COMMAND_TIMEOUT: Duration = Duration::from_secs(90);

/// Starts the IPC server: listens on the Unix socket and serves one client
/// per accepted connection until the daemon shuts down.
pub async fn run(socket_path: &Path, state: Arc<AppState>) -> Result<()> {
    if socket_path.exists() {
        std::fs::remove_file(socket_path)
            .with_context(|| format!("removing existing socket {}", socket_path.display()))?;
    }
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }

    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("binding socket {}", socket_path.display()))?;
    info!(socket = %socket_path.display(), "IPC server listening");

    loop {
        let (stream, _addr) = listener.accept().await.context("accepting IPC client")?;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_client(stream, state).await {
                debug!(error = %err, "IPC client connection closed");
            }
        });
    }
}

async fn handle_client(stream: UnixStream, state: Arc<AppState>) -> Result<()> {
    let (read_half, write_half) = stream.into_split();
    let mut reader = FramedRead::new(read_half, LinesCodec::new());
    let mut writer = FramedWrite::new(write_half, LinesCodec::new());
    let mut events_rx = state.events_tx.subscribe();
    let mut packet_log_rx = state.packet_log_tx.subscribe();

    send(
        &mut writer,
        &ServerMessage::Hello {
            version: PROTOCOL_VERSION,
        },
    )
    .await?;
    send(
        &mut writer,
        &ServerMessage::Snapshot(current_snapshot(&state).await),
    )
    .await?;
    send(
        &mut writer,
        &ServerMessage::PacketLog(state.packet_log.read().await.iter().cloned().collect()),
    )
    .await?;

    loop {
        tokio::select! {
            event = events_rx.recv() => {
                match event {
                    Ok(event) => send(&mut writer, &ServerMessage::Event(event)).await?,
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(skipped, "IPC client too slow, events dropped");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            entry = packet_log_rx.recv() => {
                match entry {
                    Ok(entry) => {
                        send(&mut writer, &ServerMessage::PacketLogEntry(Box::new(entry))).await?
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(skipped, "IPC client too slow, packet log entries dropped");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            line = reader.next() => {
                match line {
                    Some(Ok(text)) => match serde_json::from_str::<ClientMessage>(&text) {
                        Ok(ClientMessage::RequestSnapshot) => {
                            send(&mut writer, &ServerMessage::Snapshot(current_snapshot(&state).await)).await?;
                        }
                        Ok(ClientMessage::RemoveContact { public_key_prefix_hex }) => {
                            let (reply, reply_rx) = oneshot::channel();
                            let cmd = DaemonCommand::RemoveContact { public_key_prefix_hex, reply };
                            if let Err(reason) = dispatch_command(&state, cmd, reply_rx, COMMAND_TIMEOUT).await {
                                send(&mut writer, &ServerMessage::Error(reason)).await?;
                            }
                        }
                        Ok(ClientMessage::SetRepeaterStatus { public_key_prefix_hex, name, status }) => {
                            let (reply, reply_rx) = oneshot::channel();
                            let cmd = DaemonCommand::SetRepeaterStatus { public_key_prefix_hex, name, status, reply };
                            if let Err(reason) = dispatch_command(&state, cmd, reply_rx, COMMAND_TIMEOUT).await {
                                send(&mut writer, &ServerMessage::Error(reason)).await?;
                            }
                        }
                        Ok(ClientMessage::AddRepeater { public_key_hex, name, managed }) => {
                            let (reply, reply_rx) = oneshot::channel();
                            let cmd = DaemonCommand::AddRepeater { public_key_hex, name, managed, reply };
                            if let Err(reason) = dispatch_command(&state, cmd, reply_rx, COMMAND_TIMEOUT).await {
                                send(&mut writer, &ServerMessage::Error(reason)).await?;
                            }
                        }
                        Ok(ClientMessage::RequestTelemetry { public_key_prefix_hex }) => {
                            let (reply, reply_rx) = oneshot::channel();
                            let cmd = DaemonCommand::RequestTelemetry { public_key_prefix_hex: public_key_prefix_hex.clone(), reply };
                            match dispatch_command(&state, cmd, reply_rx, TELEMETRY_COMMAND_TIMEOUT).await {
                                Ok(telemetry) => {
                                    send(&mut writer, &ServerMessage::TelemetryResult { public_key_prefix_hex, telemetry }).await?;
                                }
                                Err(reason) => {
                                    send(&mut writer, &ServerMessage::Error(reason)).await?;
                                }
                            }
                        }
                        Ok(ClientMessage::RequestRepeaterDetail { public_key_prefix_hex }) => {
                            dispatch_repeater_detail(&state, public_key_prefix_hex, &mut writer).await?;
                        }
                        Err(err) => {
                            debug!(error = %err, "invalid IPC client message");
                        }
                    },
                    Some(Err(err)) => {
                        debug!(error = %err, "IPC client read error");
                        break;
                    }
                    None => break,
                }
            }
        }
    }

    Ok(())
}

/// Forwards a command to the mesh connection task (the only one holding
/// the live MeshCore connection) and waits for its outcome. Generic over
/// the reply's `Ok` payload — most commands only report success/failure
/// (`T = ()`), but e.g. `RequestTelemetry` carries a real result.
async fn dispatch_command<T>(
    state: &Arc<AppState>,
    cmd: DaemonCommand,
    reply_rx: oneshot::Receiver<std::result::Result<T, String>>,
    timeout: Duration,
) -> std::result::Result<T, String> {
    state
        .command_tx
        .send(cmd)
        .await
        .map_err(|_| "mesh connection task is not running".to_string())?;

    match tokio::time::timeout(timeout, reply_rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err("mesh connection task dropped the reply".to_string()),
        Err(_) => Err("timed out waiting for the mesh node (offline?)".to_string()),
    }
}

/// Forwards a `RequestRepeaterDetail` command and streams each category's
/// outcome to the client as soon as it's available, rather than batching
/// into one final reply — see `DaemonCommand::RequestRepeaterDetail`. A
/// transport-level failure (mesh task not running, or it goes silent for
/// longer than `REPEATER_DETAIL_COMMAND_TIMEOUT` with categories still
/// outstanding) is reported as a single `ServerMessage::Error`;
/// category-level failures are carried inside each
/// `ServerMessage::RepeaterDetailCategory` and never raise one here.
async fn dispatch_repeater_detail<W>(
    state: &Arc<AppState>,
    public_key_prefix_hex: String,
    writer: &mut FramedWrite<W, LinesCodec>,
) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let (updates_tx, mut updates_rx) = mpsc::channel(4);
    let cmd = DaemonCommand::RequestRepeaterDetail {
        public_key_prefix_hex: public_key_prefix_hex.clone(),
        updates: updates_tx,
    };
    if state.command_tx.send(cmd).await.is_err() {
        send(
            writer,
            &ServerMessage::Error("mesh connection task is not running".to_string()),
        )
        .await?;
        return Ok(());
    }

    let deadline = tokio::time::Instant::now() + REPEATER_DETAIL_COMMAND_TIMEOUT;
    loop {
        match tokio::time::timeout_at(deadline, updates_rx.recv()).await {
            Ok(Some(category)) => {
                send(
                    writer,
                    &ServerMessage::RepeaterDetailCategory {
                        public_key_prefix_hex: public_key_prefix_hex.clone(),
                        category,
                    },
                )
                .await?;
            }
            // The mesh task finished (every `updates` sender dropped) --
            // not an error, just the natural end of the stream.
            Ok(None) => break,
            Err(_) => {
                send(
                    writer,
                    &ServerMessage::Error(
                        "timed out waiting for the mesh node (offline?)".to_string(),
                    ),
                )
                .await?;
                break;
            }
        }
    }
    Ok(())
}

async fn current_snapshot(state: &Arc<AppState>) -> fez_mesh_controller_core::ipc::Snapshot {
    let mut snap = state.snapshot.read().await.clone();
    snap.uptime_secs = state.uptime_secs();
    let config = state.config.read().await;
    snap.regions = config.regions.clone();
    snap.hashtag_channels = config.hashtag_channels.clone();

    let broker_status = state.mqtt_broker_status.read().await;
    snap.mqtt_brokers = config
        .mqtt_brokers
        .iter()
        .map(
            |broker| fez_mesh_controller_core::ipc::MqttBrokerStatusDto {
                name: broker.name.clone(),
                // A broker not yet in the status map hasn't had its first
                // status update from its worker task (see `crate::mqtt`) yet.
                status: broker_status
                    .get(&broker.name)
                    .cloned()
                    .unwrap_or(fez_mesh_controller_core::ipc::MqttBrokerStatus::Connecting),
            },
        )
        .collect();

    snap.node_stats = state.node_stats.read().await.clone();

    snap
}

async fn send<W>(writer: &mut FramedWrite<W, LinesCodec>, msg: &ServerMessage) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let text = serde_json::to_string(msg).context("serializing IPC message")?;
    writer.send(text).await.context("sending IPC message")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fez_mesh_controller_core::ipc::MeshEvent;
    use fez_mesh_controller_core::mesh::{MeshEventKind, PacketLogEntry};
    use fez_mesh_controller_core::{Config, ConnectionConfig, DaemonConfig};
    use std::path::PathBuf;
    use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
    use tokio::sync::mpsc;

    async fn make_state() -> (Arc<AppState>, mpsc::Receiver<DaemonCommand>) {
        let (command_tx, command_rx) = mpsc::channel(8);
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
                db_path: PathBuf::from(":memory:"),
                observer_node_managed_config: true,
            },
            managed_repeaters: vec![],
            regions: vec![],
            hashtag_channels: vec![],
            mqtt_brokers: vec![],
        };
        let state = Arc::new(
            AppState::new(
                command_tx,
                config,
                PathBuf::from("/tmp/fez-mesh-controller-test.toml"),
            )
            .await
            .expect("AppState::new with an in-memory DB should never fail"),
        );
        (state, command_rx)
    }

    /// Spawns `handle_client` on one end of an in-process socket pair (no
    /// real socket file involved) and returns framed read/write halves for
    /// the other end, so a test can drive the protocol directly.
    fn spawn_client(
        state: Arc<AppState>,
    ) -> (
        FramedRead<OwnedReadHalf, LinesCodec>,
        FramedWrite<OwnedWriteHalf, LinesCodec>,
    ) {
        let (client_side, server_side) = UnixStream::pair().expect("socket pair");
        tokio::spawn(async move {
            let _ = handle_client(server_side, state).await;
        });
        let (read_half, write_half) = client_side.into_split();
        (
            FramedRead::new(read_half, LinesCodec::new()),
            FramedWrite::new(write_half, LinesCodec::new()),
        )
    }

    async fn recv(reader: &mut FramedRead<OwnedReadHalf, LinesCodec>) -> ServerMessage {
        let line = tokio::time::timeout(Duration::from_secs(1), reader.next())
            .await
            .expect("timed out waiting for a server message")
            .expect("stream closed")
            .expect("line read error");
        serde_json::from_str(&line).expect("invalid ServerMessage JSON")
    }

    async fn send_client_message(
        writer: &mut FramedWrite<OwnedWriteHalf, LinesCodec>,
        msg: &ClientMessage,
    ) {
        let text = serde_json::to_string(msg).unwrap();
        writer.send(text).await.unwrap();
    }

    /// Drains the three handshake messages every connection starts with.
    async fn drain_handshake(reader: &mut FramedRead<OwnedReadHalf, LinesCodec>) {
        assert!(matches!(recv(reader).await, ServerMessage::Hello { .. }));
        assert!(matches!(recv(reader).await, ServerMessage::Snapshot(_)));
        assert!(matches!(recv(reader).await, ServerMessage::PacketLog(_)));
    }

    #[tokio::test]
    async fn handshake_sends_hello_snapshot_then_packet_log() {
        let (state, _command_rx) = make_state().await;
        let (mut reader, _writer) = spawn_client(state);

        match recv(&mut reader).await {
            ServerMessage::Hello { version } => assert_eq!(version, PROTOCOL_VERSION),
            other => panic!("expected Hello, got {other:?}"),
        }
        assert!(matches!(
            recv(&mut reader).await,
            ServerMessage::Snapshot(_)
        ));
        match recv(&mut reader).await {
            ServerMessage::PacketLog(entries) => assert!(entries.is_empty()),
            other => panic!("expected PacketLog, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn request_snapshot_returns_a_fresh_snapshot() {
        let (state, _command_rx) = make_state().await;
        let (mut reader, mut writer) = spawn_client(state);
        drain_handshake(&mut reader).await;

        send_client_message(&mut writer, &ClientMessage::RequestSnapshot).await;
        assert!(matches!(
            recv(&mut reader).await,
            ServerMessage::Snapshot(_)
        ));
    }

    #[tokio::test]
    async fn current_snapshot_includes_the_configured_regions() {
        use fez_mesh_controller_core::RegionConfig;

        let (command_tx, _command_rx) = mpsc::channel(8);
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
                db_path: PathBuf::from(":memory:"),
                observer_node_managed_config: true,
            },
            managed_repeaters: vec![],
            regions: vec![RegionConfig {
                name: "World".to_string(),
                parent: None,
            }],
            hashtag_channels: vec![],
            mqtt_brokers: vec![],
        };
        let state = Arc::new(
            AppState::new(
                command_tx,
                config,
                PathBuf::from("/tmp/fez-mesh-controller-test.toml"),
            )
            .await
            .expect("AppState::new with an in-memory DB should never fail"),
        );

        let snap = current_snapshot(&state).await;

        assert_eq!(snap.regions.len(), 1);
        assert_eq!(snap.regions[0].name, "World");
    }

    #[tokio::test]
    async fn current_snapshot_includes_configured_mqtt_brokers_with_live_status() {
        use fez_mesh_controller_core::ipc::{MqttBrokerStatus, MqttBrokerStatusDto};
        use fez_mesh_controller_core::MqttBrokerConfig;

        let (command_tx, _command_rx) = mpsc::channel(8);
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
                db_path: PathBuf::from(":memory:"),
                observer_node_managed_config: true,
            },
            managed_repeaters: vec![],
            regions: vec![],
            hashtag_channels: vec![],
            mqtt_brokers: vec![
                MqttBrokerConfig {
                    name: "Home Assistant".to_string(),
                    host: "mqtt.example.com".to_string(),
                    port: 1883,
                    username: None,
                    password: None,
                    auth_method: fez_mesh_controller_core::MqttAuthMethod::Passwd,
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
                    transport_protocol: fez_mesh_controller_core::MqttTransportProtocol::Tcp,
                    websocket_path: "/mqtt".to_string(),
                },
                MqttBrokerConfig {
                    name: "Not Yet Reported".to_string(),
                    host: "other.example.com".to_string(),
                    port: 1883,
                    username: None,
                    password: None,
                    auth_method: fez_mesh_controller_core::MqttAuthMethod::Passwd,
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
                    transport_protocol: fez_mesh_controller_core::MqttTransportProtocol::Tcp,
                    websocket_path: "/mqtt".to_string(),
                },
            ],
        };
        let state = Arc::new(
            AppState::new(
                command_tx,
                config,
                PathBuf::from("/tmp/fez-mesh-controller-test.toml"),
            )
            .await
            .expect("AppState::new with an in-memory DB should never fail"),
        );
        state
            .set_mqtt_broker_status("Home Assistant", MqttBrokerStatus::Connected)
            .await;

        let snap = current_snapshot(&state).await;

        assert_eq!(
            snap.mqtt_brokers,
            vec![
                MqttBrokerStatusDto {
                    name: "Home Assistant".to_string(),
                    status: MqttBrokerStatus::Connected,
                },
                // No status reported yet for this one: defaults to Connecting.
                MqttBrokerStatusDto {
                    name: "Not Yet Reported".to_string(),
                    status: MqttBrokerStatus::Connecting,
                },
            ]
        );
    }

    #[tokio::test]
    async fn broadcast_event_is_forwarded_to_the_client() {
        let (state, _command_rx) = make_state().await;
        let (mut reader, _writer) = spawn_client(state.clone());
        drain_handshake(&mut reader).await;

        state.broadcast_event(MeshEvent {
            at_unix: 0,
            kind: MeshEventKind::Connected,
        });

        assert!(matches!(recv(&mut reader).await, ServerMessage::Event(_)));
    }

    #[tokio::test]
    async fn recorded_packet_is_forwarded_to_the_client() {
        let (state, _command_rx) = make_state().await;
        let (mut reader, _writer) = spawn_client(state.clone());
        drain_handshake(&mut reader).await;

        state
            .record_packet(PacketLogEntry {
                id: 1,
                at_unix: 0,
                snr: 1.0,
                rssi: -90,
                header: None,
                payload_hex: String::new(),
                payload_len: 0,
            })
            .await;

        assert!(matches!(
            recv(&mut reader).await,
            ServerMessage::PacketLogEntry(_)
        ));
    }

    #[tokio::test]
    async fn mutating_command_success_sends_no_error() {
        let (state, mut command_rx) = make_state().await;
        let (mut reader, mut writer) = spawn_client(state);
        drain_handshake(&mut reader).await;

        tokio::spawn(async move {
            if let Some(DaemonCommand::RemoveContact { reply, .. }) = command_rx.recv().await {
                let _ = reply.send(Ok(()));
            }
        });

        send_client_message(
            &mut writer,
            &ClientMessage::RemoveContact {
                public_key_prefix_hex: "aabbccddeeff".to_string(),
            },
        )
        .await;

        // Success sends nothing back; confirm the connection is still
        // healthy and processing in order by requesting a fresh snapshot
        // right after — if an Error had been queued, it would arrive first.
        send_client_message(&mut writer, &ClientMessage::RequestSnapshot).await;
        assert!(matches!(
            recv(&mut reader).await,
            ServerMessage::Snapshot(_)
        ));
    }

    #[tokio::test]
    async fn mutating_command_failure_sends_an_error() {
        let (state, mut command_rx) = make_state().await;
        let (mut reader, mut writer) = spawn_client(state);
        drain_handshake(&mut reader).await;

        tokio::spawn(async move {
            if let Some(DaemonCommand::RemoveContact { reply, .. }) = command_rx.recv().await {
                let _ = reply.send(Err("boom".to_string()));
            }
        });

        send_client_message(
            &mut writer,
            &ClientMessage::RemoveContact {
                public_key_prefix_hex: "aabbccddeeff".to_string(),
            },
        )
        .await;

        match recv(&mut reader).await {
            ServerMessage::Error(reason) => assert_eq!(reason, "boom"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mutating_command_without_a_consumer_fails_fast() {
        let (state, command_rx) = make_state().await;
        drop(command_rx); // nobody will ever receive the command
        let (mut reader, mut writer) = spawn_client(state);
        drain_handshake(&mut reader).await;

        send_client_message(
            &mut writer,
            &ClientMessage::RemoveContact {
                public_key_prefix_hex: "aabbccddeeff".to_string(),
            },
        )
        .await;

        match recv(&mut reader).await {
            ServerMessage::Error(reason) => {
                assert_eq!(reason, "mesh connection task is not running")
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn request_telemetry_success_sends_telemetry_result() {
        use fez_mesh_controller_core::mesh::TelemetryDto;
        use fez_mesh_controller_core::telemetry::TelemetryReading;

        let (state, mut command_rx) = make_state().await;
        let (mut reader, mut writer) = spawn_client(state);
        drain_handshake(&mut reader).await;

        tokio::spawn(async move {
            if let Some(DaemonCommand::RequestTelemetry { reply, .. }) = command_rx.recv().await {
                let _ = reply.send(Ok(TelemetryDto {
                    fetched_at_unix: 1,
                    readings: vec![TelemetryReading {
                        channel: 1,
                        label: "Voltage".to_string(),
                        value: 3.71,
                        unit: "V".to_string(),
                    }],
                }));
            }
        });

        send_client_message(
            &mut writer,
            &ClientMessage::RequestTelemetry {
                public_key_prefix_hex: "aabbccddeeff".to_string(),
            },
        )
        .await;

        match recv(&mut reader).await {
            ServerMessage::TelemetryResult {
                public_key_prefix_hex,
                telemetry,
            } => {
                assert_eq!(public_key_prefix_hex, "aabbccddeeff");
                assert_eq!(telemetry.readings.len(), 1);
                assert_eq!(telemetry.readings[0].value, 3.71);
            }
            other => panic!("expected TelemetryResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn request_telemetry_failure_sends_an_error() {
        let (state, mut command_rx) = make_state().await;
        let (mut reader, mut writer) = spawn_client(state);
        drain_handshake(&mut reader).await;

        tokio::spawn(async move {
            if let Some(DaemonCommand::RequestTelemetry { reply, .. }) = command_rx.recv().await {
                let _ = reply.send(Err("login to aabbccddeeff was rejected".to_string()));
            }
        });

        send_client_message(
            &mut writer,
            &ClientMessage::RequestTelemetry {
                public_key_prefix_hex: "aabbccddeeff".to_string(),
            },
        )
        .await;

        match recv(&mut reader).await {
            ServerMessage::Error(reason) => {
                assert_eq!(reason, "login to aabbccddeeff was rejected")
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn request_repeater_detail_streams_each_category_as_it_arrives() {
        use fez_mesh_controller_core::mesh::{RepeaterDetailCategory, TelemetryDto};
        use fez_mesh_controller_core::telemetry::TelemetryReading;

        let (state, mut command_rx) = make_state().await;
        let (mut reader, mut writer) = spawn_client(state);
        drain_handshake(&mut reader).await;

        tokio::spawn(async move {
            if let Some(DaemonCommand::RequestRepeaterDetail { updates, .. }) =
                command_rx.recv().await
            {
                // Sent out of the "usual" status/telemetry/neighbours/regions
                // order on purpose: the server must forward whatever arrives,
                // in arrival order, not assume or reorder.
                let _ = updates
                    .send(RepeaterDetailCategory::Telemetry(Ok(TelemetryDto {
                        fetched_at_unix: 1,
                        readings: vec![TelemetryReading {
                            channel: 1,
                            label: "Voltage".to_string(),
                            value: 3.71,
                            unit: "V".to_string(),
                        }],
                    })))
                    .await;
                let _ = updates
                    .send(RepeaterDetailCategory::Status(Err("timed out".to_string())))
                    .await;
                // Dropping `updates` here (end of scope) closes the channel,
                // signalling completion even though only two of four
                // categories were ever sent -- mirrors a real category
                // genuinely never resolving.
            }
        });

        send_client_message(
            &mut writer,
            &ClientMessage::RequestRepeaterDetail {
                public_key_prefix_hex: "aabbccddeeff".to_string(),
            },
        )
        .await;

        match recv(&mut reader).await {
            ServerMessage::RepeaterDetailCategory {
                public_key_prefix_hex,
                category: RepeaterDetailCategory::Telemetry(Ok(telemetry)),
            } => {
                assert_eq!(public_key_prefix_hex, "aabbccddeeff");
                assert_eq!(telemetry.readings.len(), 1);
            }
            other => panic!("expected a Telemetry RepeaterDetailCategory, got {other:?}"),
        }
        match recv(&mut reader).await {
            ServerMessage::RepeaterDetailCategory {
                category: RepeaterDetailCategory::Status(Err(reason)),
                ..
            } => {
                assert_eq!(reason, "timed out");
            }
            other => panic!("expected a Status RepeaterDetailCategory, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn request_repeater_detail_transport_failure_sends_an_error() {
        let (state, command_rx) = make_state().await;
        drop(command_rx); // nobody will ever receive the command
        let (mut reader, mut writer) = spawn_client(state);
        drain_handshake(&mut reader).await;

        send_client_message(
            &mut writer,
            &ClientMessage::RequestRepeaterDetail {
                public_key_prefix_hex: "aabbccddeeff".to_string(),
            },
        )
        .await;

        match recv(&mut reader).await {
            ServerMessage::Error(reason) => {
                assert_eq!(reason, "mesh connection task is not running")
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }
}
