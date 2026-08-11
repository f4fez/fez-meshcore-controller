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
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: default_socket_path(),
            refresh_interval_secs: 5,
            log_level: "info".to_string(),
            log_dir: default_log_dir(),
            packet_log_capacity: default_packet_log_capacity(),
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
            },
            managed_repeaters: vec![ManagedRepeater {
                name: "F4FEZ Repeater".to_string(),
                public_key_hex: "ab".repeat(32),
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
        assert_eq!(config.daemon.packet_log_capacity, 500);
        assert_eq!(config.daemon.log_dir, default_log_dir());
    }

    #[test]
    fn config_load_from_missing_file_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("does-not-exist.toml");
        assert!(Config::load_from(&path).is_err());
    }
}
