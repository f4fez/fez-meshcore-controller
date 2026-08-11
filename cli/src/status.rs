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

//! `fez-mesh-controller status` command: a single, non-interactive snapshot.

use console::style;
use fez_mesh_controller_core::ipc::{ServerMessage, Snapshot};
use fez_mesh_controller_core::Config;

use crate::format::{format_coords, format_last_seen};
use crate::ipc_client::IpcConnection;
use crate::theme::{self, accent, muted, primary};

pub async fn run(config: &Config) -> anyhow::Result<()> {
    theme::section("Network status", "📊");

    let mut conn = match IpcConnection::connect(&config.daemon.socket_path).await {
        Ok(conn) => conn,
        Err(err) => {
            theme::error_line(&format!("could not reach the daemon: {err}"));
            theme::info_line("is the daemon running? (`fez-mesh-controller-daemon`)");
            std::process::exit(1);
        }
    };

    let snapshot = loop {
        match conn.recv().await? {
            Some(ServerMessage::Snapshot(s)) => break s,
            Some(_) => continue,
            None => anyhow::bail!("daemon closed the connection before sending a snapshot"),
        }
    };

    print_snapshot(&snapshot);
    Ok(())
}

fn print_snapshot(snap: &Snapshot) {
    let dot = if snap.mesh_connected {
        style("🟢").green()
    } else {
        style("🔴").red()
    };
    println!(
        "  {dot} MeshCore node: {}",
        if snap.mesh_connected {
            accent().apply_to("connected")
        } else {
            muted().apply_to("disconnected")
        }
    );
    println!("  ⏳ Daemon uptime: {}s", snap.uptime_secs);

    if let Some(info) = &snap.self_info {
        println!();
        println!(
            "{} {}",
            style("🛰️").bold(),
            primary().apply_to("Local node")
        );
        println!("   🏷️  Name       : {}", accent().apply_to(&info.name));
        println!(
            "   🔑 Public key : {}",
            muted().apply_to(&info.public_key_hex[..12.min(info.public_key_hex.len())])
        );
        println!(
            "   📶 Radio      : {:.3} MHz · SF{} · CR{} · {} dBm",
            info.radio_freq_mhz, info.spreading_factor, info.coding_rate, info.tx_power_dbm
        );
        println!("   🌍 Position   : {}", format_coords(info.lat, info.lon));
    }

    println!();
    println!(
        "{} {} ({})",
        style("👥").bold(),
        primary().apply_to("Contacts"),
        snap.contacts.len()
    );
    if snap.contacts.is_empty() {
        println!("   {}", muted().apply_to("no known contacts yet"));
    }
    for c in &snap.contacts {
        let last_seen = format_last_seen(c.last_advert_unix);
        println!(
            "   • {:<20} {} {last_seen}",
            accent().apply_to(&c.name),
            muted().apply_to(format!("[{}]", c.public_key_prefix_hex)),
        );
    }
    println!();
}
