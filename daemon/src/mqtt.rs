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

//! Forwards received mesh events to configured MQTT brokers, using the same
//! topic structure and JSON envelope as the community
//! `ipnet-mesh/meshcore-mqtt` bridge (`mqtt_worker.py`'s
//! `_determine_mqtt_topic()`/`_serialize_to_json()`, verified against its
//! source, not assumed). The `<prefix>/status` topic instead matches the
//! community `agessaman/meshcore-packet-capture` bridge's richer status
//! message shape (verified against its source) — see
//! [`status_message_json`]. All of this — the status topic and every
//! decoded-event topic [`topic_for`] covers — is only published when
//! [`MqttBrokerConfig::enable_high_level_messages`] is set.
//!
//! Two more topics forward every *raw* overheard packet, independent of
//! whether this project's own decoders recognize it — see
//! [`packet_trafic_json`] (rich schema, matches
//! `Colorado-Mesh/mesh-client`'s `letsmesh-mqtt-auth.md` and
//! `agessaman/meshcore-packet-capture`'s own `packets` topic) and
//! [`raw_json`] (agessaman's separate, minimal, opt-in-only `raw` topic).

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use fez_mesh_controller_core::config::{MqttAuthMethod, MqttBrokerConfig, MqttTransportProtocol};
use fez_mesh_controller_core::ipc::{MeshEvent, MqttBrokerStatus};
use fez_mesh_controller_core::mesh::{
    build_packet_log_entry, hex_encode, reconstruct_raw_packet_hex, DeviceInfoDto, MeshEventKind,
    NodeStatsDto, PacketLogEntry, SelfInfoDto,
};
use fez_mesh_controller_core::mqtt_jwt::{self, AuthTokenClaims};
use meshcore_rs::events::EventPayload;
use meshcore_rs::{EventType, MeshCoreEvent};
use rumqttc::{AsyncClient, Event as MqttEvent, LastWill, MqttOptions, Packet, QoS, Transport};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::oneshot;
use tracing::{info, warn};

use crate::command::DaemonCommand;
use crate::state::AppState;

const RECONNECT_DELAY: Duration = Duration::from_secs(5);
/// Cap on how long a status publish waits for a fresh stats fetch (3 RPC
/// round-trips to the node) before falling back to the cached value —
/// stats are best-effort, this must never stall the status publish for
/// long, e.g. while the mesh is disconnected and nobody is polling
/// `command_rx`.
const STATS_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);
/// Cap on how long a device-signed auth token request waits for the node to
/// sign it, matching `MeshClient::sign`'s own `sign_finish` budget.
const SIGN_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to wait, at broker startup, for `self_info` to become known
/// before giving up on building device-signed credentials (it's populated
/// by `mesh_task::run`, a separate task racing this one at daemon startup).
const SELF_INFO_WAIT_TIMEOUT: Duration = Duration::from_secs(60);
/// How long before a device-signed auth token's expiry to refresh it —
/// matches `Colorado-Mesh/mesh-client`'s own fixed 6-minute margin.
const JWT_REFRESH_MARGIN_SECS: u64 = 360;

/// Identifies this daemon as the publisher, mirroring the `<name>/<version>`
/// shape the Python bridges use for their own `client_version` field.
const CLIENT_VERSION: &str = concat!("fez-mesh-controller/", env!("CARGO_PKG_VERSION"));

/// Spawns [`run_broker`] as its own task and returns a handle that can abort
/// it — used both at daemon startup and by `crate::reload` when a config
/// reload adds, removes or changes a broker.
pub fn spawn(state: Arc<AppState>, broker: MqttBrokerConfig) -> tokio::task::AbortHandle {
    tokio::spawn(run_broker(broker, state)).abort_handle()
}

