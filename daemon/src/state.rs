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

use std::time::Instant;

use fez_mesh_controller_core::ipc::{MeshEvent, Snapshot};
use tokio::sync::{broadcast, RwLock};

/// State shared between the mesh connection task and the IPC server.
pub struct AppState {
    pub snapshot: RwLock<Snapshot>,
    pub events_tx: broadcast::Sender<MeshEvent>,
    pub started_at: Instant,
}

impl AppState {
    pub fn new() -> Self {
        let (events_tx, _rx) = broadcast::channel(256);
        Self {
            snapshot: RwLock::new(Snapshot::default()),
            events_tx,
            started_at: Instant::now(),
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
