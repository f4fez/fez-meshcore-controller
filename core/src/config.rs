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

//! fez-mesh-controller configuration: loaded by the CLI (which runs an
//! interactive wizard if the file is missing) and by the daemon.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::error::Result;

pub const APP_DIR_NAME: &str = "fez-mesh-controller";
pub const CONFIG_FILE_NAME: &str = "config.toml";

/// Full application configuration, persisted as TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Display name used to identify this controller in the UIs.
    pub node_label: String,
    /// Connection settings for the MeshCore node.
    pub connection: ConnectionConfig,
    /// Settings for the daemon service.
    pub daemon: DaemonConfig,
    /// Repeaters managed by this application, matched against mesh contacts
    /// by public key so they can be highlighted in the UIs.
    #[serde(default)]
    pub managed_repeaters: Vec<ManagedRepeater>,
    /// The cluster's region hierarchy, mirroring the MeshCore node
    /// firmware's own region concept. Local to this controller — not
    /// synced with the connected node's own configuration.
    #[serde(default)]
    pub regions: Vec<RegionConfig>,
    /// Names of "Hashtag Channels" (`docs/companion_protocol.md`) to try
    /// decoding `GroupText` messages against, in addition to the
    /// well-known "Public" channel — e.g. `"#test"`. A hashtag channel's
    /// key is derived purely from its name
    /// (`crate::meshcore_crypto::hashtag_channel_key`), so like the
    /// Public channel, it's not actually private: "anyone who knows or
    /// guesses the channel name can derive the key."
    #[serde(default)]
    pub hashtag_channels: Vec<String>,
    /// MQTT brokers to forward received mesh events to, in the same topic
    /// structure and JSON envelope as the community `ipnet-mesh/meshcore-mqtt`
    /// bridge (verified against its source, not assumed) plus two extra
    /// topics (`<prefix>/control`, `<prefix>/anon_req`) this project also
    /// decodes. Local to this controller — the connected node itself has no
    /// notion of MQTT.
    #[serde(default)]
    pub mqtt_brokers: Vec<MqttBrokerConfig>,
}

