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

//! `fez-mesh-controller repeater` commands: manage repeaters from the
//! command line the same way the TUI's contact list does (mark/unmark a
//! contact as managed, or remove it from the companion entirely), talking
//! to the same running daemon over IPC.

use dialoguer::theme::ColorfulTheme;
use dialoguer::Confirm;
use fez_mesh_controller_core::ipc::{ClientMessage, ServerMessage, Snapshot};
use fez_mesh_controller_core::mesh::{ContactDto, TelemetryDto};
use fez_mesh_controller_core::{Config, RepeaterStatus};

use crate::format::{format_coords, format_last_seen};
use crate::ipc_client::{connect_and_await_snapshot, IpcConnection};
use crate::theme::{self, accent, muted};
use crate::tui::repeater_filter::{repeater_group, RepeaterGroup};

/// Prints every contact known to the daemon (registered or merely
/// discovered) with the same status shown in the TUI's contact list.
pub async fn list(config: &Config) -> anyhow::Result<()> {
    theme::section("Repeaters", "🛰️");
    let (_conn, snapshot) = connect_and_await_snapshot(&config.daemon.socket_path).await?;
    print_repeaters(&snapshot);
    Ok(())
}

/// Declares a new repeater directly from its full public key, without
/// requiring it to have been heard on the mesh first (unlike [`manage`],
/// which can only resolve a full key for a node already known). Optionally
/// also marks it as managed in the same step.
pub async fn add(
    config: &Config,
    name: &str,
    public_key_hex: &str,
    manage: bool,
) -> anyhow::Result<()> {
    theme::section("Add repeater", "➕");

    if public_key_hex.len() != 64 || !public_key_hex.chars().all(|c| c.is_ascii_hexdigit()) {
        anyhow::bail!(
            "a full 32-byte public key is required (64 hex characters, got {})",
            public_key_hex.len()
        );
    }
    let public_key_prefix_hex: String = public_key_hex.chars().take(12).collect();

    let (mut conn, snapshot) = connect_and_await_snapshot(&config.daemon.socket_path).await?;
    if let Some(existing) = snapshot.contacts.iter().find(|c| {
        c.public_key_prefix_hex
            .eq_ignore_ascii_case(&public_key_prefix_hex)
    }) {
        if existing.registered && (existing.managed || !manage) {
            theme::info_line(&format!("\"{}\" is already registered", existing.name));
            return Ok(());
        }
    }

    theme::info_line(&format!("declaring \"{name}\"…"));
    conn.send(&ClientMessage::AddRepeater {
        public_key_hex: public_key_hex.to_string(),
        name: name.to_string(),
        managed: manage,
    })
    .await?;

    match wait_for_outcome(&mut conn, &public_key_prefix_hex).await? {
        Some(c) if c.registered && (c.managed || !manage) => {
            let suffix = if c.managed { " (managed)" } else { "" };
            theme::success_line(&format!("🛰️  \"{}\" added{suffix}", c.name));
        }
        Some(c) => managed_state_mismatch(&c.name),
        None => contact_not_found_after_update(),
    }
    Ok(())
}

/// Marks a contact as managed, registering it with the node first if it
/// isn't already a companion contact (fails if the repeater has never been
/// heard on the mesh, same as the TUI's `m` key).
pub async fn manage(config: &Config, prefix: &str, name: Option<String>) -> anyhow::Result<()> {
    theme::section("Manage repeater", "🛰️");
    let (mut conn, snapshot) = connect_and_await_snapshot(&config.daemon.socket_path).await?;
    let contact = find_contact(&snapshot, prefix)?;
    let display_name = name.unwrap_or_else(|| contact.name.clone());

    if contact.managed {
        theme::info_line(&format!("\"{display_name}\" is already managed"));
        return Ok(());
    }

    theme::info_line(&format!("marking \"{display_name}\" as managed…"));
    conn.send(&ClientMessage::SetRepeaterStatus {
        public_key_prefix_hex: contact.public_key_prefix_hex.clone(),
        name: display_name,
        status: Some(RepeaterStatus::Managed),
    })
    .await?;

    match wait_for_outcome(&mut conn, &contact.public_key_prefix_hex).await? {
        Some(c) if c.managed => {
            theme::success_line(&format!("🛰️  \"{}\" is now managed", c.name));
        }
        Some(c) => managed_state_mismatch(&c.name),
        None => contact_not_found_after_update(),
    }
    Ok(())
}

