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

use fez_mesh_controller_core::mesh::{NodeStatsDto, RepeaterDetailCategory, TelemetryDto};
use fez_mesh_controller_core::RepeaterStatus;
use tokio::sync::{mpsc, oneshot};

/// A command issued by an IPC client that needs to run against the live
/// MeshCore connection. The IPC server task doesn't hold that connection
/// itself (only the mesh connection task does), so commands are forwarded
/// through this channel; `reply` carries the outcome back.
pub enum DaemonCommand {
    RemoveContact {
        public_key_prefix_hex: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// `status: None` removes the repeater from `managed_repeaters`
    /// entirely; `Some(status)` creates or updates its entry with that
    /// tier — see `mesh_task::set_repeater_status`.
    SetRepeaterStatus {
        public_key_prefix_hex: String,
        name: String,
        status: Option<RepeaterStatus>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    AddRepeater {
        public_key_hex: String,
        name: String,
        managed: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Fetches fresh telemetry from a known contact (typically a managed
    /// repeater), logging in first if the matching `ManagedRepeater` config
    /// entry has a password set.
    RequestTelemetry {
        public_key_prefix_hex: String,
        reply: oneshot::Sender<Result<TelemetryDto, String>>,
    },
    /// Fetches status + telemetry + neighbours + region hierarchy from a
    /// known contact together, as one combined command (one login, then the
    /// four requests sequentially — see
    /// `mesh_task::request_repeater_detail`, which explains why not
    /// `tokio::join!`). Unlike other commands, the outcome isn't carried by
    /// a single `oneshot` reply: each category is pushed to `updates` as
    /// soon as it's fetched, so the requesting IPC client can render the
    /// popup progressively instead of waiting for everything. The channel
    /// closing (all senders dropped once the fetch function returns) is the
    /// only completion signal — there's no separate "done" message.
    RequestRepeaterDetail {
        public_key_prefix_hex: String,
        updates: mpsc::Sender<RepeaterDetailCategory>,
    },
    /// Not IPC-originated -- sent by `crate::mqtt`'s status-publish path,
    /// which doesn't hold the live connection itself either. Best-effort:
    /// no error case, a partial/failed fetch just yields a `NodeStatsDto`
    /// with fewer fields set rather than blocking the status publish.
    RefreshNodeStats {
        reply: oneshot::Sender<NodeStatsDto>,
    },
    /// Not IPC-originated -- sent by `crate::mqtt`'s device-signed auth
    /// flow, which needs the node to sign a JWT signing input but (like
    /// `RefreshNodeStats`) doesn't hold the live connection itself. Unlike
    /// `RefreshNodeStats` this has no best-effort fallback: without a
    /// signature the broker connection simply cannot authenticate, so
    /// failures are propagated rather than silently ignored.
    SignData {
        data: Vec<u8>,
        reply: oneshot::Sender<Result<Vec<u8>, String>>,
    },
    /// Not IPC-originated -- sent by `crate::reload` right after a config
    /// reload that leaves `observer_node_managed_config` set to `true`, so
    /// the lockdown (and any `managed_repeaters` change) applies immediately
    /// instead of waiting for the next reconnect. Fire-and-forget: outcome
    /// is already broadcast as `MeshEventKind::ObserverNodeConfigEnforced`
    /// if it changes anything.
    ResyncObserverConfig,
}