/// An MQTT broker to forward received mesh events to — see
/// [`Config::mqtt_brokers`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MqttBrokerConfig {
    /// Internal identification for this broker (shown in the TUI's Observer
    /// node block); not part of the MQTT protocol itself.
    pub name: String,
    pub host: String,
    #[serde(default = "default_mqtt_port")]
    pub port: u16,
    /// Only used when [`Self::auth_method`] is [`MqttAuthMethod::Passwd`].
    #[serde(default)]
    pub username: Option<String>,
    /// Stored in plaintext, like the rest of this config file — there is no
    /// secret-encryption mechanism here today. Only used when
    /// [`Self::auth_method`] is [`MqttAuthMethod::Passwd`].
    #[serde(default)]
    pub password: Option<String>,
    /// How to authenticate with this broker.
    #[serde(default)]
    pub auth_method: MqttAuthMethod,
    /// Lifetime, in seconds, of a device-signed auth token before it's
    /// refreshed. Only used when [`Self::auth_method`] is
    /// [`MqttAuthMethod::Device`]. The refresh happens 6 minutes before
    /// expiry, matching `Colorado-Mesh/mesh-client`'s own refresh margin.
    #[serde(default = "default_mqtt_jwt_ttl_secs")]
    pub jwt_ttl_secs: u32,
    /// `aud` claim for a device-signed auth token. Defaults to
    /// [`Self::host`] if unset, which is what LetsMesh/MeshMapper expect
    /// (`letsMeshJwtAudience()`: the exact MQTT connect hostname). Only used
    /// when [`Self::auth_method`] is [`MqttAuthMethod::Device`].
    #[serde(default)]
    pub jwt_audience: Option<String>,
    /// Prefix prepended to every published topic (`<prefix>/advertisement`,
    /// `<prefix>/status`, ...), matching `ipnet-mesh/meshcore-mqtt`'s own
    /// `topic_prefix` config key.
    #[serde(default = "default_mqtt_topic_prefix")]
    pub topic_prefix: String,
    #[serde(default)]
    pub tls_enabled: bool,
    /// Path to a CA certificate to trust, in addition to the system's own
    /// trust store. `None` with `tls_enabled` still gets a TLS connection,
    /// just without a custom CA.
    #[serde(default)]
    pub tls_ca_cert: Option<PathBuf>,
    /// Client certificate for mutual TLS. Requires `tls_client_key` too.
    #[serde(default)]
    pub tls_client_cert: Option<PathBuf>,
    /// Private key for `tls_client_cert`.
    #[serde(default)]
    pub tls_client_key: Option<PathBuf>,
    /// How often (in seconds) to republish the retained `<prefix>/status`
    /// message while connected, so its `timestamp` stays fresh for
    /// consumers watching that topic — matches
    /// `agessaman/meshcore-packet-capture`'s own `STATS_REFRESH_INTERVAL`
    /// default. `0` disables the periodic republish (status is still
    /// published on connect/disconnect transitions).
    #[serde(default = "default_mqtt_status_refresh_interval_secs")]
    pub status_refresh_interval_secs: u32,
    /// Whether to publish the decoded-event topics and the `<prefix>/status`
    /// topic at all. `false` mutes this broker entirely (the connection is
    /// still made, but nothing is ever published) — useful for a broker
    /// added in advance of enabling forwarding, or temporarily silenced
    /// without removing its configuration.
    #[serde(default = "default_mqtt_enable_high_level_messages")]
    pub enable_high_level_messages: bool,
    /// Whether to publish the rich `"PACKET"`-schema raw-packet topic (every
    /// overheard packet, decoded metadata + raw hex — matches the format
    /// documented by `Colorado-Mesh/mesh-client`'s `letsmesh-mqtt-auth.md`
    /// and `agessaman/meshcore-packet-capture`'s own `packets` topic).
    #[serde(default = "default_mqtt_enable_packet_trafic_messages")]
    pub enable_packet_trafic_messages: bool,
    /// Topic route for [`Self::enable_packet_trafic_messages`]. Supports
    /// `{prefix}` (this broker's `topic_prefix`) and `{public_key}` (this
    /// node's own uppercase public key hex) placeholders.
    #[serde(default = "default_mqtt_packet_trafic_topic")]
    pub packet_trafic_topic: String,
    /// Whether to publish the minimal `"RAW"`-envelope topic
    /// (`agessaman/meshcore-packet-capture`'s own separate, opt-in-only
    /// `raw` topic — just `{origin, origin_id, timestamp, type, data}`,
    /// `data` being the raw packet hex). Off by default, unlike the other
    /// MQTT topics — matches `agessaman`'s own default (no built-in topic
    /// route for it unless explicitly configured).
    #[serde(default)]
    pub enable_raw_messages: bool,
    /// Topic route for [`Self::enable_raw_messages`]. Same `{prefix}`/
    /// `{public_key}` placeholders as [`Self::packet_trafic_topic`].
    #[serde(default = "default_mqtt_raw_topic")]
    pub raw_topic: String,
    /// Topic route for the `<prefix>/status` topic. Same `{prefix}`/
    /// `{public_key}` placeholders as [`Self::packet_trafic_topic`] —
    /// needed to match consumers (e.g.
    /// `yellowcooln/meshcore-mqtt-live-map`) that require a node-id segment
    /// in every topic, including status.
    #[serde(default = "default_mqtt_status_topic")]
    pub status_topic: String,
    /// Connection protocol to the broker.
    #[serde(default)]
    pub transport_protocol: MqttTransportProtocol,
    /// URL path for the WebSocket connection (e.g. `/mqtt`, `/ws`) — only
    /// used when [`Self::transport_protocol`] is
    /// [`MqttTransportProtocol::Websocket`]. Broker-specific; there is no
    /// universal default across MQTT-over-WebSocket providers.
    #[serde(default = "default_mqtt_websocket_path")]
    pub websocket_path: String,
}

/// Connection protocol to an MQTT broker — see
/// [`MqttBrokerConfig::transport_protocol`]. TLS is layered on top via the
/// existing [`MqttBrokerConfig::tls_enabled`] (`Tcp`+TLS = `mqtts`-style
/// secure TCP; `Websocket`+TLS = `wss`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MqttTransportProtocol {
    #[default]
    Tcp,
    Websocket,
}

/// How to authenticate with an MQTT broker — see
/// [`MqttBrokerConfig::auth_method`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MqttAuthMethod {
    /// [`MqttBrokerConfig::username`]/[`MqttBrokerConfig::password`], the
    /// pre-existing default — either both set, or both left unset for an
    /// anonymous connection under this mode too.
    #[default]
    Passwd,
    /// A MeshCore device-signed token — required by LetsMesh and
    /// MeshMapper's public brokers. The MQTT username is derived as
    /// `v1_<node public key, uppercase hex>`; the password is a JWT-style
    /// token signed on-device (the node's private key is never extracted —
    /// see [`crate::mesh::MeshClient::sign`]), matching
    /// `michaelhart/meshcore-decoder`'s `createAuthToken` format
    /// (`crate::mqtt_jwt`). [`MqttBrokerConfig::username`]/
    /// [`MqttBrokerConfig::password`] are ignored under this mode.
    Device,
    /// Always connects anonymously, ignoring
    /// [`MqttBrokerConfig::username`]/[`MqttBrokerConfig::password`] even if
    /// set.
    None,
}