/// Unmarks a contact as managed. It stays registered with the node — only
/// [`remove`] actually deletes a companion contact.
pub async fn unmanage(config: &Config, prefix: &str) -> anyhow::Result<()> {
    theme::section("Unmanage repeater", "🛰️");
    let (mut conn, snapshot) = connect_and_await_snapshot(&config.daemon.socket_path).await?;
    let contact = find_contact(&snapshot, prefix)?;

    if !contact.managed {
        theme::info_line(&format!("\"{}\" is not managed", contact.name));
        return Ok(());
    }

    theme::info_line(&format!("unmarking \"{}\" as managed…", contact.name));
    conn.send(&ClientMessage::SetRepeaterStatus {
        public_key_prefix_hex: contact.public_key_prefix_hex.clone(),
        name: contact.name.clone(),
        status: None,
    })
    .await?;

    match wait_for_outcome(&mut conn, &contact.public_key_prefix_hex).await? {
        Some(c) if !c.managed => {
            theme::success_line(&format!("\"{}\" is no longer managed", c.name));
        }
        Some(c) => managed_state_mismatch(&c.name),
        None => contact_not_found_after_update(),
    }
    Ok(())
}

/// Permanently removes a contact from the node's own contact list, same as
/// the TUI's `d` key. Asks for confirmation unless `yes` is set.
pub async fn remove(config: &Config, prefix: &str, yes: bool) -> anyhow::Result<()> {
    theme::section("Remove repeater", "🗑️");
    let (mut conn, snapshot) = connect_and_await_snapshot(&config.daemon.socket_path).await?;
    let contact = find_contact(&snapshot, prefix)?;

    if !yes {
        let confirmed = Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt(format!(
                "⚠️  Permanently remove \"{}\" from the node?",
                contact.name
            ))
            .default(false)
            .interact()?;
        if !confirmed {
            theme::info_line("cancelled");
            return Ok(());
        }
    }

    theme::info_line(&format!("removing \"{}\"…", contact.name));
    conn.send(&ClientMessage::RemoveContact {
        public_key_prefix_hex: contact.public_key_prefix_hex.clone(),
    })
    .await?;

    match wait_for_outcome(&mut conn, &contact.public_key_prefix_hex).await? {
        Some(c) if c.registered => {
            theme::error_line(&format!(
                "\"{}\" still appears registered — try again",
                c.name
            ));
            std::process::exit(1);
        }
        _ => theme::success_line(&format!("\"{}\" removed from the node", contact.name)),
    }
    Ok(())
}

/// Fetches and prints fresh telemetry (battery voltage, temperature, ...)
/// from a repeater over the mesh — logs in first if it has a `password`
/// configured (see `ManagedRepeater::password`); most repeaters require
/// this. Same on-demand action as the TUI's `t` key. Can take a while (a
/// real multi-hop mesh round trip), unlike every other `repeater` command.
pub async fn telemetry(config: &Config, prefix: &str) -> anyhow::Result<()> {
    theme::section("Repeater telemetry", "📡");
    let (mut conn, snapshot) = connect_and_await_snapshot(&config.daemon.socket_path).await?;
    let contact = find_contact(&snapshot, prefix)?;

    theme::info_line(&format!(
        "requesting telemetry from \"{}\"… (this can take a while)",
        contact.name
    ));
    conn.send(&ClientMessage::RequestTelemetry {
        public_key_prefix_hex: contact.public_key_prefix_hex.clone(),
    })
    .await?;

    loop {
        match conn.recv().await? {
            Some(ServerMessage::Error(reason)) => {
                theme::error_line(&reason);
                std::process::exit(1);
            }
            Some(ServerMessage::TelemetryResult { telemetry, .. }) => {
                print_telemetry(&contact.name, &telemetry);
                return Ok(());
            }
            Some(_) => continue,
            None => anyhow::bail!("daemon closed the connection"),
        }
    }
}

fn print_telemetry(name: &str, telemetry: &TelemetryDto) {
    println!();
    println!(
        "{} {} {}",
        console::style("📡").bold(),
        theme::primary().apply_to(name),
        muted().apply_to(format!(
            "(fetched {})",
            format_last_seen(telemetry.fetched_at_unix.max(0) as u32)
        ))
    );
    if telemetry.readings.is_empty() {
        println!("   {}", muted().apply_to("no readings"));
    }
    for reading in &telemetry.readings {
        println!(
            "   • {:<22} {}",
            accent().apply_to(&reading.label),
            format!("{} {}", reading.value, reading.unit).trim(),
        );
    }
    println!();
}

fn managed_state_mismatch(name: &str) {
    theme::error_line(&format!(
        "\"{name}\" doesn't reflect the change yet — check `repeater list` or try again"
    ));
    std::process::exit(1);
}

fn contact_not_found_after_update() {
    theme::error_line("contact no longer found after the update");
    std::process::exit(1);
}

