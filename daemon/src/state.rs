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

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use fez_mesh_controller_core::ipc::{MeshEvent, Snapshot};
use fez_mesh_controller_core::mesh::DiscoveredNode;
use fez_mesh_controller_core::Config;
use tokio::sync::{broadcast, mpsc, RwLock};

use crate::command::DaemonCommand;

/// State shared between the mesh connection task and the IPC server.
pub struct AppState {
    pub snapshot: RwLock<Snapshot>,
    pub events_tx: broadcast::Sender<MeshEvent>,
    pub started_at: Instant,
    /// Forwards commands from IPC clients to the mesh connection task,
    /// which is the only one holding the live MeshCore connection.
    pub command_tx: mpsc::Sender<DaemonCommand>,
    /// The daemon's own copy of the config, mutated and persisted when a
    /// client adds/removes a managed repeater at runtime.
    pub config: RwLock<Config>,
    pub config_path: PathBuf,
    /// Repeaters overheard but not (yet) a companion contact, keyed by
    /// 12-hex-char public key prefix. Populated from decoded RF log data,
    /// which (unlike the plain `Advertisement` push) carries the full
    /// public key needed to register them.
    pub discovered_repeaters: RwLock<HashMap<String, DiscoveredNode>>,
}

impl AppState {
    pub fn new(
        command_tx: mpsc::Sender<DaemonCommand>,
        config: Config,
        config_path: PathBuf,
    ) -> Self {
        let (events_tx, _rx) = broadcast::channel(256);
        Self {
            snapshot: RwLock::new(Snapshot::default()),
            events_tx,
            started_at: Instant::now(),
            command_tx,
            config: RwLock::new(config),
            config_path,
            discovered_repeaters: RwLock::new(HashMap::new()),
        }
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Broadcasts an event to connected IPC clients (silently dropped if
    /// nobody is listening).
    pub fn broadcast_event(&self, event: MeshEvent) {
        let _ = self.events_tx.send(event);
    }
}