/// Default MQTT broker port (`ipnet-mesh/meshcore-mqtt`'s own default).
pub fn default_mqtt_port() -> u16 {
    1883
}

/// Default MQTT topic prefix (`ipnet-mesh/meshcore-mqtt`'s own default).
pub fn default_mqtt_topic_prefix() -> String {
    "meshcore".to_string()
}

/// Default MQTT status heartbeat interval, in seconds
/// (`agessaman/meshcore-packet-capture`'s own `STATS_REFRESH_INTERVAL` default).
pub fn default_mqtt_status_refresh_interval_secs() -> u32 {
    300
}

/// Default lifetime, in seconds, of a device-signed MQTT auth token — one
/// hour, refreshed 6 minutes before expiry (see
/// [`MqttBrokerConfig::jwt_ttl_secs`]).
pub fn default_mqtt_jwt_ttl_secs() -> u32 {
    3600
}

/// Default for [`MqttBrokerConfig::enable_high_level_messages`] — publish by
/// default once a broker is configured.
pub fn default_mqtt_enable_high_level_messages() -> bool {
    true
}

/// Default for [`MqttBrokerConfig::enable_packet_trafic_messages`] — on by
/// default, matching the user's request for the primary raw-packet topic.
pub fn default_mqtt_enable_packet_trafic_messages() -> bool {
    true
}

/// Default topic route for [`MqttBrokerConfig::packet_trafic_topic`].
pub fn default_mqtt_packet_trafic_topic() -> String {
    "{prefix}/packets".to_string()
}

/// Default topic route for [`MqttBrokerConfig::raw_topic`].
pub fn default_mqtt_raw_topic() -> String {
    "{prefix}/raw".to_string()
}

/// Default topic route for [`MqttBrokerConfig::status_topic`].
pub fn default_mqtt_status_topic() -> String {
    "{prefix}/status".to_string()
}

/// Default WebSocket path for [`MqttBrokerConfig::websocket_path`].
pub fn default_mqtt_websocket_path() -> String {
    "/mqtt".to_string()
}

/// A region in the cluster's hierarchy, mirroring the MeshCore node
/// firmware's `RegionMap` (name + parent, forming a tree). `parent`
/// references another configured region's `name`, or `None` for a root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionConfig {
    pub name: String,
    #[serde(default)]
    pub parent: Option<String>,
}

/// A repeater managed by this application, identified by name and public key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedRepeater {
    /// Display name for this repeater.
    pub name: String,
    /// Public key (hex-encoded). May be the full 32-byte key or just a
    /// prefix; matched against a contact's public key prefix.
    pub public_key_hex: String,
}

impl ManagedRepeater {
    /// Whether a contact's public key prefix (hex) belongs to this repeater.
    pub fn matches(&self, contact_public_key_prefix_hex: &str) -> bool {
        self.public_key_hex
            .to_ascii_lowercase()
            .starts_with(&contact_public_key_prefix_hex.to_ascii_lowercase())
    }
}

/// Connection method used by the daemon to talk to the MeshCore node.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConnectionConfig {
    /// Serial connection (USB/UART), the most common for a companion radio.
    Serial { port: String, baud_rate: u32 },
    /// TCP connection (e.g. firmware with a network gateway).
    Tcp { host: String, port: u16 },
    /// Bluetooth Low Energy connection, by advertised device name.
    Ble { name: String },
}

impl std::fmt::Display for ConnectionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionConfig::Serial { port, baud_rate } => {
                write!(f, "serial {port} @ {baud_rate} baud")
            }
            ConnectionConfig::Tcp { host, port } => write!(f, "TCP {host}:{port}"),
            ConnectionConfig::Ble { name } => write!(f, "BLE \"{name}\""),
        }
    }
}