/// Finds the single contact whose public key prefix starts with `input`
/// (case-insensitive), so an abbreviated prefix can be used as long as
/// it's unambiguous.
fn find_contact(snapshot: &Snapshot, input: &str) -> anyhow::Result<ContactDto> {
    let needle = input.trim().to_ascii_lowercase();
    if needle.is_empty() {
        anyhow::bail!("a public key prefix is required (see `repeater list`)");
    }

    let mut matches: Vec<&ContactDto> = snapshot
        .contacts
        .iter()
        .filter(|c| {
            c.public_key_prefix_hex
                .to_ascii_lowercase()
                .starts_with(&needle)
        })
        .collect();

    match matches.len() {
        0 => anyhow::bail!("no known contact matches prefix \"{input}\" (see `repeater list`)"),
        1 => Ok(matches.remove(0).clone()),
        n => anyhow::bail!(
            "prefix \"{input}\" is ambiguous ({n} matches) — provide more hex characters"
        ),
    }
}

/// Sends a fresh snapshot request right after a mutating command (the
/// daemon processes messages from one connection strictly in order, so
/// this is guaranteed to reflect the command's outcome) and returns the
/// resulting state of the given contact, if still present.
async fn wait_for_outcome(
    conn: &mut IpcConnection,
    public_key_prefix_hex: &str,
) -> anyhow::Result<Option<ContactDto>> {
    conn.send(&ClientMessage::RequestSnapshot).await?;

    loop {
        match conn.recv().await? {
            Some(ServerMessage::Error(reason)) => {
                theme::error_line(&reason);
                std::process::exit(1);
            }
            Some(ServerMessage::Snapshot(snapshot)) => {
                return Ok(snapshot
                    .contacts
                    .into_iter()
                    .find(|c| c.public_key_prefix_hex == public_key_prefix_hex));
            }
            Some(_) => continue,
            None => anyhow::bail!("daemon closed the connection"),
        }
    }
}

fn print_repeaters(snap: &Snapshot) {
    if snap.contacts.is_empty() {
        println!("   {}", muted().apply_to("no known contacts yet"));
        println!();
        return;
    }

    let mut contacts: Vec<&ContactDto> = snap.contacts.iter().collect();
    contacts.sort_by_key(|c| std::cmp::Reverse(c.last_advert_unix));

    for c in contacts {
        let (status_text, status_style) = match repeater_group(c) {
            RepeaterGroup::Managed => ("🛰️  managed", theme::success()),
            RepeaterGroup::Supervised => ("🛡️  supervised", accent()),
            RepeaterGroup::Known => ("✅ known", muted()),
            RepeaterGroup::Discovered => ("🔍 discovered", theme::warning()),
        };
        println!(
            "   • {:<20} {:<14} {} {} {}",
            accent().apply_to(&c.name),
            status_style.apply_to(status_text),
            muted().apply_to(format!("[{}]", c.public_key_prefix_hex)),
            format_last_seen(c.last_advert_unix),
            muted().apply_to(format_coords(c.lat, c.lon)),
        );
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contact(name: &str, prefix: &str) -> ContactDto {
        ContactDto {
            name: name.to_string(),
            public_key_prefix_hex: prefix.to_string(),
            last_advert_unix: 0,
            lat: 0.0,
            lon: 0.0,
            registered: true,
            managed: false,
            repeater_status: None,
            contact_type: 2, // Repeater
            last_telemetry: None,
        }
    }

    fn snapshot(contacts: Vec<ContactDto>) -> Snapshot {
        Snapshot {
            contacts,
            ..Snapshot::default()
        }
    }

    #[test]
    fn find_contact_matches_exact_prefix() {
        let snap = snapshot(vec![contact("Repeater A", "aabbccddeeff")]);
        let found = find_contact(&snap, "aabbccddeeff").unwrap();
        assert_eq!(found.name, "Repeater A");
    }

    #[test]
    fn find_contact_matches_case_insensitively() {
        let snap = snapshot(vec![contact("Repeater A", "aabbccddeeff")]);
        let found = find_contact(&snap, "AABBCC").unwrap();
        assert_eq!(found.name, "Repeater A");
    }

    #[test]
    fn find_contact_accepts_an_abbreviated_unambiguous_prefix() {
        let snap = snapshot(vec![
            contact("Repeater A", "aabbccddeeff"),
            contact("Repeater B", "112233445566"),
        ]);
        let found = find_contact(&snap, "aabb").unwrap();
        assert_eq!(found.name, "Repeater A");
    }

    #[test]
    fn find_contact_rejects_an_ambiguous_prefix() {
        let snap = snapshot(vec![
            contact("Repeater A", "aabbccddeeff"),
            contact("Repeater A2", "aabbcc001122"),
        ]);
        assert!(find_contact(&snap, "aabbcc").is_err());
    }

    #[test]
    fn find_contact_errors_when_nothing_matches() {
        let snap = snapshot(vec![contact("Repeater A", "aabbccddeeff")]);
        assert!(find_contact(&snap, "ffffff").is_err());
    }

    #[test]
    fn find_contact_errors_on_empty_input() {
        let snap = snapshot(vec![contact("Repeater A", "aabbccddeeff")]);
        assert!(find_contact(&snap, "").is_err());
        assert!(find_contact(&snap, "   ").is_err());
    }
}
