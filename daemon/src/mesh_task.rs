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

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use fez_mesh_controller_core::ipc::MeshEvent;
use fez_mesh_controller_core::mesh::{
    build_packet_log_entry, extract_discovered_node, is_repeater_or_room, map_event, ContactDto,
    MeshClient, MeshEventKind,
};
use fez_mesh_controller_core::{ConnectionConfig, ManagedRepeater};
use futures::StreamExt;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::command::DaemonCommand;
use crate::state::AppState;

const RECONNECT_DELAY: Duration = Duration::from_secs(5);

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Connection loop for the MeshCore node: (re)connects indefinitely, updates
/// the shared snapshot and broadcasts received events.
pub async fn run(
    connection: ConnectionConfig,
    refresh_interval: Duration,
    mut command_rx: mpsc::Receiver<DaemonCommand>,
    state: Arc<AppState>,
) {
    loop {
        info!(target = %connection, "connecting to MeshCore node...");
        match MeshClient::connect(&connection).await {
            Ok(client) => {
                info!("connected to MeshCore node");
                {
                    let mut snap = state.snapshot.write().await;
                    snap.mesh_connected = true;
                    snap.self_info = client.self_info().await;
                    snap.generated_at_unix = now_unix();
                }
                state.set_device_info(client.device_info().await).await;
                refresh_snapshot_contacts(&client, &state).await;
                state.broadcast_event(MeshEvent {
                    at_unix: now_unix(),
                    kind: MeshEventKind::Connected,
                });

                reconcile_managed_repeaters(&client, &state).await;

                let mut events = client.event_stream();
                let mut ticker = tokio::time::interval(refresh_interval);
                ticker.tick().await; // the first tick fires immediately

                let already_notified_disconnect;
                loop {
                    tokio::select! {
                        maybe_event = events.next() => {
                            match maybe_event {
                                Some(raw) => {
                                    if let Some(node) = extract_discovered_node(&raw, now_unix()) {
                                        if is_repeater_or_room(node.adv_type) {
                                            handle_discovered_node(node, &client, &state).await;
                                        }
                                    }

                                    if let Some(entry) =
                                        build_packet_log_entry(&raw, state.next_packet_id(), now_unix())
                                    {
                                        state.record_packet(entry).await;
                                    }

                                    // Every raw event, regardless of whether
                                    // `map_event` below recognizes it —
                                    // MQTT forwarding (see `crate::mqtt`)
                                    // covers some event types (e.g. Control,
                                    // AnonReq, both carried via LogData) that
                                    // `MeshEventKind` doesn't.
                                    state.broadcast_raw_event(Arc::new(raw.clone()));

                                    let Some(kind) = map_event(&raw) else { continue };
                                    let is_disconnect = matches!(kind, MeshEventKind::Disconnected);
                                    let refresh_contacts = matches!(
                                        kind,
                                        MeshEventKind::NewContact { .. } | MeshEventKind::Advertisement { .. }
                                    );

                                    if refresh_contacts {
                                        refresh_snapshot_contacts(&client, &state).await;
                                    }

                                    state.broadcast_event(MeshEvent { at_unix: now_unix(), kind });

                                    if is_disconnect {
                                        already_notified_disconnect = true;
                                        break;
                                    }
                                }
                                None => {
                                    already_notified_disconnect = false;
                                    break;
                                }
                            }
                        }
                        _ = ticker.tick() => {
                            let mut snap = state.snapshot.write().await;
                            snap.self_info = client.self_info().await;
                            snap.uptime_secs = state.uptime_secs();
                            snap.generated_at_unix = now_unix();
                        }
                        Some(cmd) = command_rx.recv() => {
                            handle_command(cmd, &client, &state).await;
                        }
                    }
                }

                let _ = client.disconnect().await;
                if !already_notified_disconnect {
                    state.broadcast_event(MeshEvent {
                        at_unix: now_unix(),
                        kind: MeshEventKind::Disconnected,
                    });
                }
            }
            Err(err) => {
                warn!(error = %err, "failed to connect to MeshCore node");
            }
        }

        {
            let mut snap = state.snapshot.write().await;
            snap.mesh_connected = false;
            snap.generated_at_unix = now_unix();
        }

        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

/// Rebuilds the contact list shown to clients: the companion's own
/// contacts (annotated with whether each is managed), plus any discovered
/// repeater not already among them.
async fn build_snapshot_contacts(client: &MeshClient, state: &AppState) -> Vec<ContactDto> {
    let managed_repeaters = state.config.read().await.managed_repeaters.clone();
    let mut contacts = client.contacts().await;
    for contact in &mut contacts {
        contact.managed = managed_repeaters
            .iter()
            .any(|r| r.matches(&contact.public_key_prefix_hex));
    }

    let known_prefixes: HashSet<String> = contacts
        .iter()
        .map(|c| c.public_key_prefix_hex.clone())
        .collect();

    let discovered = state.discovered_repeaters.read().await;
    for node in discovered.values() {
        if known_prefixes.contains(&node.public_key_prefix_hex) {
            continue;
        }
        contacts.push(ContactDto {
            name: node.name.clone(),
            public_key_prefix_hex: node.public_key_prefix_hex.clone(),
            last_advert_unix: node.last_seen_unix.max(0) as u32,
            lat: node.lat,
            lon: node.lon,
            registered: false,
            managed: managed_repeaters
                .iter()
                .any(|r| r.matches(&node.public_key_prefix_hex)),
            contact_type: node.adv_type,
        });
    }

    contacts
}

async fn refresh_snapshot_contacts(client: &MeshClient, state: &AppState) {
    let contacts = build_snapshot_contacts(client, state).await;
    let mut snap = state.snapshot.write().await;
    snap.contacts = contacts;
    snap.generated_at_unix = now_unix();
}

/// Records a newly-overheard node (full identity resolved from RF log
/// data) in the (capacity-bounded) discovered-nodes cache and, if it
/// isn't already known, surfaces it in the contact list as "discovered"
/// and logs a one-off event the first time it's seen.
async fn handle_discovered_node(
    node: fez_mesh_controller_core::mesh::DiscoveredNode,
    client: &MeshClient,
    state: &AppState,
) {
    let is_new = state.upsert_discovered_node(node.clone()).await;

    if !is_new {
        return;
    }

    // Already a real contact? Nothing to surface as "discovered".
    let already_registered = client
        .contacts()
        .await
        .iter()
        .any(|c| c.public_key_prefix_hex == node.public_key_prefix_hex);
    if already_registered {
        return;
    }

    refresh_snapshot_contacts(client, state).await;
    state.broadcast_event(MeshEvent {
        at_unix: now_unix(),
        kind: MeshEventKind::RepeaterHeard {
            name: node.name,
            prefix_hex: node.public_key_prefix_hex,
        },
    });
}

/// Declares any managed repeater from the config that the node doesn't
/// already know as a contact, so it's recognized even before ever being
/// directly heard from. Runs on every (re)connect as a self-healing pass
/// (e.g. after the companion forgets a contact).
async fn reconcile_managed_repeaters(client: &MeshClient, state: &AppState) {
    let managed_repeaters = state.config.read().await.managed_repeaters.clone();
    if managed_repeaters.is_empty() {
        return;
    }

    let known = client.contacts().await;
    let mut declared_any = false;

    for repeater in &managed_repeaters {
        let already_known = known
            .iter()
            .any(|c| repeater.matches(&c.public_key_prefix_hex));
        if already_known {
            continue;
        }

        match client.declare_contact(repeater).await {
            Ok(()) => {
                info!(repeater = %repeater.name, "declared managed repeater to the node");
                declared_any = true;
                state.broadcast_event(MeshEvent {
                    at_unix: now_unix(),
                    kind: MeshEventKind::ManagedRepeaterDeclared {
                        name: repeater.name.clone(),
                    },
                });
            }
            Err(err) => {
                warn!(
                    repeater = %repeater.name,
                    error = %err,
                    "failed to declare managed repeater to the node"
                );
            }
        }
    }

    if declared_any {
        refresh_snapshot_contacts(client, state).await;
    }
}

/// Runs a command issued by an IPC client against the live MeshCore
/// connection and reports the outcome back through its reply channel.
async fn handle_command(cmd: DaemonCommand, client: &MeshClient, state: &AppState) {
    match cmd {
        DaemonCommand::RemoveContact {
            public_key_prefix_hex,
            reply,
        } => {
            let outcome = client.remove_contact(&public_key_prefix_hex).await;
            let result = match outcome {
                Ok(()) => finalize_contact_removal(client, state, &public_key_prefix_hex).await,
                Err(err) => Err(err.to_string()),
            };
            let _ = reply.send(result);
        }
        DaemonCommand::SetManagedRepeater {
            public_key_prefix_hex,
            name,
            managed,
            reply,
        } => {
            let result =
                set_managed_repeater(client, state, &public_key_prefix_hex, &name, managed).await;
            let _ = reply.send(result);
        }
        DaemonCommand::AddRepeater {
            public_key_hex,
            name,
            managed,
            reply,
        } => {
            let result = add_repeater(client, state, &public_key_hex, &name, managed).await;
            let _ = reply.send(result);
        }
    }
}

async fn finalize_contact_removal(
    client: &MeshClient,
    state: &AppState,
    public_key_prefix_hex: &str,
) -> Result<(), String> {
    let removed_name = {
        let snap = state.snapshot.read().await;
        snap.contacts
            .iter()
            .find(|c| {
                c.public_key_prefix_hex
                    .eq_ignore_ascii_case(public_key_prefix_hex)
            })
            .map(|c| c.name.clone())
            .unwrap_or_else(|| public_key_prefix_hex.to_string())
    };

    client
        .fetch_contacts()
        .await
        .map_err(|err| format!("contact removed, but failed to refresh contacts: {err}"))?;
    refresh_snapshot_contacts(client, state).await;

    state.broadcast_event(MeshEvent {
        at_unix: now_unix(),
        kind: MeshEventKind::ContactRemoved {
            name: removed_name,
            prefix_hex: public_key_prefix_hex.to_string(),
        },
    });

    Ok(())
}

/// Adds or removes a repeater from the managed list and persists the
/// config. When adding one that isn't yet a companion contact, its full
/// public key is resolved from the discovered-repeaters cache (populated
/// from RF log data) and it's registered on the spot: a managed repeater
/// must always be registered.
async fn set_managed_repeater(
    client: &MeshClient,
    state: &AppState,
    public_key_prefix_hex: &str,
    name: &str,
    managed: bool,
) -> Result<(), String> {
    // The full key, if we just resolved one while registering — persisted
    // instead of the bare prefix so the config alone is enough to
    // re-declare this repeater later (e.g. after a companion reset).
    let mut resolved_full_key_hex: Option<String> = None;

    if managed {
        let already_registered = client
            .contacts()
            .await
            .iter()
            .any(|c| c.public_key_prefix_hex == public_key_prefix_hex);

        if !already_registered {
            let discovered = state
                .discovered_repeaters
                .read()
                .await
                .get(public_key_prefix_hex)
                .cloned();

            let Some(node) = discovered else {
                return Err(format!(
                    "\"{name}\" hasn't been heard yet on the mesh, so its full public key isn't known — cannot register it"
                ));
            };

            let repeater = ManagedRepeater {
                name: name.to_string(),
                public_key_hex: node.public_key_hex.clone(),
            };
            client
                .declare_contact(&repeater)
                .await
                .map_err(|err| format!("failed to register \"{name}\": {err}"))?;

            resolved_full_key_hex = Some(node.public_key_hex);

            state.broadcast_event(MeshEvent {
                at_unix: now_unix(),
                kind: MeshEventKind::ManagedRepeaterDeclared {
                    name: name.to_string(),
                },
            });
        }
    }

    upsert_managed_repeater_config(
        state,
        public_key_prefix_hex,
        name,
        managed,
        resolved_full_key_hex,
    )
    .await?;

    refresh_snapshot_contacts(client, state).await;
    Ok(())
}

/// Declares a new contact directly from a caller-supplied full public key,
/// without requiring it to have been overheard on the mesh first (unlike
/// [`set_managed_repeater`], which can only resolve a full key for a node
/// already known). If `managed` is `true`, it's also added to the config's
/// managed-repeater list.
async fn add_repeater(
    client: &MeshClient,
    state: &AppState,
    public_key_hex: &str,
    name: &str,
    managed: bool,
) -> Result<(), String> {
    if public_key_hex.len() != 64 || !public_key_hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "\"{name}\" needs a full 32-byte public key (64 hex characters), got {} characters",
            public_key_hex.len()
        ));
    }
    let public_key_prefix_hex: String = public_key_hex.chars().take(12).collect();

    let already_registered = client.contacts().await.iter().any(|c| {
        c.public_key_prefix_hex
            .eq_ignore_ascii_case(&public_key_prefix_hex)
    });

    if !already_registered {
        let repeater = ManagedRepeater {
            name: name.to_string(),
            public_key_hex: public_key_hex.to_string(),
        };
        client
            .declare_contact(&repeater)
            .await
            .map_err(|err| format!("failed to register \"{name}\": {err}"))?;

        state.broadcast_event(MeshEvent {
            at_unix: now_unix(),
            kind: MeshEventKind::ManagedRepeaterDeclared {
                name: name.to_string(),
            },
        });
    }

    if managed {
        upsert_managed_repeater_config(
            state,
            &public_key_prefix_hex,
            name,
            true,
            Some(public_key_hex.to_string()),
        )
        .await?;
    }

    refresh_snapshot_contacts(client, state).await;
    Ok(())
}