/// Settings specific to the daemon service (IPC socket exposed to the CLI, etc).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// Path of the Unix socket used for daemon <-> CLI IPC.
    pub socket_path: PathBuf,
    /// Interval (seconds) between periodic state refreshes.
    pub refresh_interval_secs: u64,
    /// Log level (error, warn, info, debug, trace).
    pub log_level: String,
    /// Directory for the daily-rotating log files written when the daemon
    /// runs in the background (`--daemon`).
    #[serde(default = "default_log_dir")]
    pub log_dir: PathBuf,
    /// Number of raw packets kept in the rotating packet-log cache (TUI
    /// page F3), oldest evicted first.
    #[serde(default = "default_packet_log_capacity")]
    pub packet_log_capacity: usize,
    /// Number of overheard-but-not-yet-a-contact nodes kept in the
    /// discovered-nodes cache, least-recently-seen evicted first.
    #[serde(default = "default_discovered_nodes_capacity")]
    pub discovered_nodes_capacity: usize,
    /// When true, the daemon actively locks this node down to an
    /// observer-only state on every (re)connect: disables the node's own
    /// contact auto-add, removes every channel (Public and
    /// private/hashtag), and prunes any contact that isn't in
    /// [`Config::managed_repeaters`].
    #[serde(default = "default_observer_node_managed_config")]
    pub observer_node_managed_config: bool,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: default_socket_path(),
            refresh_interval_secs: 5,
            log_level: "info".to_string(),
            log_dir: default_log_dir(),
            packet_log_capacity: default_packet_log_capacity(),
            discovered_nodes_capacity: default_discovered_nodes_capacity(),
            observer_node_managed_config: default_observer_node_managed_config(),
        }
    }
}

/// Configuration directory (`~/.config/fez-mesh-controller` on Linux/macOS).
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_DIR_NAME)
}

/// Full path of the default configuration file.
pub fn config_path() -> PathBuf {
    config_dir().join(CONFIG_FILE_NAME)
}

/// Default path of the IPC Unix socket (XDG runtime directory if available).
pub fn default_socket_path() -> PathBuf {
    let base = dirs::runtime_dir().unwrap_or_else(std::env::temp_dir);
    base.join(format!("{APP_DIR_NAME}.sock"))
}

/// Default directory for the daemon's rotating log files (XDG state
/// directory if available, falling back to the local data directory).
pub fn default_log_dir() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join(APP_DIR_NAME)
        .join("logs")
}

/// Default number of entries kept in the rotating packet-log cache.
pub fn default_packet_log_capacity() -> usize {
    500
}

/// Default number of entries kept in the discovered-nodes cache.
pub fn default_discovered_nodes_capacity() -> usize {
    200
}

/// Default for [`DaemonConfig::observer_node_managed_config`] — enforce the
/// observer-only lock by default once a node is connected.
pub fn default_observer_node_managed_config() -> bool {
    true
}

impl Config {
    /// Whether a configuration file already exists at the default location.
    pub fn exists() -> bool {
        config_path().exists()
    }

    /// Loads the configuration from the default location.
    pub fn load() -> Result<Self> {
        Self::load_from(&config_path())
    }

