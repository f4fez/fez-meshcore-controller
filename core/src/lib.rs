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

//! Library shared between the fez-mesh-controller daemon and CLI/TUI.
//!
//! It groups together:
//! - configuration loading/saving ([`config`])
//! - the socket-based protocol between the CLI and the daemon ([`ipc`])
//! - a high-level wrapper around `meshcore-rs` and the shared serializable
//!   data transfer objects (DTOs) ([`mesh`])

pub mod channel;
pub mod config;
pub mod error;
pub mod ipc;
pub mod mesh;
pub mod meshcore_crypto;
pub mod region;

pub use config::{Config, ConnectionConfig, DaemonConfig, ManagedRepeater, RegionConfig};
pub use error::{Error, Result};
