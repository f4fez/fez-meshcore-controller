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

use fez_mesh_controller_core::mesh::NodeStatsDto;
use tokio::sync::oneshot;

/// A command issued by an IPC client that needs to run against the live
/// MeshCore connection. The IPC server task doesn't hold that connection
/// itself (only the mesh connection task does), so commands are forwarded
/// through this channel; `reply` carries the outcome back.
pub enum DaemonCommand {
    RemoveContact {
        public_key_prefix_hex: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SetManagedRepeater {
        public_key_prefix_hex: String,
        name: String,
        managed: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
    AddRepeater {
        public_key_hex: String,
        name: String,
        managed: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Not IPC-originated -- sent by `crate::mqtt`'s status-publish path,
    /// which doesn't hold the live connection itself either. Best-effort:
    /// no error case, a partial/failed fetch just yields a `NodeStatsDto`
    /// with fewer fields set rather than blocking the status publish.
    RefreshNodeStats {
        reply: oneshot::Sender<NodeStatsDto>,
    },
}