    /// Loads the configuration from a given path.
    pub fn load_from(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&content)?)
    }

    /// Saves the configuration to the default location.
    pub fn save(&self) -> Result<()> {
        self.save_to(&config_path())
    }

    /// Saves the configuration to a given path, creating parent directories
    /// as needed.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config() -> Config {
        Config {
            node_label: "test-node".to_string(),
            connection: ConnectionConfig::Serial {
                port: "/dev/ttyUSB0".to_string(),
                baud_rate: 115_200,
            },
            daemon: DaemonConfig {
                socket_path: PathBuf::from("/tmp/fez-mesh-controller.sock"),
                refresh_interval_secs: 5,
                log_level: "info".to_string(),
                log_dir: PathBuf::from("/tmp/fez-mesh-controller/logs"),
                packet_log_capacity: 500,
                discovered_nodes_capacity: 200,
                observer_node_managed_config: true,
            },
            managed_repeaters: vec![ManagedRepeater {
                name: "F4FEZ Repeater".to_string(),
                public_key_hex: "ab".repeat(32),
            }],
            regions: vec![
                RegionConfig {
                    name: "World".to_string(),
                    parent: None,
                },
                RegionConfig {
                    name: "France".to_string(),
                    parent: Some("World".to_string()),
                },
            ],
            hashtag_channels: vec!["#test".to_string()],
            mqtt_brokers: vec![MqttBrokerConfig {
                name: "Home Assistant".to_string(),
                host: "mqtt.example.com".to_string(),
                port: 8883,
                username: Some("fez".to_string()),
                password: Some("hunter2".to_string()),
                auth_method: MqttAuthMethod::Passwd,
                jwt_ttl_secs: 3600,
                jwt_audience: None,
                topic_prefix: "meshcore".to_string(),
                tls_enabled: true,
                tls_ca_cert: Some(PathBuf::from("/etc/mqtt/ca.pem")),
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
            }],
        }
    }

    #[test]
    fn managed_repeater_matches_is_case_insensitive_prefix() {
        let repeater = ManagedRepeater {
            name: "Repeater".to_string(),
            public_key_hex: "AaBbCc0011223344556677889900aabbccddeeff00112233445566778899aa"
                .to_string(),
        };

        assert!(repeater.matches("aabbcc001122"));
        assert!(repeater.matches("AABBCC001122"));
        assert!(repeater.matches(&repeater.public_key_hex));
        assert!(!repeater.matches("ffffff"));
        assert!(!repeater.matches("aabbcc001123"));
    }

    #[test]
    fn managed_repeater_matches_empty_prefix() {
        let repeater = ManagedRepeater {
            name: "Repeater".to_string(),
            public_key_hex: "ab".repeat(32),
        };
        // Every key starts with the empty prefix.
        assert!(repeater.matches(""));
    }

    #[test]
    fn connection_config_display() {
        assert_eq!(
            ConnectionConfig::Serial {
                port: "/dev/ttyUSB0".to_string(),
                baud_rate: 115_200,
            }
            .to_string(),
            "serial /dev/ttyUSB0 @ 115200 baud"
        );
        assert_eq!(
            ConnectionConfig::Tcp {
                host: "192.168.1.42".to_string(),
                port: 5000,
            }
            .to_string(),
            "TCP 192.168.1.42:5000"
        );
        assert_eq!(
            ConnectionConfig::Ble {
                name: "MeshCore-XXXXXX".to_string(),
            }
            .to_string(),
            "BLE \"MeshCore-XXXXXX\""
        );
    }

    #[test]
    fn daemon_config_default_uses_500_packet_log_capacity() {
        assert_eq!(DaemonConfig::default().packet_log_capacity, 500);
    }

    #[test]
    fn daemon_config_default_uses_200_discovered_nodes_capacity() {
        assert_eq!(DaemonConfig::default().discovered_nodes_capacity, 200);
    }

    #[test]
    fn daemon_config_default_enables_observer_node_managed_config() {
        assert!(DaemonConfig::default().observer_node_managed_config);
    }

    #[test]
    fn config_save_and_load_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let original = sample_config();

        original.save_to(&path).expect("save_to");
        let loaded = Config::load_from(&path).expect("load_from");

        assert_eq!(loaded.node_label, original.node_label);
        assert_eq!(loaded.daemon.packet_log_capacity, 500);
        assert_eq!(loaded.managed_repeaters.len(), 1);
        assert_eq!(loaded.managed_repeaters[0].name, "F4FEZ Repeater");
        assert_eq!(loaded.regions.len(), 2);
        assert_eq!(loaded.regions[0].name, "World");
        assert_eq!(loaded.regions[0].parent, None);
        assert_eq!(loaded.regions[1].name, "France");
        assert_eq!(loaded.regions[1].parent.as_deref(), Some("World"));
        assert_eq!(loaded.hashtag_channels, vec!["#test".to_string()]);
        assert_eq!(loaded.mqtt_brokers, original.mqtt_brokers);
        match loaded.connection {
            ConnectionConfig::Serial { port, baud_rate } => {
                assert_eq!(port, "/dev/ttyUSB0");
                assert_eq!(baud_rate, 115_200);
            }
            other => panic!("expected Serial connection, got {other:?}"),
        }
    }

    #[test]
    fn config_missing_managed_repeaters_defaults_to_empty() {
        // Older config files predate `managed_repeaters`; loading one
        // without the key must not fail.
        let toml = r#"
            node_label = "legacy-node"

            [connection]
            type = "tcp"
            host = "127.0.0.1"
            port = 5000

            [daemon]
            socket_path = "/tmp/fez-mesh-controller.sock"
            refresh_interval_secs = 5
            log_level = "info"
        "#;
        let config: Config = toml::from_str(toml).expect("parse legacy config");
        assert!(config.managed_repeaters.is_empty());
        assert!(config.regions.is_empty());
        assert!(config.hashtag_channels.is_empty());
        assert!(config.mqtt_brokers.is_empty());
        assert_eq!(config.daemon.packet_log_capacity, 500);
        assert_eq!(config.daemon.discovered_nodes_capacity, 200);
        assert_eq!(config.daemon.log_dir, default_log_dir());
        assert!(config.daemon.observer_node_managed_config);
    }

    #[test]
    fn region_config_parent_defaults_to_none_when_omitted() {
        let toml = r#"
            name = "World"
        "#;
        let region: RegionConfig = toml::from_str(toml).expect("parse region without parent");
        assert_eq!(region.name, "World");
        assert_eq!(region.parent, None);
    }

    #[test]
    fn config_load_from_missing_file_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("does-not-exist.toml");
        assert!(Config::load_from(&path).is_err());
    }

    #[test]
    fn mqtt_broker_config_defaults_port_and_topic_prefix_when_omitted() {
        let toml = r#"
            name = "Home Assistant"
            host = "mqtt.example.com"
        "#;
        let broker: MqttBrokerConfig = toml::from_str(toml).expect("parse minimal broker");
        assert_eq!(broker.port, 1883);
        assert_eq!(broker.topic_prefix, "meshcore");
        assert_eq!(broker.username, None);
        assert_eq!(broker.password, None);
        assert!(!broker.tls_enabled);
        assert_eq!(broker.tls_ca_cert, None);
        assert_eq!(broker.status_refresh_interval_secs, 300);
        assert!(broker.enable_high_level_messages);
        assert!(broker.enable_packet_trafic_messages);
        assert_eq!(broker.packet_trafic_topic, "{prefix}/packets");
        assert!(!broker.enable_raw_messages);
        assert_eq!(broker.raw_topic, "{prefix}/raw");
        assert_eq!(broker.status_topic, "{prefix}/status");
        assert_eq!(broker.transport_protocol, MqttTransportProtocol::Tcp);
        assert_eq!(broker.websocket_path, "/mqtt");
        assert_eq!(broker.auth_method, MqttAuthMethod::Passwd);
        assert_eq!(broker.jwt_ttl_secs, 3600);
        assert_eq!(broker.jwt_audience, None);
    }

    #[test]
    fn mqtt_broker_config_parses_device_auth_method() {
        let toml = r#"
            name = "LetsMesh"
            host = "mqtt-us-v1.letsmesh.net"
            auth_method = "device"
            jwt_ttl_secs = 1800
            jwt_audience = "mqtt-us-v1.letsmesh.net"
        "#;
        let broker: MqttBrokerConfig = toml::from_str(toml).expect("parse device-signed broker");
        assert_eq!(broker.auth_method, MqttAuthMethod::Device);
        assert_eq!(broker.jwt_ttl_secs, 1800);
        assert_eq!(
            broker.jwt_audience,
            Some("mqtt-us-v1.letsmesh.net".to_string())
        );
    }

    #[test]
    fn mqtt_broker_config_parses_none_auth_method() {
        let toml = r#"
            name = "Public"
            host = "mqtt.example.com"
            auth_method = "none"
        "#;
        let broker: MqttBrokerConfig = toml::from_str(toml).expect("parse anonymous broker");
        assert_eq!(broker.auth_method, MqttAuthMethod::None);
    }

    #[test]
    fn mqtt_broker_config_round_trips_tls_settings() {
        let broker = MqttBrokerConfig {
            name: "Home Assistant".to_string(),
            host: "mqtt.example.com".to_string(),
            port: 8883,
            username: Some("fez".to_string()),
            password: Some("hunter2".to_string()),
            auth_method: MqttAuthMethod::Passwd,
            jwt_ttl_secs: 3600,
            jwt_audience: None,
            topic_prefix: "meshcore".to_string(),
            tls_enabled: true,
            tls_ca_cert: Some(PathBuf::from("/etc/mqtt/ca.pem")),
            tls_client_cert: Some(PathBuf::from("/etc/mqtt/client.pem")),
            tls_client_key: Some(PathBuf::from("/etc/mqtt/client.key")),
            status_refresh_interval_secs: 600,
            enable_high_level_messages: false,
            enable_packet_trafic_messages: false,
            packet_trafic_topic: "custom/packets".to_string(),
            enable_raw_messages: true,
            raw_topic: "custom/raw".to_string(),
            status_topic: "custom/status".to_string(),
            transport_protocol: MqttTransportProtocol::Websocket,
            websocket_path: "/ws".to_string(),
        };

        let toml = toml::to_string_pretty(&broker).expect("serialize");
        let reloaded: MqttBrokerConfig = toml::from_str(&toml).expect("deserialize");

        assert_eq!(reloaded, broker);
    }
}
