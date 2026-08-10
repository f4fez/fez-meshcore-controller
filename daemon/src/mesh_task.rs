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

use std::sync::Arc;
use std::time::Duration;

use fez_mesh_controller_core::ipc::MeshEvent;
use fez_mesh_controller_core::mesh::{map_event, MeshClient, MeshEventKind};
use fez_mesh_controller_core::ConnectionConfig;
use futures::StreamExt;
use tracing::{info, warn};

use crate::state::AppState;

const RECONNECT_DELAY: Duration = Duration::from_secs(5);

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Connection loop for the MeshCore node: (re)connects indefinitely, updates
/// the shared snapshot and broadcasts received events.
pub async fn run(connection: ConnectionConfig, refresh_interval: Duration, state: Arc<AppState>) {
    loop {
        info!(target = %connection, "connecting to MeshCore node...");
        match MeshClient::connect(&connection).await {
            Ok(client) => {
                info!("connected to MeshCore node");
                {
                    let mut snap = state.snapshot.write().await;
                    snap.mesh_connected = true;
                    snap.self_info = client.self_info().await;
                    snap.contacts = client.contacts().await;
                    snap.generated_at_unix = now_unix();
                }
                state.broadcast_event(MeshEvent {
                    at_unix: now_unix(),
                    kind: MeshEventKind::Connected,
                });

                let mut events = client.event_stream();
                let mut ticker = tokio::time::interval(refresh_interval);
                ticker.tick().await; // the first tick fires immediately

                let already_notified_disconnect;
                loop {
                    tokio::select! {
                        maybe_event = events.next() => {
                            match maybe_event {
                                Some(raw) => {
                                    let Some(kind) = map_event(&raw) else { continue };
                                    let is_disconnect = matches!(kind, MeshEventKind::Disconnected);
                                    let refresh_contacts = matches!(
                                        kind,
                                        MeshEventKind::NewContact { .. } | MeshEventKind::Advertisement { .. }
                                    );

                                    if refresh_contacts {
                                        let mut snap = state.snapshot.write().await;
                                        snap.contacts = client.contacts().await;
                                        snap.generated_at_unix = now_unix();
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
