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

//! Interactive first-run setup wizard, launched automatically when no
//! configuration file is found (or explicitly via `fez-mesh-controller
//! setup`).

use std::time::Duration;

use console::style;
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, Select};
use fez_mesh_controller_core::{Config, ConnectionConfig, DaemonConfig};
use indicatif::{ProgressBar, ProgressStyle};
use names::Generator;

use crate::theme::{self, accent, muted, primary};

const MANUAL_ENTRY: &str = "✏️  Enter manually...";

/// Generates a fun, memorable default name (e.g. "rusty-nail") to suggest
/// as the controller's label.
fn random_node_label() -> String {
    Generator::default()
        .next()
        .unwrap_or_else(|| "fez-mesh-controller".to_string())
}

pub async fn run(existing: Option<&Config>) -> anyhow::Result<Config> {
    theme::section("Setup wizard", "🧙");
    println!(
        "  {}",
        muted().apply_to("Answer the few questions below; you can re-run this wizard")
    );
    println!(
        "  {}",
        muted().apply_to("at any time with `fez-mesh-controller setup`.")
    );

    let dtheme = ColorfulTheme::default();

    let node_label: String = Input::with_theme(&dtheme)
        .with_prompt("🏷️  Name of this controller")
        .default(
            existing
                .map(|c| c.node_label.clone())
                .unwrap_or_else(random_node_label),
        )
        .interact_text()?;

    let connection = ask_connection(&dtheme, existing.map(|c| &c.connection)).await?;

    let socket_path = existing
        .map(|c| c.daemon.socket_path.clone())
        .unwrap_or_else(fez_mesh_controller_core::config::default_socket_path);

    println!();
    let refresh_interval_secs: u64 = Input::with_theme(&dtheme)
        .with_prompt("⏱️  Refresh interval (seconds)")
        .default(
            existing
                .map(|c| c.daemon.refresh_interval_secs)
                .unwrap_or(5),
        )
        .interact_text()?;

    let log_level = existing
        .map(|c| c.daemon.log_level.clone())
        .unwrap_or_else(|| "info".to_string());
    let log_dir = existing
        .map(|c| c.daemon.log_dir.clone())
        .unwrap_or_else(fez_mesh_controller_core::config::default_log_dir);

    let managed_repeaters = existing
        .map(|c| c.managed_repeaters.clone())
        .unwrap_or_default();

    let config = Config {
        node_label,
        connection,
        daemon: DaemonConfig {
            socket_path,
            refresh_interval_secs,
            log_level,
            log_dir,
            packet_log_capacity: fez_mesh_controller_core::config::default_packet_log_capacity(),
        },
        managed_repeaters,
    };

    println!();
    println!("{} {}", style("📝").bold(), primary().apply_to("Summary"));
    println!(
        "   🏷️  Name         : {}",
        accent().apply_to(&config.node_label)
    );
    println!(
        "   📡 Connection   : {}",
        accent().apply_to(config.connection.to_string())
    );
    println!(
        "   🔌 IPC socket   : {}",
        accent().apply_to(config.daemon.socket_path.display())
    );
    println!(
        "   ⏱️  Refresh      : {}",
        accent().apply_to(format!("{}s", config.daemon.refresh_interval_secs))
    );
    println!();

    let confirmed = Confirm::with_theme(&dtheme)
        .with_prompt("💾 Save this configuration?")
        .default(true)
        .interact()?;

    if !confirmed {
        anyhow::bail!("configuration cancelled by user");
    }

    Ok(config)
}

async fn ask_connection(
    dtheme: &ColorfulTheme,
    existing: Option<&ConnectionConfig>,
) -> anyhow::Result<ConnectionConfig> {
    let options = ["📻 Serial (USB / UART)", "🌐 TCP", "📶 Bluetooth (BLE)"];
    let default_idx = match existing {
        Some(ConnectionConfig::Serial { .. }) => 0,
        Some(ConnectionConfig::Tcp { .. }) => 1,
        Some(ConnectionConfig::Ble { .. }) => 2,
        None => 0,
    };

    let choice = Select::with_theme(dtheme)
        .with_prompt("📡 Connection type to the MeshCore node")
        .items(&options)
        .default(default_idx)
        .interact()?;

    match choice {
        0 => ask_serial(dtheme, existing),
        1 => ask_tcp(dtheme, existing),
        _ => ask_ble(dtheme, existing).await,
    }
}