/// Runs the publish loop for one configured broker: connects (rumqttc
/// retries internally as long as `poll()` keeps being called), subscribes
/// to every raw mesh event the daemon receives, and publishes the ones
/// [`topic_for`] covers. Loops forever — a broker outage must not affect
/// the mesh connection or other brokers (mirrors `mesh_task::run`'s own
/// reconnect-forever style).
pub async fn run_broker(config: MqttBrokerConfig, state: Arc<AppState>) {
    let client_id = format!("fez-mesh-controller-{}-{}", config.name, std::process::id());
    let mut options = match config.transport_protocol {
        MqttTransportProtocol::Tcp => MqttOptions::new(client_id, config.host.clone(), config.port),
        MqttTransportProtocol::Websocket => {
            // For Ws/Wss, rumqttc takes the full URL (scheme + host + port +
            // path) as the "host" — the separately-configured `port` is
            // unused for this transport (rumqttc derives it from the URL).
            let scheme = if config.tls_enabled { "wss" } else { "ws" };
            let url = format!(
                "{scheme}://{}:{}{}",
                config.host, config.port, config.websocket_path
            );
            MqttOptions::new(client_id, url, config.port)
        }
    };
    options.set_keep_alive(Duration::from_secs(60));

    match config.auth_method {
        MqttAuthMethod::Device => {
            let Some(self_info) = wait_for_self_info(&state, SELF_INFO_WAIT_TIMEOUT).await else {
                warn!(
                    broker = %config.name,
                    "timed out waiting for mesh self_info; cannot build device-signed MQTT credentials"
                );
                state
                    .set_mqtt_broker_status(
                        &config.name,
                        MqttBrokerStatus::Error {
                            reason: "timed out waiting for mesh connection to build \
                                     device-signed auth credentials"
                                .to_string(),
                        },
                    )
                    .await;
                return;
            };
            match build_device_signed_credentials(&state, &self_info, &config).await {
                Ok((username, token)) => {
                    options.set_credentials(username, token);
                }
                Err(reason) => {
                    warn!(broker = %config.name, %reason, "failed to build device-signed MQTT credentials");
                    state
                        .set_mqtt_broker_status(&config.name, MqttBrokerStatus::Error { reason })
                        .await;
                    return;
                }
            }
        }
        MqttAuthMethod::Passwd => {
            if let (Some(username), Some(password)) = (&config.username, &config.password) {
                options.set_credentials(username.clone(), password.clone());
            }
        }
        MqttAuthMethod::None => {}
    }

    if config.tls_enabled {
        match build_transport(&config) {
            Ok(transport) => {
                options.set_transport(transport);
            }
            Err(reason) => {
                warn!(broker = %config.name, %reason, "invalid MQTT TLS configuration");
                state
                    .set_mqtt_broker_status(&config.name, MqttBrokerStatus::Error { reason })
                    .await;
                return;
            }
        }
    }

    if config.enable_high_level_messages {
        // Best-effort — the mesh connection may not be established yet when
        // this broker task starts, in which case the LWT falls back to the
        // same "unknown"/"DEVICE" placeholders `status_message_json` itself
        // uses. Set once at options-build time, like the Python bridge's own
        // `mqtt_client.will_set(...)` (called once at client creation).
        let self_info = state.snapshot.read().await.self_info.clone();
        let lwt_topic =
            resolve_topic_template_best_effort(&config.status_topic, &config, self_info.as_ref());
        let lwt_payload = serde_json::to_vec(&offline_lwt_payload(self_info.as_ref(), Utc::now()))
            .unwrap_or_default();
        options.set_last_will(LastWill::new(lwt_topic, lwt_payload, QoS::AtMostOnce, true));
    }

    let (client, mut eventloop) = AsyncClient::new(options, 64);
    let mut raw_events = state.raw_events_tx.subscribe();
    // Subscribed before the snapshot read below, so any Connected/Disconnected
    // transition happening in between is still observed via the channel
    // rather than silently missed.
    let mut mesh_events = state.events_tx.subscribe();
    let mut mesh_connected = state.snapshot.read().await.mesh_connected;

    state
        .set_mqtt_broker_status(&config.name, MqttBrokerStatus::Connecting)
        .await;

    let mut mqtt_connected = false;
    let mut heartbeat = (config.status_refresh_interval_secs > 0).then(|| {
        tokio::time::interval(Duration::from_secs(
            config.status_refresh_interval_secs as u64,
        ))
    });
    let mut jwt_refresh = (config.auth_method == MqttAuthMethod::Device).then(|| {
        let refresh_period_secs = (config.jwt_ttl_secs as u64)
            .saturating_sub(JWT_REFRESH_MARGIN_SECS)
            .max(1);
        tokio::time::interval(Duration::from_secs(refresh_period_secs))
    });

    loop {
        tokio::select! {
            poll_result = eventloop.poll() => {
                match poll_result {
                    Ok(MqttEvent::Incoming(Packet::ConnAck(_))) => {
                        info!(broker = %config.name, "connected to MQTT broker");
                        mqtt_connected = true;
                        state
                            .set_mqtt_broker_status(&config.name, MqttBrokerStatus::Connected)
                            .await;
                        // Self-heals from a missed/lagged mesh_events broadcast.
                        mesh_connected = state.snapshot.read().await.mesh_connected;
                        publish_current_status(&client, &config, &state, mesh_connected).await;
                    }
                    Ok(_) => {}
                    Err(err) => {
                        warn!(broker = %config.name, error = %err, "MQTT connection error");
                        mqtt_connected = false;
                        state
                            .set_mqtt_broker_status(
                                &config.name,
                                MqttBrokerStatus::Error { reason: err.to_string() },
                            )
                            .await;
                        // The LWT already covers an unclean MQTT disconnect;
                        // a genuine mesh disconnect is handled by the
                        // mesh_events branch below, not here.
                        tokio::time::sleep(RECONNECT_DELAY).await;
                    }
                }
            }
            _ = tick_optional(&mut heartbeat), if mqtt_connected => {
                publish_current_status(&client, &config, &state, mesh_connected).await;
            }
            _ = tick_optional(&mut jwt_refresh) => {
                let Some(self_info) = state.snapshot.read().await.self_info.clone() else {
                    warn!(
                        broker = %config.name,
                        "mesh self_info unknown; skipping device-signed MQTT credential refresh"
                    );
                    continue;
                };
                match build_device_signed_credentials(&state, &self_info, &config).await {
                    Ok((username, token)) => {
                        eventloop.mqtt_options.set_credentials(username, token);
                        // MQTT 3.1.1 has no in-band re-auth packet -- a
                        // refreshed credential only takes effect on the
                        // *next* CONNECT, so force one when currently
                        // connected. If not connected, the next natural
                        // reconnect already picks up `mqtt_options` above.
                        if mqtt_connected {
                            if let Err(err) = client.disconnect().await {
                                warn!(
                                    broker = %config.name,
                                    error = %err,
                                    "failed to force MQTT reconnect for auth token refresh"
                                );
                            }
                        }
                    }
                    Err(reason) => {
                        warn!(broker = %config.name, %reason, "failed to refresh device-signed MQTT credentials");
                    }
                }
            }
            mesh_event = mesh_events.recv() => {
                match mesh_event {
                    Ok(MeshEvent { kind: MeshEventKind::Connected, .. }) => {
                        mesh_connected = true;
                        publish_current_status(&client, &config, &state, mesh_connected).await;
                    }
                    Ok(MeshEvent { kind: MeshEventKind::Disconnected, .. }) => {
                        mesh_connected = false;
                        publish_current_status(&client, &config, &state, mesh_connected).await;
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                }
            }
            event = raw_events.recv() => {
                match event {
                    Ok(event) => {
                        publish_event(&client, &config, &event).await;
                        publish_raw_packet(&client, &config, &state, &event).await;
                    }
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                }
            }
        }
    }
}

/// Awaits the next tick of an optional interval, or never resolves if
/// `None` (heartbeat disabled, `status_refresh_interval_secs == 0`) — lets
/// `run_broker`'s `tokio::select!` carry a heartbeat branch unconditionally.
async fn tick_optional(interval: &mut Option<tokio::time::Interval>) {
    match interval {
        Some(interval) => {
            interval.tick().await;
        }
        None => std::future::pending().await,
    }
}

/// Polls `state.snapshot`'s `self_info` until it's known, needed before
/// building device-signed MQTT credentials (the JWT payload's `publicKey`
/// requires it) — `self_info` is populated by `mesh_task::run`, a separate
/// task racing this one at daemon startup. Returns `None` if it never
/// becomes known within `timeout`.
async fn wait_for_self_info(state: &AppState, timeout: Duration) -> Option<SelfInfoDto> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(info) = state.snapshot.read().await.self_info.clone() {
            return Some(info);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Asks the mesh connection task to sign `data` on-device (see
/// [`DaemonCommand::SignData`]) — `mqtt.rs` doesn't hold the live
/// `MeshClient` itself, only `mesh_task.rs` does (same reasoning as
/// `refresh_node_stats`). Unlike `refresh_node_stats`, failures here are
/// propagated rather than falling back to stale data: without a fresh
/// signature a device-signed broker simply cannot authenticate.
async fn sign_via_mesh(state: &AppState, data: &[u8]) -> Result<Vec<u8>, String> {
    let (reply_tx, reply_rx) = oneshot::channel();
    state
        .command_tx
        .send(DaemonCommand::SignData {
            data: data.to_vec(),
            reply: reply_tx,
        })
        .await
        .map_err(|_| "mesh connection task is not running".to_string())?;

    match tokio::time::timeout(SIGN_TIMEOUT, reply_rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err("mesh connection task dropped the sign request".to_string()),
        Err(_) => Err("timed out waiting for the node to sign the auth token".to_string()),
    }
}

/// Builds `(username, password)` for a device-signed MQTT broker: username
/// `v1_<node public key, uppercase hex>`, password a JWT-style token signed
/// on-device — matches `Colorado-Mesh/mesh-client`'s `letsmesh-mqtt-auth.md`
/// scheme (see [`MqttBrokerConfig::auth_method`]'s doc comment and
/// [`fez_mesh_controller_core::mqtt_jwt`]). The node's private key never
/// leaves the device: only the signing input bytes are sent to it.
async fn build_device_signed_credentials(
    state: &AppState,
    self_info: &SelfInfoDto,
    config: &MqttBrokerConfig,
) -> Result<(String, String), String> {
    let username = format!("v1_{}", self_info.public_key_hex.to_uppercase());
    let iat = Utc::now().timestamp();
    let claims = AuthTokenClaims {
        public_key_hex: self_info.public_key_hex.clone(),
        iat,
        exp: iat + config.jwt_ttl_secs as i64,
        aud: config
            .jwt_audience
            .clone()
            .unwrap_or_else(|| config.host.clone()),
    };
    let signing_input = mqtt_jwt::signing_input(&claims);
    let signature = sign_via_mesh(state, signing_input.as_bytes()).await?;
    Ok((
        username,
        mqtt_jwt::assemble_token(&signing_input, &signature),
    ))
}

/// Builds a `rumqttc` TLS `Transport` from the broker's cert/key paths —
/// `Transport::Tls` for plain TCP, `Transport::Wss` when
/// `transport_protocol` is [`MqttTransportProtocol::Websocket`].
/// `tls_client_cert`/`tls_client_key` must both be set or neither; a
/// custom `tls_ca_cert` is required when they are (rumqttc's simple TLS
/// constructor ties the two together) — otherwise the platform's own
/// trust store is used.
fn build_transport(config: &MqttBrokerConfig) -> Result<Transport, String> {
    let client_auth = match (&config.tls_client_cert, &config.tls_client_key) {
        (Some(cert_path), Some(key_path)) => {
            let cert = std::fs::read(cert_path)
                .map_err(|err| format!("reading tls_client_cert {}: {err}", cert_path.display()))?;
            let key = std::fs::read(key_path)
                .map_err(|err| format!("reading tls_client_key {}: {err}", key_path.display()))?;
            Some((cert, key))
        }
        (None, None) => None,
        _ => {
            return Err(
                "tls_client_cert and tls_client_key must both be set, or neither".to_string(),
            )
        }
    };
    let websocket = config.transport_protocol == MqttTransportProtocol::Websocket;

    match &config.tls_ca_cert {
        Some(ca_path) => {
            let ca = std::fs::read(ca_path)
                .map_err(|err| format!("reading tls_ca_cert {}: {err}", ca_path.display()))?;
            if websocket {
                Ok(Transport::wss(ca, client_auth, None))
            } else {
                Ok(Transport::tls(ca, client_auth, None))
            }
        }
        None if client_auth.is_some() => {
            Err("tls_ca_cert is required when tls_client_cert/tls_client_key are set".to_string())
        }
        None if websocket => Ok(Transport::wss_with_default_config()),
        None => Ok(Transport::tls_with_default_config()),
    }
}

async fn publish_event(client: &AsyncClient, config: &MqttBrokerConfig, event: &MeshCoreEvent) {
    if !config.enable_high_level_messages {
        return;
    }
    let Some(topic) = topic_for(event, &config.topic_prefix) else {
        return;
    };
    let body = match serde_json::to_vec(&payload_json(event)) {
        Ok(bytes) => bytes,
        Err(err) => {
            warn!(broker = %config.name, error = %err, "failed to serialize MQTT payload");
            return;
        }
    };
    if let Err(err) = client.publish(topic, QoS::AtMostOnce, false, body).await {
        warn!(broker = %config.name, error = %err, "failed to publish to MQTT broker");
    }
}

/// Substitutes `{prefix}`/`{public_key}` placeholders in a configured topic
/// route template — shared by [`MqttBrokerConfig::packet_trafic_topic`] and
/// [`MqttBrokerConfig::raw_topic`]. Requires a known `self_info` for the
/// same reason as [`status_message_json`] — see [`publish_raw_packet`].
fn resolve_topic_template(
    template: &str,
    config: &MqttBrokerConfig,
    self_info: &SelfInfoDto,
) -> String {
    template
        .replace("{prefix}", &config.topic_prefix)
        .replace("{public_key}", &self_info.public_key_hex.to_uppercase())
}

/// Same substitution as [`resolve_topic_template`], but for the one call
/// site (the LWT, set once at broker-connect time) that must still resolve
/// a topic *before* `self_info` may be known — falls back to
/// [`origin_id`]'s `"DEVICE"` placeholder for `{public_key}` in that case,
/// same documented best-effort limitation as [`offline_lwt_payload`].
fn resolve_topic_template_best_effort(
    template: &str,
    config: &MqttBrokerConfig,
    self_info: Option<&SelfInfoDto>,
) -> String {
    template
        .replace("{prefix}", &config.topic_prefix)
        .replace("{public_key}", &origin_id(self_info))
}

/// Publishes every raw overheard packet to the configured raw-packet
/// topics, independent of [`publish_event`]/[`topic_for`] — covers packet
/// types this project's own decoders don't recognize too. Both topics are
/// independently gated by their own `enable_*` config field. Publishes
/// nothing until `self_info` is known — same "never send an undefined
/// origin/origin_id" rule as [`publish_current_status`].
async fn publish_raw_packet(
    client: &AsyncClient,
    config: &MqttBrokerConfig,
    state: &AppState,
    event: &MeshCoreEvent,
) {
    if !config.enable_packet_trafic_messages && !config.enable_raw_messages {
        return;
    }
    let Some(self_info) = state.snapshot.read().await.self_info.clone() else {
        return;
    };
    let Some(entry) = build_packet_log_entry(event, 0, Utc::now().timestamp()) else {
        return;
    };
    let now = Utc::now();

    if config.enable_packet_trafic_messages {
        if let Some(body) = packet_trafic_json(&entry, &self_info, now) {
            let topic = resolve_topic_template(&config.packet_trafic_topic, config, &self_info);
            if let Err(err) = client
                .publish(
                    topic,
                    QoS::AtMostOnce,
                    false,
                    serde_json::to_vec(&body).unwrap_or_default(),
                )
                .await
            {
                warn!(broker = %config.name, error = %err, "failed to publish packet capture to MQTT broker");
            }
        }
    }

    if config.enable_raw_messages {
        if let Some(body) = raw_json(&entry, &self_info, now) {
            let topic = resolve_topic_template(&config.raw_topic, config, &self_info);
            if let Err(err) = client
                .publish(
                    topic,
                    QoS::AtMostOnce,
                    false,
                    serde_json::to_vec(&body).unwrap_or_default(),
                )
                .await
            {
                warn!(broker = %config.name, error = %err, "failed to publish raw data to MQTT broker");
            }
        }
    }
}

/// Rich `"PACKET"`-schema raw-packet message — matches the format
/// documented in `Colorado-Mesh/mesh-client`'s `letsmesh-mqtt-auth.md`
/// ("Packet logger" topic), field-for-field identical to
/// `agessaman/meshcore-packet-capture`'s own `packets` topic
/// (`format_packet_data()`). `None` if `entry.header` is `None` (can't
/// populate `packet_type`/`route`, and nothing to reconstruct `raw` from).
fn packet_trafic_json(
    entry: &PacketLogEntry,
    self_info: &SelfInfoDto,
    now: DateTime<Utc>,
) -> Option<Value> {
    let header = entry.header.as_ref()?;
    let raw_hex = reconstruct_raw_packet_hex(entry)?;
    // "raw": "raw packet hex (truncated to 2048 chars)" — letsmesh-mqtt-auth.md.
    let raw_hex: String = raw_hex.chars().take(2048).collect();
    let transport_code_len = if header.transport_code_hex.is_some() {
        4
    } else {
        0
    };
    let len = 1 + transport_code_len + 1 + header.path_hex.len() / 2 + entry.payload_len;
    let route = match header.route_type.as_str() {
        "Direct" | "TransportDirect" => "direct".to_string(),
        _ => header.hops.to_string(),
    };
    // Not a firmware-native value (no such concept verified in MeshCore's
    // protocol) — a locally-computed dedup convenience, not a protocol field.
    let hash = hex_encode(&Sha256::digest(raw_hex.as_bytes())[..8]);

    Some(json!({
        "origin": self_info.name,
        "origin_id": self_info.public_key_hex.to_uppercase(),
        "timestamp": now.to_rfc3339_opts(SecondsFormat::Micros, false),
        "type": "PACKET",
        "direction": "rx",
        "time": now.format("%H:%M:%S").to_string(),
        "date": now.format("%d/%m/%Y").to_string(),
        "len": len,
        "packet_type": header.payload_type_raw,
        "route": route,
        "payload_len": entry.payload_len,
        "raw": raw_hex,
        "SNR": entry.snr,
        "RSSI": entry.rssi,
        "hash": hash,
    }))
}

/// Minimal `"RAW"` envelope — matches
/// `agessaman/meshcore-packet-capture`'s own separate `raw` topic
/// (`output_packet()`, `packet_capture.py:3707-3715`): just the raw packet
/// hex, uppercase, untruncated (no cap documented in its source).
fn raw_json(entry: &PacketLogEntry, self_info: &SelfInfoDto, now: DateTime<Utc>) -> Option<Value> {
    let raw_hex = reconstruct_raw_packet_hex(entry)?;
    Some(json!({
        "origin": self_info.name,
        "origin_id": self_info.public_key_hex.to_uppercase(),
        "timestamp": now.to_rfc3339_opts(SecondsFormat::Micros, false),
        "type": "RAW",
        "data": raw_hex.to_uppercase(),
    }))
}

/// Publishes the bridge's own connection-status topic (`<prefix>/status`),
/// retained — matches `agessaman/meshcore-packet-capture`'s own
/// `publish_status()` JSON shape (verified against its source), including
/// its `stats` field — see [`status_message_json`]. Publishes **nothing**
/// until `self_info` (i.e. `origin`/`origin_id`) is known — never sends a
/// message with placeholder/undefined values.
async fn publish_current_status(
    client: &AsyncClient,
    config: &MqttBrokerConfig,
    state: &AppState,
    mesh_connected: bool,
) {
    if !config.enable_high_level_messages {
        return;
    }
    let Some(self_info) = state.snapshot.read().await.self_info.clone() else {
        return;
    };
    let device_info = state.device_info.read().await.clone();
    let node_stats = refresh_node_stats(state).await;
    let body = status_message_json(
        mesh_connected,
        &self_info,
        device_info.as_ref(),
        node_stats.as_ref(),
        Utc::now(),
    );
    let topic = resolve_topic_template(&config.status_topic, config, &self_info);
    if let Err(err) = client
        .publish(
            topic,
            QoS::AtMostOnce,
            true,
            serde_json::to_vec(&body).unwrap_or_default(),
        )
        .await
    {
        warn!(broker = %config.name, error = %err, "failed to publish MQTT status");
    }
}

/// Fetches a fresh [`NodeStatsDto`] before a status publish, mirroring
/// `agessaman`'s own `refresh_stats(force=True)` right before
/// `publish_status`. `mqtt.rs` doesn't hold the live `MeshClient` (only
/// `mesh_task.rs` does), so this asks for a refresh via
/// [`DaemonCommand::RefreshNodeStats`] and waits briefly — on timeout
/// (e.g. the mesh is currently disconnected and nobody is polling
/// `command_rx`) or a dropped reply, falls back to whatever's already
/// cached in [`AppState::node_stats`] rather than failing the publish.
async fn refresh_node_stats(state: &AppState) -> Option<NodeStatsDto> {
    let (reply_tx, reply_rx) = oneshot::channel();
    if state
        .command_tx
        .send(DaemonCommand::RefreshNodeStats { reply: reply_tx })
        .await
        .is_ok()
    {
        if let Ok(Ok(stats)) = tokio::time::timeout(STATS_REFRESH_TIMEOUT, reply_rx).await {
            return Some(stats);
        }
    }
    state.node_stats.read().await.clone()
}

/// Builds the `<prefix>/status` message body — matches
/// `agessaman/meshcore-packet-capture`'s `publish_status()`
/// (`packet_capture.py:2792-2830`) field-for-field, including `stats`.
///
/// `self_info` is required (not `Option`) rather than falling back to a
/// placeholder — callers must already have confirmed it's known, so this
/// function can never publish an "unknown"/"DEVICE" message. `status` is
/// `"online"` only when the mesh is connected *and* `device_info` is also
/// known; otherwise `"offline"`, with `model`/`firmware_version` omitted
/// entirely (not placeholdered) if `device_info` was never successfully
/// fetched. On a genuine mesh disconnect, `device_info`/`self_info` simply
/// keep their last-known values (`AppState` never clears them,
/// `mesh_task.rs`), so `"offline"` naturally still carries the last-known
/// `model`/`firmware_version`/`radio` rather than resetting them.
///
/// `stats`, when present, is only included while `status == "online"` —
/// matching `agessaman`'s own `if status.lower() == "online"` gate exactly
/// (`packet_capture.py:2806-2810`) — with `packets_sent`/`packets_received`
/// aliases added on top of the packet counters
/// (`agessaman`'s `normalize_packet_stats`, `packet_capture.py:2850-2858`,
/// not part of `meshcore_py`/`meshcore-rs` itself, so added here rather
/// than on [`fez_mesh_controller_core::mesh::PacketStatsDto`]).
fn status_message_json(
    mesh_connected: bool,
    self_info: &SelfInfoDto,
    device_info: Option<&DeviceInfoDto>,
    node_stats: Option<&NodeStatsDto>,
    now: DateTime<Utc>,
) -> Value {
    let status = if mesh_connected && device_info.is_some() {
        "online"
    } else {
        "offline"
    };

    let mut fields = serde_json::Map::new();
    fields.insert("status".to_string(), json!(status));
    fields.insert(
        "timestamp".to_string(),
        json!(now.to_rfc3339_opts(SecondsFormat::Micros, false)),
    );
    fields.insert("origin".to_string(), json!(self_info.name));
    fields.insert(
        "origin_id".to_string(),
        json!(self_info.public_key_hex.to_uppercase()),
    );
    if let Some(device_info) = device_info {
        fields.insert("model".to_string(), json!(device_info.model));
        fields.insert(
            "firmware_version".to_string(),
            json!(device_info.firmware_version),
        );
    }
    fields.insert(
        "radio".to_string(),
        json!(format!(
            "{},{},{},{}",
            self_info.radio_freq_mhz,
            self_info.radio_bw_khz,
            self_info.spreading_factor,
            self_info.coding_rate
        )),
    );
    fields.insert("client_version".to_string(), json!(CLIENT_VERSION));

    if status == "online" {
        if let Some(node_stats) = node_stats {
            if let Value::Object(mut stats_fields) = serde_json::to_value(node_stats)
                .unwrap_or_else(|_| Value::Object(serde_json::Map::new()))
            {
                if let Some(packets) = &node_stats.packets {
                    stats_fields.insert("packets_sent".to_string(), json!(packets.sent));
                    stats_fields.insert("packets_received".to_string(), json!(packets.recv));
                }
                if !stats_fields.is_empty() {
                    fields.insert("stats".to_string(), Value::Object(stats_fields));
                }
            }
        }
    }

    Value::Object(fields)
}

/// The Last Will payload set once at MQTT connect time, published by the
/// broker itself if this daemon disconnects uncleanly — matches
/// `agessaman/meshcore-packet-capture`'s own LWT (`packet_capture.py:2679-2690`):
/// a strict subset of [`status_message_json`], no `model`/`firmware_version`/
/// `radio`/`client_version`/`stats`.
fn offline_lwt_payload(self_info: Option<&SelfInfoDto>, now: DateTime<Utc>) -> Value {
    json!({
        "status": "offline",
        "timestamp": now.to_rfc3339_opts(SecondsFormat::Micros, false),
        "origin": self_info.map(|s| s.name.as_str()).unwrap_or("unknown"),
        "origin_id": origin_id(self_info),
    })
}

/// `origin_id` field shared by [`status_message_json`]/[`offline_lwt_payload`]
/// — uppercase public key hex, falling back to the Python bridge's own
/// `"DEVICE"` literal when unknown.
fn origin_id(self_info: Option<&SelfInfoDto>) -> String {
    self_info
        .map(|s| s.public_key_hex.to_uppercase())
        .unwrap_or_else(|| "DEVICE".to_string())
}

/// Topic for a raw mesh event — mirrors `_determine_mqtt_topic()`
/// (`mqtt_worker.py`). Returns `None` for anything not in this list:
/// internal command/response plumbing (`Ok`, `Error`, ...) is never
/// forwarded.
fn topic_for(event: &MeshCoreEvent, prefix: &str) -> Option<String> {
    match (&event.event_type, &event.payload) {
        (EventType::Connected, _) | (EventType::Disconnected, _) => {
            Some(format!("{prefix}/events/connection"))
        }
        (EventType::LoginSuccess, _) | (EventType::LoginFailed, _) => {
            Some(format!("{prefix}/login"))
        }
        (EventType::DeviceInfo, _) => Some(format!("{prefix}/device_info")),
        (EventType::Battery, _) => Some(format!("{prefix}/battery")),
        (EventType::NewContact, _) => Some(format!("{prefix}/new_contact")),
        (EventType::Advertisement, _) => Some(format!("{prefix}/advertisement")),
        (EventType::TelemetryResponse, _) => Some(format!("{prefix}/telemetry")),
        (EventType::Contacts, _) => Some(format!("{prefix}/contacts")),
        (EventType::SelfInfo, _) => Some(format!("{prefix}/self_info")),
        (EventType::ChannelInfo, _) => Some(format!("{prefix}/channel_info")),
        (EventType::TraceData, _) => {
            // The Python bridge path-segments this topic by the request's
            // tag; `meshcore-rs`'s `TraceInfo` doesn't expose it (only the
            // per-hop SNR list), so this always takes the same "unknown"
            // fallback the bridge itself uses when it can't determine the
            // tag.
            Some(format!("{prefix}/traceroute/unknown"))
        }
        (EventType::ContactMsgRecv, EventPayload::ContactMessage(msg)) => Some(format!(
            "{prefix}/message/direct/{}",
            hex_encode(&msg.sender_prefix)
        )),
        (EventType::ChannelMsgRecv, EventPayload::ChannelMessage(msg)) => {
            Some(format!("{prefix}/message/channel/{}", msg.channel_idx))
        }
        _ => None,
    }
}

/// Builds the `{"type": "EventType.<NAME>", "payload": {...}, "attributes":
/// {...}}` envelope `ipnet-mesh/meshcore-mqtt` publishes
/// (`_serialize_to_json()` serializing the whole `Event.__dict__` —
/// verified against source). `payload` field names match `meshcore_py`'s
/// own decoded dict keys (`reader.py`) where cross-checked; best-effort
/// for less common event types where they weren't.
fn payload_json(event: &MeshCoreEvent) -> Value {
    let type_name = format!(
        "EventType.{}",
        screaming_snake_case(&format!("{:?}", event.event_type))
    );
    let (payload, attributes) = event_payload_json(event);
    json!({ "type": type_name, "payload": payload, "attributes": attributes })
}

/// `(payload, attributes)` for every event type [`topic_for`] covers.
/// Field names match `meshcore_py`'s `reader.py` where verified this
/// session; unverified/rare types get a reasonable best-effort shape
/// instead of guessed Python key names.
fn event_payload_json(event: &MeshCoreEvent) -> (Value, Value) {
    match &event.payload {
        EventPayload::ContactMessage(msg) => (
            json!({
                "pubkey_prefix": hex_encode(&msg.sender_prefix),
                "path_len": msg.path_len,
                "txt_type": msg.txt_type,
                "sender_timestamp": msg.sender_timestamp,
                "text": msg.text,
                "SNR": msg.snr,
            }),
            json!({
                "pubkey_prefix": hex_encode(&msg.sender_prefix),
                "txt_type": msg.txt_type,
            }),
        ),
        EventPayload::ChannelMessage(msg) => (
            json!({
                "channel_idx": msg.channel_idx,
                "path_len": msg.path_len,
                "txt_type": msg.txt_type,
                "sender_timestamp": msg.sender_timestamp,
                "text": msg.text,
                "SNR": msg.snr,
            }),
            json!({
                "channel_idx": msg.channel_idx,
                "txt_type": msg.txt_type,
            }),
        ),
        EventPayload::Advertisement(adv) => (
            json!({ "public_key": hex_encode(&adv.prefix) }),
            Value::Object(Default::default()),
        ),
        EventPayload::Contact(contact) => (
            json!({
                "public_key": contact.public_key_hex(),
                "adv_name": contact.adv_name,
                "adv_lat": contact.latitude(),
                "adv_lon": contact.longitude(),
            }),
            Value::Object(Default::default()),
        ),
        EventPayload::Contacts(contacts) => (
            Value::Array(
                contacts
                    .iter()
                    .map(|c| {
                        json!({
                            "public_key": c.public_key_hex(),
                            "adv_name": c.adv_name,
                        })
                    })
                    .collect(),
            ),
            Value::Object(Default::default()),
        ),
        EventPayload::DeviceInfo(info) => (
            json!({
                "fw ver": info.fw_version_code,
                "max_contacts": info.max_contacts,
                "max_channels": info.max_channels,
                "ble_pin": info.ble_pin,
                "fw_build": info.fw_build,
                "model": info.model,
                "ver": info.version,
                "repeat": info.repeat,
            }),
            Value::Object(Default::default()),
        ),
        EventPayload::Battery(battery) => (
            json!({
                "level": battery.battery_mv,
                "used_kb": battery.used_kb,
                "total_kb": battery.total_kb,
            }),
            Value::Object(Default::default()),
        ),
        EventPayload::SelfInfo(info) => (
            json!({
                "public_key": hex_encode(&info.public_key),
                "name": info.name,
                "adv_lat": info.adv_lat as f64 / 1_000_000.0,
                "adv_lon": info.adv_lon as f64 / 1_000_000.0,
            }),
            Value::Object(Default::default()),
        ),
        EventPayload::ChannelInfo(info) => (
            json!({
                "channel_idx": info.channel_idx,
                "channel_name": info.name,
                "channel_hash": hex_encode(&info.secret[..1]),
            }),
            Value::Object(Default::default()),
        ),
        EventPayload::TraceData(trace) => (
            json!({
                "path": trace
                    .hops
                    .iter()
                    .map(|hop| json!({ "hash": hex_encode(&hop.prefix), "snr": hop.snr }))
                    .collect::<Vec<_>>(),
            }),
            Value::Object(Default::default()),
        ),
        EventPayload::Telemetry(bytes) => (
            json!({ "lpp_data_hex": hex_encode(bytes) }),
            Value::Object(Default::default()),
        ),
        EventPayload::None => (Value::Null, Value::Object(Default::default())),
        EventPayload::String(s) => (json!(s), Value::Object(Default::default())),
        other => (
            json!({ "debug": format!("{other:?}") }),
            Value::Object(Default::default()),
        ),
    }
}

/// `"ContactMsgRecv"` -> `"CONTACT_MSG_RECV"` — converts a Rust enum
/// variant's `Debug` name to the SCREAMING_SNAKE_CASE Python's own
/// `EventType` members use, so `payload_json`'s `"type"` field matches
/// what `meshcore_py`/the bridge would have produced for the same event.
fn screaming_snake_case(pascal_case: &str) -> String {
    let mut out = String::with_capacity(pascal_case.len() + 4);
    for (i, c) in pascal_case.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.extend(c.to_uppercase());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fez_mesh_controller_core::mesh::{CoreStatsDto, PacketStatsDto, RadioStatsDto};
    use fez_mesh_controller_core::{Config, ConnectionConfig, DaemonConfig};
    use meshcore_rs::events::{ChannelMessage, ContactMessage, LogData, MeshPacketHeader};
    use meshcore_rs::packets::RouteType;
    use meshcore_rs::PayloadType;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use tokio::sync::mpsc;

    async fn sample_app_state(command_tx: mpsc::Sender<DaemonCommand>) -> AppState {
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
        AppState::new(
            command_tx,
            config,
            PathBuf::from("/tmp/fez-mesh-controller-test.toml"),
        )
        .await
        .expect("AppState::new with an in-memory DB should never fail")
    }

    fn event(event_type: EventType, payload: EventPayload) -> MeshCoreEvent {
        MeshCoreEvent {
            event_type,
            payload,
            attributes: HashMap::new(),
        }
    }

    // --- screaming_snake_case ---------------------------------------------

    #[test]
    fn screaming_snake_case_converts_pascal_case_variant_names() {
        assert_eq!(screaming_snake_case("ContactMsgRecv"), "CONTACT_MSG_RECV");
        assert_eq!(screaming_snake_case("SelfInfo"), "SELF_INFO");
        assert_eq!(screaming_snake_case("Connected"), "CONNECTED");
        assert_eq!(
            screaming_snake_case("TelemetryResponse"),
            "TELEMETRY_RESPONSE"
        );
    }

    // --- topic_for ----------------------------------------------------------

    #[test]
    fn topic_for_connection_events() {
        assert_eq!(
            topic_for(&event(EventType::Connected, EventPayload::None), "meshcore"),
            Some("meshcore/events/connection".to_string())
        );
        assert_eq!(
            topic_for(
                &event(EventType::Disconnected, EventPayload::None),
                "meshcore"
            ),
            Some("meshcore/events/connection".to_string())
        );
    }

    #[test]
    fn topic_for_direct_message_uses_sender_prefix() {
        let msg = ContactMessage {
            sender_prefix: [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
            path_len: 1,
            txt_type: 0,
            sender_timestamp: 0,
            text: "hi".to_string(),
            snr: None,
            signature: None,
        };
        let e = event(EventType::ContactMsgRecv, EventPayload::ContactMessage(msg));

        assert_eq!(
            topic_for(&e, "meshcore"),
            Some("meshcore/message/direct/aabbccddeeff".to_string())
        );
    }

    #[test]
    fn topic_for_channel_message_uses_channel_idx() {
        let msg = ChannelMessage {
            channel_idx: 3,
            path_len: 1,
            txt_type: 0,
            sender_timestamp: 0,
            text: "hi".to_string(),
            snr: None,
        };
        let e = event(EventType::ChannelMsgRecv, EventPayload::ChannelMessage(msg));

        assert_eq!(
            topic_for(&e, "meshcore"),
            Some("meshcore/message/channel/3".to_string())
        );
    }

    #[test]
    fn topic_for_log_data_is_never_forwarded() {
        let log = LogData {
            snr: 1.0,
            rssi: -80,
            header: Some(MeshPacketHeader {
                route_type: RouteType::Direct,
                payload_type: PayloadType::Control,
                payload_version: 0,
                transport_code: None,
                path_len: 0,
                path_hash_size: 1,
                path: vec![],
            }),
            advertisement: None,
            payload: vec![0x80, 0x04, 0, 0, 0, 0],
        };
        let e = event(EventType::LogData, EventPayload::LogData(log));

        assert_eq!(topic_for(&e, "meshcore"), None);
    }

    #[test]
    fn topic_for_none_for_internal_command_responses() {
        assert_eq!(
            topic_for(&event(EventType::Ok, EventPayload::None), "meshcore"),
            None
        );
        assert_eq!(
            topic_for(&event(EventType::Error, EventPayload::None), "meshcore"),
            None
        );
    }

    // --- payload_json ---------------------------------------------------

    #[test]
    fn payload_json_envelope_shape_and_type_name() {
        let value = payload_json(&event(EventType::Connected, EventPayload::None));

        assert_eq!(value["type"], "EventType.CONNECTED");
        assert!(value.get("payload").is_some());
        assert!(value.get("attributes").is_some());
    }

    #[test]
    fn payload_json_contact_message_fields_and_attributes() {
        let msg = ContactMessage {
            sender_prefix: [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
            path_len: 2,
            txt_type: 0,
            sender_timestamp: 1_700_000_000,
            text: "hello".to_string(),
            snr: Some(5.5),
            signature: None,
        };
        let e = event(EventType::ContactMsgRecv, EventPayload::ContactMessage(msg));

        let value = payload_json(&e);

        assert_eq!(value["type"], "EventType.CONTACT_MSG_RECV");
        assert_eq!(value["payload"]["pubkey_prefix"], "aabbccddeeff");
        assert_eq!(value["payload"]["text"], "hello");
        assert_eq!(value["payload"]["sender_timestamp"], 1_700_000_000);
        assert_eq!(value["payload"]["SNR"], 5.5);
        assert_eq!(value["attributes"]["pubkey_prefix"], "aabbccddeeff");
        assert_eq!(value["attributes"]["txt_type"], 0);
    }

    // --- status_message_json / offline_lwt_payload -------------------------

    fn sample_self_info() -> SelfInfoDto {
        SelfInfoDto {
            name: "F4FEZ_BRIDGE".to_string(),
            public_key_hex: "81364a93ab7221a1280da26d91e7a702c6773925045a49c7782567efa39544d0"
                .to_string(),
            radio_freq_mhz: 869.618,
            radio_bw_khz: 62.5,
            spreading_factor: 8,
            coding_rate: 8,
            tx_power_dbm: 22,
            lat: 0.0,
            lon: 0.0,
        }
    }

    fn sample_device_info() -> DeviceInfoDto {
        DeviceInfoDto {
            model: "Seeed Xiao-nrf52".to_string(),
            firmware_version: "v1.16.0-07a3ca9 (Build: 06-Jun-2026)".to_string(),
        }
    }

    fn sample_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-14T07:53:47.871080+00:00")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn sample_node_stats() -> NodeStatsDto {
        NodeStatsDto {
            core: Some(CoreStatsDto {
                battery_mv: 4012,
                uptime_secs: 123456,
                errors: 0,
                queue_len: 0,
            }),
            radio: Some(RadioStatsDto {
                noise_floor: -120,
                last_rssi: -80,
                last_snr: 8.25,
                tx_air_secs: 120,
                rx_air_secs: 340,
            }),
            packets: Some(PacketStatsDto {
                recv: 1000,
                sent: 500,
                flood_tx: 100,
                direct_tx: 400,
                flood_rx: 200,
                direct_rx: 800,
                recv_errors: Some(3),
            }),
        }
    }

    #[test]
    fn status_message_json_is_online_when_mesh_connected_and_device_info_known() {
        let value = status_message_json(
            true,
            &sample_self_info(),
            Some(&sample_device_info()),
            None,
            sample_now(),
        );

        assert_eq!(value["status"], "online");
        assert_eq!(value["timestamp"], "2026-08-14T07:53:47.871080+00:00");
        assert_eq!(value["origin"], "F4FEZ_BRIDGE");
        assert_eq!(
            value["origin_id"],
            "81364A93AB7221A1280DA26D91E7A702C6773925045A49C7782567EFA39544D0"
        );
        assert_eq!(value["model"], "Seeed Xiao-nrf52");
        assert_eq!(
            value["firmware_version"],
            "v1.16.0-07a3ca9 (Build: 06-Jun-2026)"
        );
        assert_eq!(value["radio"], "869.618,62.5,8,8");
        assert_eq!(value["client_version"], CLIENT_VERSION);
        assert!(value.get("stats").is_none());
    }

    #[test]
    fn status_message_json_includes_stats_when_online_with_packets_aliases() {
        let value = status_message_json(
            true,
            &sample_self_info(),
            Some(&sample_device_info()),
            Some(&sample_node_stats()),
            sample_now(),
        );

        assert_eq!(value["status"], "online");
        let stats = &value["stats"];
        // Flat, matching agessaman's own merged shape -- not nested under
        // "core"/"radio"/"packets".
        assert_eq!(stats["battery_mv"], 4012);
        assert_eq!(stats["noise_floor"], -120);
        assert_eq!(stats["recv"], 1000);
        assert_eq!(stats["recv_errors"], 3);
        // agessaman's own normalize_packet_stats aliases.
        assert_eq!(stats["packets_sent"], 500);
        assert_eq!(stats["packets_received"], 1000);
    }

    #[test]
    fn status_message_json_omits_stats_when_offline_even_if_present() {
        // Mirrors agessaman's own `if status.lower() == "online"` gate.
        let value = status_message_json(
            false,
            &sample_self_info(),
            Some(&sample_device_info()),
            Some(&sample_node_stats()),
            sample_now(),
        );

        assert_eq!(value["status"], "offline");
        assert!(value.get("stats").is_none());
    }

    #[test]
    fn status_message_json_is_offline_and_omits_device_fields_when_device_info_unknown() {
        // Connected, but the device-info query hasn't succeeded (yet, or ever)
        // — per the user's rule, publish only what's known, status offline.
        let value = status_message_json(true, &sample_self_info(), None, None, sample_now());

        assert_eq!(value["status"], "offline");
        assert_eq!(value["origin"], "F4FEZ_BRIDGE");
        assert_eq!(
            value["origin_id"],
            "81364A93AB7221A1280DA26D91E7A702C6773925045A49C7782567EFA39544D0"
        );
        assert!(value.get("model").is_none());
        assert!(value.get("firmware_version").is_none());
        assert_eq!(value["radio"], "869.618,62.5,8,8");
    }

    #[test]
    fn status_message_json_keeps_last_known_device_fields_when_mesh_disconnected() {
        // Mesh disconnected, but self_info/device_info are the last-known
        // (stale) values `AppState` never clears — status flips to offline,
        // the other fields are NOT reset to placeholders.
        let value = status_message_json(
            false,
            &sample_self_info(),
            Some(&sample_device_info()),
            None,
            sample_now(),
        );

        assert_eq!(value["status"], "offline");
        assert_eq!(value["model"], "Seeed Xiao-nrf52");
        assert_eq!(
            value["firmware_version"],
            "v1.16.0-07a3ca9 (Build: 06-Jun-2026)"
        );
        assert_eq!(value["radio"], "869.618,62.5,8,8");
    }

    #[test]
    fn offline_lwt_payload_is_a_strict_subset_of_status_message() {
        let value = offline_lwt_payload(Some(&sample_self_info()), sample_now());

        assert_eq!(value["status"], "offline");
        assert_eq!(value["timestamp"], "2026-08-14T07:53:47.871080+00:00");
        assert_eq!(value["origin"], "F4FEZ_BRIDGE");
        assert_eq!(
            value["origin_id"],
            "81364A93AB7221A1280DA26D91E7A702C6773925045A49C7782567EFA39544D0"
        );
        assert_eq!(
            value.as_object().unwrap().len(),
            4,
            "LWT payload must only carry status/timestamp/origin/origin_id"
        );
    }

    // --- resolve_topic_template / packet_trafic_json / raw_json ------------

    fn sample_broker_config() -> MqttBrokerConfig {
        MqttBrokerConfig {
            name: "Home Assistant".to_string(),
            host: "mqtt.example.com".to_string(),
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

    #[tokio::test]
    async fn wait_for_self_info_returns_immediately_when_already_known() {
        let (command_tx, _command_rx) = mpsc::channel(8);
        let state = sample_app_state(command_tx).await;
        state.snapshot.write().await.self_info = Some(sample_self_info());

        let result = wait_for_self_info(&state, Duration::from_secs(5)).await;

        assert_eq!(
            result.map(|i| i.public_key_hex),
            Some(sample_self_info().public_key_hex)
        );
    }

    #[tokio::test]
    async fn sign_via_mesh_propagates_a_signing_error() {
        let (command_tx, mut command_rx) = mpsc::channel(8);
        tokio::spawn(async move {
            if let Some(DaemonCommand::SignData { reply, .. }) = command_rx.recv().await {
                let _ = reply.send(Err("no mesh connection".to_string()));
            }
        });
        let state = sample_app_state(command_tx).await;

        let result = sign_via_mesh(&state, b"signing input").await;

        assert_eq!(result, Err("no mesh connection".to_string()));
    }

    #[tokio::test]
    async fn build_device_signed_credentials_uses_the_signed_token() {
        let (command_tx, mut command_rx) = mpsc::channel(8);
        tokio::spawn(async move {
            if let Some(DaemonCommand::SignData { reply, .. }) = command_rx.recv().await {
                let _ = reply.send(Ok(vec![0xAB; 64]));
            }
        });
        let state = sample_app_state(command_tx).await;
        let self_info = sample_self_info();
        let config = sample_broker_config();

        let (username, token) = build_device_signed_credentials(&state, &self_info, &config)
            .await
            .expect("signing succeeds");

        assert_eq!(
            username,
            format!("v1_{}", self_info.public_key_hex.to_uppercase())
        );
        assert_eq!(token.matches('.').count(), 2);
        assert!(token.ends_with(&"AB".repeat(64)));
    }

    #[tokio::test]
    async fn build_device_signed_credentials_uses_host_as_default_audience() {
        let (command_tx, mut command_rx) = mpsc::channel(8);
        tokio::spawn(async move {
            if let Some(DaemonCommand::SignData { data, reply }) = command_rx.recv().await {
                // Round-trips the signing input's `aud` claim back out so the
                // test can assert on it without duplicating JWT parsing here.
                let signing_input = String::from_utf8(data).expect("ascii signing input");
                let payload_b64 = signing_input.split('.').nth(1).expect("payload segment");
                let payload_json = base64::Engine::decode(
                    &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                    payload_b64,
                )
                .expect("valid base64url");
                let payload: serde_json::Value =
                    serde_json::from_slice(&payload_json).expect("valid JSON");
                assert_eq!(payload["aud"], "mqtt.example.com");
                let _ = reply.send(Ok(vec![0u8; 64]));
            }
        });
        let state = sample_app_state(command_tx).await;
        let self_info = sample_self_info();
        let config = sample_broker_config(); // jwt_audience: None, host: "mqtt.example.com"

        build_device_signed_credentials(&state, &self_info, &config)
            .await
            .expect("signing succeeds");
    }

    fn packet_log_entry(route_type: RouteType, path_len: u8, payload: Vec<u8>) -> PacketLogEntry {
        let log = LogData {
            snr: 5.5,
            rssi: -90,
            header: Some(MeshPacketHeader {
                route_type,
                payload_type: PayloadType::TextMsg,
                payload_version: 0,
                transport_code: None,
                path_len,
                path_hash_size: 1,
                path: vec![0x11; path_len as usize],
            }),
            advertisement: None,
            payload,
        };
        build_packet_log_entry(&event(EventType::LogData, EventPayload::LogData(log)), 1, 0)
            .unwrap()
    }

    #[test]
    fn resolve_topic_template_substitutes_prefix_and_public_key() {
        let config = sample_broker_config();

        let resolved = resolve_topic_template(
            "{prefix}/{public_key}/packets",
            &config,
            &sample_self_info(),
        );

        assert_eq!(
            resolved,
            "meshcore/81364A93AB7221A1280DA26D91E7A702C6773925045A49C7782567EFA39544D0/packets"
        );
    }

    #[test]
    fn resolve_topic_template_best_effort_uses_device_fallback_when_self_info_missing() {
        let config = sample_broker_config();

        let resolved =
            resolve_topic_template_best_effort("{prefix}/{public_key}/status", &config, None);

        assert_eq!(resolved, "meshcore/DEVICE/status");
    }

    #[test]
    fn resolve_topic_template_best_effort_matches_the_strict_variant_when_self_info_known() {
        let config = sample_broker_config();
        let self_info = sample_self_info();

        let best_effort = resolve_topic_template_best_effort(
            "{prefix}/{public_key}/status",
            &config,
            Some(&self_info),
        );
        let strict = resolve_topic_template("{prefix}/{public_key}/status", &config, &self_info);

        assert_eq!(best_effort, strict);
    }

    #[test]
    fn build_transport_selects_tls_for_tcp_and_wss_for_websocket() {
        let mut config = sample_broker_config();
        config.transport_protocol = MqttTransportProtocol::Tcp;
        assert!(matches!(
            build_transport(&config).unwrap(),
            Transport::Tls(_)
        ));

        config.transport_protocol = MqttTransportProtocol::Websocket;
        assert!(matches!(
            build_transport(&config).unwrap(),
            Transport::Wss(_)
        ));
    }

    #[test]
    fn packet_trafic_json_matches_documented_schema() {
        let entry = packet_log_entry(RouteType::Direct, 2, vec![0xde, 0xad, 0xbe, 0xef]);

        let value = packet_trafic_json(&entry, &sample_self_info(), sample_now()).unwrap();

        assert_eq!(value["origin"], "F4FEZ_BRIDGE");
        assert_eq!(
            value["origin_id"],
            "81364A93AB7221A1280DA26D91E7A702C6773925045A49C7782567EFA39544D0"
        );
        assert_eq!(value["timestamp"], "2026-08-14T07:53:47.871080+00:00");
        assert_eq!(value["type"], "PACKET");
        assert_eq!(value["direction"], "rx");
        assert_eq!(value["time"], "07:53:47");
        assert_eq!(value["date"], "14/08/2026");
        assert_eq!(value["packet_type"], PayloadType::TextMsg as u64);
        assert_eq!(value["route"], "direct");
        assert_eq!(value["payload_len"], 4);
        // header byte(1) + path byte(1) + path bytes(2) + payload(4)
        assert_eq!(value["len"], 8);
        assert_eq!(value["SNR"], 5.5);
        assert_eq!(value["RSSI"], -90);
        // header_byte=0x0a (version=0,TextMsg=2,Direct=2), path_byte=0x02
        // (path_hash_size=1,path_len=2), path=[0x11,0x11], payload=deadbeef.
        assert_eq!(value["raw"], "0a021111deadbeef");
        assert_eq!(value["hash"].as_str().unwrap().len(), 16);
    }

    #[test]
    fn packet_trafic_json_route_is_hop_count_for_flood_routes() {
        let entry = packet_log_entry(RouteType::Flood, 3, vec![0x01]);

        let value = packet_trafic_json(&entry, &sample_self_info(), sample_now()).unwrap();

        assert_eq!(value["route"], "3");
    }

    #[test]
    fn packet_trafic_json_truncates_raw_to_2048_chars() {
        let entry = packet_log_entry(RouteType::Direct, 0, vec![0xab; 2000]);

        let value = packet_trafic_json(&entry, &sample_self_info(), sample_now()).unwrap();

        assert_eq!(value["raw"].as_str().unwrap().len(), 2048);
        // `len` still reflects the true, untruncated packet size.
        assert_eq!(value["len"], 1 + 1 + 2000);
    }

    #[test]
    fn packet_trafic_json_none_when_header_missing() {
        let log = LogData {
            snr: 1.0,
            rssi: -80,
            header: None,
            advertisement: None,
            payload: vec![],
        };
        let entry =
            build_packet_log_entry(&event(EventType::LogData, EventPayload::LogData(log)), 1, 0)
                .unwrap();

        assert!(packet_trafic_json(&entry, &sample_self_info(), sample_now()).is_none());
    }

    #[test]
    fn raw_json_is_a_minimal_uppercase_envelope() {
        let entry = packet_log_entry(RouteType::Direct, 2, vec![0xde, 0xad, 0xbe, 0xef]);

        let value = raw_json(&entry, &sample_self_info(), sample_now()).unwrap();

        assert_eq!(value["type"], "RAW");
        assert_eq!(value["origin"], "F4FEZ_BRIDGE");
        assert_eq!(
            value["origin_id"],
            "81364A93AB7221A1280DA26D91E7A702C6773925045A49C7782567EFA39544D0"
        );
        assert_eq!(value["timestamp"], "2026-08-14T07:53:47.871080+00:00");
        let data = value["data"].as_str().unwrap();
        assert_eq!(data, data.to_uppercase());
        assert_eq!(
            value.as_object().unwrap().len(),
            5,
            "RAW envelope must only carry origin/origin_id/timestamp/type/data"
        );
    }
}
