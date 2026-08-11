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
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: default_socket_path(),
            refresh_interval_secs: 5,
            log_level: "info".to_string(),
            log_dir: default_log_dir(),
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
