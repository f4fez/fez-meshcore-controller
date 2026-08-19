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
    build_packet_log_entry, contacts_to_prune, extract_discovered_node, is_registered_contact,
    is_repeater_or_room, map_event, matching_repeater_status, ContactDto, MeshClient,
    MeshEventKind, RepeaterDetailCategory, TelemetryDto,
};
use fez_mesh_controller_core::{ConnectionConfig, ManagedRepeater, RepeaterStatus};
use futures::StreamExt;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::command::DaemonCommand;
use crate::state::AppState;

const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// A repeater's out-of-the-box guest password, per firmware source
/// (`src/helpers/CommonCLI.h`'s `NodePrefs()` constructor zero-initializes
/// `guest_password`, i.e. an empty string, for every device type). "hello"
/// is *not* this: that's specifically `ROOM_PASSWORD`, a build flag only
/// `examples/simple_room_server/MyMesh.cpp` uses to override the same field
/// -- plain repeaters have no equivalent override, so they keep the empty
/// default until an admin runs `set guest.password <pwd>`. Worth trying so
/// a repeater without a `managed_repeaters` password configured can still
/// yield guest-accessible data (status/telemetry/neighbours; not the
/// admin-gated region hierarchy). Silently ignored if it doesn't work (e.g.
/// the admin did set a real guest password), see `request_repeater_detail`.
const DEFAULT_GUEST_PASSWORD: &str = "";

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
                state.set_node_stats(client.node_stats().await).await;
                refresh_snapshot_contacts(&client, &state).await;
                state.broadcast_event(MeshEvent {
                    at_unix: now_unix(),
                    kind: MeshEventKind::Connected,
                });

                reconcile_managed_repeaters(&client, &state).await;
                enforce_observer_node_config(&client, &state).await;

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
///
/// Always fetches fresh from the node rather than reading the cached
/// contact list: the node never pushes a notification for contact-list
/// changes *we* initiated (`declare_contact`/`remove_contact`, e.g. from
/// `reconcile_managed_repeaters`, `set_repeater_status`, `add_repeater`,
/// `enforce_observer_node_config`), so the cache would otherwise stay
/// stale until an unrelated node-pushed event happened to mark it dirty.
async fn build_snapshot_contacts(client: &MeshClient, state: &AppState) -> Vec<ContactDto> {
    let managed_repeaters = state.config.read().await.managed_repeaters.clone();
    let mut contacts = match client.fetch_contacts().await {
        Ok(contacts) => contacts,
        Err(err) => {
            warn!(
                error = %err,
                "failed to fetch fresh contacts from the node, using possibly-stale cache"
            );
            client.contacts().await
        }
    };
    let telemetry = state.telemetry.read().await;
    for contact in &mut contacts {
        let status = matching_repeater_status(&managed_repeaters, &contact.public_key_prefix_hex);
        contact.repeater_status = status;
        contact.managed = matches!(
            status,
            Some(RepeaterStatus::Managed | RepeaterStatus::Supervised)
        );
        contact.last_telemetry = telemetry.get(&contact.public_key_prefix_hex).cloned();
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
        let status = matching_repeater_status(&managed_repeaters, &node.public_key_prefix_hex);
        contacts.push(ContactDto {
            name: node.name.clone(),
            public_key_prefix_hex: node.public_key_prefix_hex.clone(),
            last_advert_unix: node.last_seen_unix.max(0) as u32,
            lat: node.lat,
            lon: node.lon,
            registered: false,
            managed: matches!(
                status,
                Some(RepeaterStatus::Managed | RepeaterStatus::Supervised)
            ),
            repeater_status: status,
            contact_type: node.adv_type,
            last_telemetry: telemetry.get(&node.public_key_prefix_hex).cloned(),
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
/// data) in the SQLite-backed discovered-nodes store and, if it isn't
/// already known, surfaces it in the contact list as "discovered" and logs
/// a one-off event the first time it's seen.
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
/// (e.g. after the companion forgets a contact). Deliberately status-
/// agnostic: every `managed_repeaters` entry is declared regardless of its
/// `RepeaterStatus` (`Known`/`Supervised` get registered exactly like
/// `Managed`, not just fully-managed repeaters).
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

/// When `observer_node_managed_config` is enabled, locks the connected node
/// to an observer-only state: disables its own contact auto-add, wipes
/// every channel slot (Public and private/hashtag alike), and prunes any
/// contact that isn't in `managed_repeaters` (via [`contacts_to_prune`],
/// itself status-agnostic -- see its doc comment -- so `Known`/`Supervised`
/// repeaters are protected exactly like `Managed` ones). Each correction is
/// independent -- a failure in one doesn't stop the others from being
/// attempted, mirroring [`reconcile_managed_repeaters`].
async fn enforce_observer_node_config(client: &MeshClient, state: &AppState) {
    if !state
        .config
        .read()
        .await
        .daemon
        .observer_node_managed_config
    {
        return;
    }

    match client.autoadd_enabled().await {
        Ok(true) => match client.disable_auto_add_contacts().await {
            Ok(()) => {
                info!("disabled contact auto-add on the observation node");
                state.broadcast_event(MeshEvent {
                    at_unix: now_unix(),
                    kind: MeshEventKind::ObserverNodeConfigEnforced {
                        detail: "disabled contact auto-add".to_string(),
                    },
                });
            }
            Err(err) => warn!(error = %err, "failed to disable contact auto-add on the node"),
        },
        Ok(false) => {} // already compliant
        Err(err) => warn!(error = %err, "failed to query the node's auto-add configuration"),
    }

    if let Some(max_channels) = client.max_channels().await {
        for idx in 0..max_channels {
            match client.get_channel(idx).await {
                Ok(channel) if !channel.name.is_empty() || channel.secret != [0u8; 16] => {
                    match client.remove_channel(idx).await {
                        Ok(()) => {
                            info!(
                                channel_idx = idx,
                                "removed channel from the observation node"
                            );
                            state.broadcast_event(MeshEvent {
                                at_unix: now_unix(),
                                kind: MeshEventKind::ObserverNodeConfigEnforced {
                                    detail: format!("removed channel {idx}"),
                                },
                            });
                        }
                        Err(err) => {
                            warn!(channel_idx = idx, error = %err, "failed to remove channel from the node")
                        }
                    }
                }
                Ok(_) => {} // already empty
                Err(err) => {
                    warn!(channel_idx = idx, error = %err, "failed to query channel from the node")
                }
            }
        }
    }

    let managed_repeaters = state.config.read().await.managed_repeaters.clone();
    let known = client.contacts().await;
    let mut pruned_any = false;

    for contact in contacts_to_prune(&known, &managed_repeaters) {
        match client.remove_contact(&contact.public_key_prefix_hex).await {
            Ok(()) => {
                pruned_any = true;
                info!(contact = %contact.name, "pruned non-managed contact from the observation node");
                state.broadcast_event(MeshEvent {
                    at_unix: now_unix(),
                    kind: MeshEventKind::ObserverNodeConfigEnforced {
                        detail: format!("pruned non-managed contact {}", contact.name),
                    },
                });
            }
            Err(err) => {
                warn!(contact = %contact.name, error = %err, "failed to prune non-managed contact from the node")
            }
        }
    }

    if pruned_any {
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
        DaemonCommand::SetRepeaterStatus {
            public_key_prefix_hex,
            name,
            status,
            reply,
        } => {
            let result =
                set_repeater_status(client, state, &public_key_prefix_hex, &name, status).await;
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
        DaemonCommand::RequestTelemetry {
            public_key_prefix_hex,
            reply,
        } => {
            let result = request_telemetry(client, state, &public_key_prefix_hex).await;
            let _ = reply.send(result);
        }
        DaemonCommand::RequestRepeaterDetail {
            public_key_prefix_hex,
            updates,
        } => {
            request_repeater_detail(client, state, &public_key_prefix_hex, updates).await;
        }
        DaemonCommand::RefreshNodeStats { reply } => {
            let stats = client.node_stats().await;
            state.set_node_stats(stats.clone()).await;
            let _ = reply.send(stats);
        }
        DaemonCommand::SignData { data, reply } => {
            let result = client.sign(&data).await.map_err(|e| e.to_string());
            let _ = reply.send(result);
        }
        DaemonCommand::ResyncObserverConfig => {
            enforce_observer_node_config(client, state).await;
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

/// Sets, changes or clears a repeater's status in the config's
/// managed-repeater list and persists the config. When setting any status
/// (`Managed`/`Known`/`Supervised`) on a repeater that isn't yet a
/// companion contact, its full public key is resolved from the
/// discovered-repeaters cache (populated from RF log data) and it's
/// registered on the spot: every tier requires the repeater to be a real
/// companion contact, not just `Managed` (`Known`/`Supervised` repeaters
/// get the same auto-declare/prune-protection treatment — see
/// [`reconcile_managed_repeaters`]/[`contacts_to_prune`]).
async fn set_repeater_status(
    client: &MeshClient,
    state: &AppState,
    public_key_prefix_hex: &str,
    name: &str,
    status: Option<RepeaterStatus>,
) -> Result<(), String> {
    // The full key, if we just resolved one while registering — persisted
    // instead of the bare prefix so the config alone is enough to
    // re-declare this repeater later (e.g. after a companion reset).
    let mut resolved_full_key_hex: Option<String> = None;

    if status.is_some() {
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
                password: None,
                status: status.unwrap_or_default(),
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
        status,
        resolved_full_key_hex,
    )
    .await?;

    // Unlike `ManagedRepeaterDeclared` above (only broadcast when a fresh
    // `declare_contact` happens), this fires unconditionally -- changing an
    // already-registered repeater's tier (or clearing it) wouldn't
    // otherwise broadcast anything, leaving connected clients' contact
    // lists stale until an unrelated event happened to refresh them.
    state.broadcast_event(MeshEvent {
        at_unix: now_unix(),
        kind: MeshEventKind::RepeaterStatusChanged {
            name: name.to_string(),
            status,
        },
    });

    refresh_snapshot_contacts(client, state).await;
    Ok(())
}

/// Declares a new contact directly from a caller-supplied full public key,
/// without requiring it to have been overheard on the mesh first (unlike
/// [`set_repeater_status`], which can only resolve a full key for a node
/// already known). If `managed` is `true`, it's also added to the config's
/// managed-repeater list with status `Managed` (this CLI-only entry point
/// doesn't expose the `Known`/`Supervised` tiers -- those are set via the
/// TUI's `k`/`s` shortcuts or the config file directly).
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
            password: None,
            status: RepeaterStatus::Managed,
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
            Some(RepeaterStatus::Managed),
            Some(public_key_hex.to_string()),
        )
        .await?;
    }

    refresh_snapshot_contacts(client, state).await;
    Ok(())
}

/// Looks up the password configured for a contact's matching
/// `ManagedRepeater` config entry, if any — shared by every command that
/// needs to log in before an authenticated request (telemetry, and the
/// combined status/telemetry/neighbours/regions fetch). `None` doesn't mean
/// "don't log in": [`request_repeater_detail`] falls back to
/// [`DEFAULT_GUEST_PASSWORD`] in that case.
async fn find_repeater_password(state: &AppState, public_key_prefix_hex: &str) -> Option<String> {
    state
        .config
        .read()
        .await
        .managed_repeaters
        .iter()
        .find(|r| r.matches(public_key_prefix_hex))
        .and_then(|r| r.password.clone())
}

/// Fetches and decodes telemetry from a contact (typically a managed
/// repeater). Logs in first if the matching `ManagedRepeater` config entry
/// has a password set (most repeaters require it, see `MeshClient::login`),
/// and logs back out afterward regardless of outcome (best-effort,
/// considerate towards the repeater's limited ACL table size).
async fn request_telemetry(
    client: &MeshClient,
    state: &AppState,
    public_key_prefix_hex: &str,
) -> Result<TelemetryDto, String> {
    let password = find_repeater_password(state, public_key_prefix_hex).await;

    let mut logged_in = false;
    if let Some(password) = &password {
        client
            .login(public_key_prefix_hex, password)
            .await
            .map_err(|err| err.to_string())?;
        logged_in = true;
    }

    let result = client.request_telemetry(public_key_prefix_hex).await;

    if logged_in {
        if let Err(err) = client.logout(public_key_prefix_hex).await {
            warn!(prefix = %public_key_prefix_hex, error = %err, "failed to log out after telemetry request");
        }
    }

    let readings = result.map_err(|err| err.to_string())?;
    let dto = TelemetryDto {
        fetched_at_unix: now_unix(),
        readings,
    };

    finalize_telemetry_success(client, state, public_key_prefix_hex, &dto).await;

    Ok(dto)
}

/// Shared by [`request_telemetry`] and [`request_repeater_detail`]: caches
/// the fresh reading, refreshes the snapshot so `ContactDto.last_telemetry`
/// reflects it immediately, and broadcasts
/// `MeshEventKind::TelemetryReceived` for the event log / other connected
/// clients.
async fn finalize_telemetry_success(
    client: &MeshClient,
    state: &AppState,
    public_key_prefix_hex: &str,
    dto: &TelemetryDto,
) {
    state
        .set_telemetry(public_key_prefix_hex, dto.clone())
        .await;
    refresh_snapshot_contacts(client, state).await;

    let name = state
        .snapshot
        .read()
        .await
        .contacts
        .iter()
        .find(|c| {
            c.public_key_prefix_hex
                .eq_ignore_ascii_case(public_key_prefix_hex)
        })
        .map(|c| c.name.clone())
        .unwrap_or_else(|| public_key_prefix_hex.to_string());

    let summary = dto
        .readings
        .iter()
        .map(|r| format!("{}: {}{}", r.label, r.value, r.unit))
        .collect::<Vec<_>>()
        .join(", ");

    state.broadcast_event(MeshEvent {
        at_unix: now_unix(),
        kind: MeshEventKind::TelemetryReceived { name, summary },
    });
}

/// Fetches a contact's status, telemetry, neighbours and configured region
/// hierarchy together, as one combined command: a single login, then the
/// four requests *sequentially* (not concurrently -- `meshcore-rs`'s
/// `commands()` mutex is held for a request's entire round trip, so
/// concurrent calls would just serialize behind it without actually
/// overlapping; the region-hierarchy fetch in particular has no tag-based
/// correlation at all, see `MeshClient::request_region_hierarchy`), then a
/// single logout. Each category is pushed to `updates` as soon as it's
/// fetched (not batched into one final value) so the requesting IPC client
/// can render its popup progressively -- see
/// `DaemonCommand::RequestRepeaterDetail`.
///
/// Login: if the matching `ManagedRepeater` config entry has a password
/// set, it's used and a failure there is a hard stop -- reported as a
/// login-failed error on all four categories, since none of them can
/// possibly succeed without it. Otherwise (no password configured for this
/// repeater), [`DEFAULT_GUEST_PASSWORD`] is tried best-effort: success or
/// failure, we proceed to the four requests regardless (a repeater with no
/// guest access at all will simply time out on each, same as before this
/// fallback existed).
///
/// Precondition: `public_key_prefix_hex` must be a contact the companion
/// already knows (`client.contacts()`), not merely a "discovered" node
/// (overheard on the mesh via RF log data, tracked in our own SQLite cache,
/// but never declared to the companion). A merely-discovered node can't be
/// logged into or sent a mesh-routed request at all -- every one of the
/// four calls would fail deep inside `MeshClient` with the same confusing
/// low-level "no contact matches prefix" error, so that case is reported
/// once, up front, as a single clear explanation instead.
async fn request_repeater_detail(
    client: &MeshClient,
    state: &AppState,
    public_key_prefix_hex: &str,
    updates: mpsc::Sender<RepeaterDetailCategory>,
) {
    let contacts = client.contacts().await;
    if !is_registered_contact(&contacts, public_key_prefix_hex) {
        let message = "not registered as a contact on this node (only overheard on the mesh \
            so far) -- press 'm' to register and manage it first"
            .to_string();
        let _ = updates
            .send(RepeaterDetailCategory::Status(Err(message.clone())))
            .await;
        let _ = updates
            .send(RepeaterDetailCategory::Telemetry(Err(message.clone())))
            .await;
        let _ = updates
            .send(RepeaterDetailCategory::Neighbours(Err(message.clone())))
            .await;
        let _ = updates
            .send(RepeaterDetailCategory::Regions(Err(message)))
            .await;
        return;
    }

    let configured_password = find_repeater_password(state, public_key_prefix_hex).await;

    let logged_in = match &configured_password {
        Some(password) => match client.login(public_key_prefix_hex, password).await {
            Ok(()) => true,
            Err(err) => {
                let message = format!("login failed: {err}");
                let _ = updates
                    .send(RepeaterDetailCategory::Status(Err(message.clone())))
                    .await;
                let _ = updates
                    .send(RepeaterDetailCategory::Telemetry(Err(message.clone())))
                    .await;
                let _ = updates
                    .send(RepeaterDetailCategory::Neighbours(Err(message.clone())))
                    .await;
                let _ = updates
                    .send(RepeaterDetailCategory::Regions(Err(message)))
                    .await;
                return;
            }
        },
        None => client
            .login(public_key_prefix_hex, DEFAULT_GUEST_PASSWORD)
            .await
            .is_ok(),
    };

    let status_result = client
        .request_status(public_key_prefix_hex)
        .await
        .map_err(|err| err.to_string());
    let _ = updates
        .send(RepeaterDetailCategory::Status(status_result))
        .await;

    match client.request_telemetry(public_key_prefix_hex).await {
        Ok(readings) => {
            let dto = TelemetryDto {
                fetched_at_unix: now_unix(),
                readings,
            };
            finalize_telemetry_success(client, state, public_key_prefix_hex, &dto).await;
            let _ = updates
                .send(RepeaterDetailCategory::Telemetry(Ok(dto)))
                .await;
        }
        Err(err) => {
            let _ = updates
                .send(RepeaterDetailCategory::Telemetry(Err(err.to_string())))
                .await;
        }
    }

    let neighbours_result = client
        .request_neighbours(public_key_prefix_hex)
        .await
        .map_err(|err| err.to_string());
    let _ = updates
        .send(RepeaterDetailCategory::Neighbours(neighbours_result))
        .await;

    // Regions requires an *admin* login (see `request_region_hierarchy`'s
    // doc comment) -- attempted regardless of what role the login above
    // ended up granting, since we can't tell client-side; a guest-only
    // login just times out here like a genuinely absent reply would.
    let regions_result = client
        .request_region_hierarchy(public_key_prefix_hex)
        .await
        .map_err(|err| err.to_string());
    let _ = updates
        .send(RepeaterDetailCategory::Regions(regions_result))
        .await;

    if logged_in {
        if let Err(err) = client.logout(public_key_prefix_hex).await {
            warn!(prefix = %public_key_prefix_hex, error = %err, "failed to log out after repeater detail request");
        }
    }
}

/// Adds, updates or removes a repeater's entry in the config's
/// managed-repeater list and persists the config. `status: None` removes
/// the entry entirely; `Some(status)` creates it (or updates an existing
/// one's name/key/status, e.g. switching an existing `Managed` repeater to
/// `Known`). `resolved_full_key_hex`, when available, is persisted instead
/// of the bare prefix so the config alone is enough to re-declare this
/// repeater later (e.g. after a companion reset).
async fn upsert_managed_repeater_config(
    state: &AppState,
    public_key_prefix_hex: &str,
    name: &str,
    status: Option<RepeaterStatus>,
    resolved_full_key_hex: Option<String>,
) -> Result<(), String> {
    let mut config = state.config.write().await;
    let existing_index = config
        .managed_repeaters
        .iter()
        .position(|r| r.matches(public_key_prefix_hex));

    match (status, existing_index) {
        (Some(new_status), Some(index)) => {
            config.managed_repeaters[index].name = name.to_string();
            config.managed_repeaters[index].status = new_status;
            if let Some(full_key) = resolved_full_key_hex {
                config.managed_repeaters[index].public_key_hex = full_key;
            }
        }
        (Some(new_status), None) => config.managed_repeaters.push(ManagedRepeater {
            name: name.to_string(),
            public_key_hex: resolved_full_key_hex
                .unwrap_or_else(|| public_key_prefix_hex.to_string()),
            password: None,
            status: new_status,
        }),
        (None, Some(index)) => {
            config.managed_repeaters.remove(index);
        }
        (None, None) => {}
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
    async fn make_state(managed_repeaters: Vec<ManagedRepeater>) -> (AppState, tempfile::TempDir) {
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
                db_path: PathBuf::from(":memory:"),
                observer_node_managed_config: true,
            },
            managed_repeaters,
            regions: vec![],
            hashtag_channels: vec![],
            mqtt_brokers: vec![],
        };
        let state = AppState::new(command_tx, config, config_path)
            .await
            .expect("AppState::new with an in-memory DB should never fail");
        (state, dir)
    }

    #[tokio::test]
    async fn upsert_adds_a_new_managed_repeater() {
        let (state, _dir) = make_state(vec![]).await;

        upsert_managed_repeater_config(
            &state,
            "aabbccddeeff",
            "Repeater A",
            Some(RepeaterStatus::Managed),
            Some("aa".repeat(32)),
        )
        .await
        .unwrap();

        let config = state.config.read().await;
        assert_eq!(config.managed_repeaters.len(), 1);
        assert_eq!(config.managed_repeaters[0].name, "Repeater A");
        assert_eq!(config.managed_repeaters[0].public_key_hex, "aa".repeat(32));
        assert_eq!(config.managed_repeaters[0].status, RepeaterStatus::Managed);
    }

    #[tokio::test]
    async fn upsert_adds_a_new_known_or_supervised_repeater() {
        for status in [RepeaterStatus::Known, RepeaterStatus::Supervised] {
            let (state, _dir) = make_state(vec![]).await;

            upsert_managed_repeater_config(
                &state,
                "aabbccddeeff",
                "Repeater A",
                Some(status),
                Some("aa".repeat(32)),
            )
            .await
            .unwrap();

            let config = state.config.read().await;
            assert_eq!(config.managed_repeaters[0].status, status);
        }
    }

    #[tokio::test]
    async fn upsert_updates_an_existing_managed_repeater() {
        let (state, _dir) = make_state(vec![ManagedRepeater {
            name: "Old Name".to_string(),
            public_key_hex: "aabbccddeeff".to_string(),
            password: None,
            status: RepeaterStatus::Managed,
        }])
        .await;

        upsert_managed_repeater_config(
            &state,
            "aabbccddeeff",
            "New Name",
            Some(RepeaterStatus::Managed),
            None,
        )
        .await
        .unwrap();

        let config = state.config.read().await;
        assert_eq!(config.managed_repeaters.len(), 1);
        assert_eq!(config.managed_repeaters[0].name, "New Name");
        // No resolved full key given: the existing key is left untouched.
        assert_eq!(config.managed_repeaters[0].public_key_hex, "aabbccddeeff");
    }

    #[tokio::test]
    async fn upsert_changes_an_existing_repeaters_status() {
        let (state, _dir) = make_state(vec![ManagedRepeater {
            name: "Repeater A".to_string(),
            public_key_hex: "aabbccddeeff".to_string(),
            password: None,
            status: RepeaterStatus::Managed,
        }])
        .await;

        upsert_managed_repeater_config(
            &state,
            "aabbccddeeff",
            "Repeater A",
            Some(RepeaterStatus::Known),
            None,
        )
        .await
        .unwrap();

        let config = state.config.read().await;
        assert_eq!(config.managed_repeaters.len(), 1);
        assert_eq!(config.managed_repeaters[0].status, RepeaterStatus::Known);
        // Name/key untouched by the status change.
        assert_eq!(config.managed_repeaters[0].name, "Repeater A");
    }

    #[tokio::test]
    async fn upsert_removes_an_existing_managed_repeater() {
        let (state, _dir) = make_state(vec![ManagedRepeater {
            name: "Repeater A".to_string(),
            public_key_hex: "aabbccddeeff".to_string(),
            password: None,
            status: RepeaterStatus::Managed,
        }])
        .await;

        upsert_managed_repeater_config(&state, "aabbccddeeff", "Repeater A", None, None)
            .await
            .unwrap();

        assert!(state.config.read().await.managed_repeaters.is_empty());
    }

    #[tokio::test]
    async fn upsert_unmanaging_an_unknown_prefix_is_a_noop() {
        let (state, _dir) = make_state(vec![]).await;

        upsert_managed_repeater_config(&state, "aabbccddeeff", "Nobody", None, None)
            .await
            .unwrap();

        assert!(state.config.read().await.managed_repeaters.is_empty());
    }

    #[tokio::test]
    async fn upsert_persists_to_disk() {
        let (state, _dir) = make_state(vec![]).await;
        let config_path = state.config_path.clone();

        upsert_managed_repeater_config(
            &state,
            "aabbccddeeff",
            "Repeater A",
            Some(RepeaterStatus::Managed),
            Some("aa".repeat(32)),
        )
        .await
        .unwrap();

        let reloaded = Config::load_from(&config_path).expect("reload persisted config");
        assert_eq!(reloaded.managed_repeaters.len(), 1);
        assert_eq!(reloaded.managed_repeaters[0].name, "Repeater A");
    }
}