/// Adds, updates or removes a repeater's entry in the config's
/// managed-repeater list and persists the config. `resolved_full_key_hex`,
/// when available, is persisted instead of the bare prefix so the config
/// alone is enough to re-declare this repeater later (e.g. after a
/// companion reset).
async fn upsert_managed_repeater_config(
    state: &AppState,
    public_key_prefix_hex: &str,
    name: &str,
    managed: bool,
    resolved_full_key_hex: Option<String>,
) -> Result<(), String> {
    let mut config = state.config.write().await;
    let existing_index = config
        .managed_repeaters
        .iter()
        .position(|r| r.matches(public_key_prefix_hex));

    match (managed, existing_index) {
        (true, Some(index)) => {
            config.managed_repeaters[index].name = name.to_string();
            if let Some(full_key) = resolved_full_key_hex {
                config.managed_repeaters[index].public_key_hex = full_key;
            }
        }
        (true, None) => config.managed_repeaters.push(ManagedRepeater {
            name: name.to_string(),
            public_key_hex: resolved_full_key_hex
                .unwrap_or_else(|| public_key_prefix_hex.to_string()),
        }),
        (false, Some(index)) => {
            config.managed_repeaters.remove(index);
        }
        (false, None) => {}
    }

    config
        .save_to(&state.config_path)
        .map_err(|err| format!("failed to save config: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fez_mesh_controller_core::{Config, DaemonConfig};
    use std::path::PathBuf;

    /// Builds an `AppState` backed by a real (tempdir) config path, so
    /// `save_to` actually persists and can be reloaded to verify.
    fn make_state(managed_repeaters: Vec<ManagedRepeater>) -> (AppState, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("config.toml");
        let (command_tx, _command_rx) = mpsc::channel(1);
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
                discovered_nodes_capacity: 200,
            },
            managed_repeaters,
            regions: vec![],
            hashtag_channels: vec![],
            mqtt_brokers: vec![],
        };
        let state = AppState::new(command_tx, config, config_path);
        (state, dir)
    }

    #[tokio::test]
    async fn upsert_adds_a_new_managed_repeater() {
        let (state, _dir) = make_state(vec![]);

        upsert_managed_repeater_config(
            &state,
            "aabbccddeeff",
            "Repeater A",
            true,
            Some("aa".repeat(32)),
        )
        .await
        .unwrap();

        let config = state.config.read().await;
        assert_eq!(config.managed_repeaters.len(), 1);
        assert_eq!(config.managed_repeaters[0].name, "Repeater A");
        assert_eq!(config.managed_repeaters[0].public_key_hex, "aa".repeat(32));
    }

    #[tokio::test]
    async fn upsert_updates_an_existing_managed_repeater() {
        let (state, _dir) = make_state(vec![ManagedRepeater {
            name: "Old Name".to_string(),
            public_key_hex: "aabbccddeeff".to_string(),
        }]);

        upsert_managed_repeater_config(&state, "aabbccddeeff", "New Name", true, None)
            .await
            .unwrap();

        let config = state.config.read().await;
        assert_eq!(config.managed_repeaters.len(), 1);
        assert_eq!(config.managed_repeaters[0].name, "New Name");
        // No resolved full key given: the existing key is left untouched.
        assert_eq!(config.managed_repeaters[0].public_key_hex, "aabbccddeeff");
    }

    #[tokio::test]
    async fn upsert_removes_an_existing_managed_repeater() {
        let (state, _dir) = make_state(vec![ManagedRepeater {
            name: "Repeater A".to_string(),
            public_key_hex: "aabbccddeeff".to_string(),
        }]);

        upsert_managed_repeater_config(&state, "aabbccddeeff", "Repeater A", false, None)
            .await
            .unwrap();

        assert!(state.config.read().await.managed_repeaters.is_empty());
    }

    #[tokio::test]
    async fn upsert_unmanaging_an_unknown_prefix_is_a_noop() {
        let (state, _dir) = make_state(vec![]);

        upsert_managed_repeater_config(&state, "aabbccddeeff", "Nobody", false, None)
            .await
            .unwrap();

        assert!(state.config.read().await.managed_repeaters.is_empty());
    }

    #[tokio::test]
    async fn upsert_persists_to_disk() {
        let (state, _dir) = make_state(vec![]);
        let config_path = state.config_path.clone();

        upsert_managed_repeater_config(
            &state,
            "aabbccddeeff",
            "Repeater A",
            true,
            Some("aa".repeat(32)),
        )
        .await
        .unwrap();

        let reloaded = Config::load_from(&config_path).expect("reload persisted config");
        assert_eq!(reloaded.managed_repeaters.len(), 1);
        assert_eq!(reloaded.managed_repeaters[0].name, "Repeater A");
    }
}