fn ask_serial(
    dtheme: &ColorfulTheme,
    existing: Option<&ConnectionConfig>,
) -> anyhow::Result<ConnectionConfig> {
    let existing_port = match existing {
        Some(ConnectionConfig::Serial { port, .. }) => Some(port.clone()),
        _ => None,
    };

    let available: Vec<String> = serialport::available_ports()
        .map(|ports| ports.into_iter().map(|p| p.port_name).collect())
        .unwrap_or_default();

    let port = if available.is_empty() {
        Input::with_theme(dtheme)
            .with_prompt("🔌 Serial port (e.g. /dev/ttyUSB0, /dev/tty.usbserial-XXXX)")
            .default(existing_port.unwrap_or_else(|| "/dev/ttyUSB0".to_string()))
            .interact_text()?
    } else {
        let mut items: Vec<String> = available.clone();
        items.push(MANUAL_ENTRY.to_string());
        let default_idx = existing_port
            .as_ref()
            .and_then(|p| available.iter().position(|a| a == p))
            .unwrap_or(0);

        let idx = Select::with_theme(dtheme)
            .with_prompt("🔌 Detected serial port")
            .items(&items)
            .default(default_idx)
            .interact()?;

        if idx == items.len() - 1 {
            Input::with_theme(dtheme)
                .with_prompt("🔌 Serial port")
                .default("/dev/ttyUSB0".to_string())
                .interact_text()?
        } else {
            items[idx].clone()
        }
    };

    let baud_rate: u32 = Input::with_theme(dtheme)
        .with_prompt("⚡ Baud rate")
        .default(match existing {
            Some(ConnectionConfig::Serial { baud_rate, .. }) => *baud_rate,
            _ => 115_200,
        })
        .interact_text()?;

    Ok(ConnectionConfig::Serial { port, baud_rate })
}

fn ask_tcp(
    dtheme: &ColorfulTheme,
    existing: Option<&ConnectionConfig>,
) -> anyhow::Result<ConnectionConfig> {
    let host: String = Input::with_theme(dtheme)
        .with_prompt("🌐 Node address (host)")
        .default(match existing {
            Some(ConnectionConfig::Tcp { host, .. }) => host.clone(),
            _ => "192.168.1.50".to_string(),
        })
        .interact_text()?;

    let port: u16 = Input::with_theme(dtheme)
        .with_prompt("🔢 TCP port")
        .default(match existing {
            Some(ConnectionConfig::Tcp { port, .. }) => *port,
            _ => 5000,
        })
        .interact_text()?;

    Ok(ConnectionConfig::Tcp { host, port })
}

async fn ask_ble(
    dtheme: &ColorfulTheme,
    existing: Option<&ConnectionConfig>,
) -> anyhow::Result<ConnectionConfig> {
    let scan = Confirm::with_theme(dtheme)
        .with_prompt("🔍 Scan for nearby MeshCore devices (5s)?")
        .default(true)
        .interact()?;

    let discovered = if scan {
        let spinner = ProgressBar::new_spinner();
        spinner.set_style(ProgressStyle::with_template("{spinner:.cyan} {msg}").unwrap());
        spinner.set_message("Scanning for BLE devices...");
        spinner.enable_steady_tick(Duration::from_millis(100));

        let result = meshcore_rs::MeshCore::ble_discover(Duration::from_secs(5)).await;

        spinner.finish_and_clear();
        match result {
            Ok(devices) if !devices.is_empty() => devices,
            Ok(_) => {
                theme::info_line("no devices found, make sure Bluetooth is enabled");
                Vec::new()
            }
            Err(err) => {
                theme::error_line(&format!("BLE scan failed: {err}"));
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    let name = if discovered.is_empty() {
        Input::with_theme(dtheme)
            .with_prompt("📶 BLE device name")
            .default(match existing {
                Some(ConnectionConfig::Ble { name }) => name.clone(),
                _ => String::new(),
            })
            .interact_text()?
    } else {
        let mut items = discovered.clone();
        items.push(MANUAL_ENTRY.to_string());
        let idx = Select::with_theme(dtheme)
            .with_prompt("📶 Detected device")
            .items(&items)
            .default(0)
            .interact()?;

        if idx == items.len() - 1 {
            Input::with_theme(dtheme)
                .with_prompt("📶 BLE device name")
                .interact_text()?
        } else {
            items[idx].clone()
        }
    };

    Ok(ConnectionConfig::Ble { name })
}
