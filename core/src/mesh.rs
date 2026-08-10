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

//! High-level wrapper around `meshcore-rs` and serializable data transfer
//! objects (DTOs) shared between the daemon and the CLI via the IPC protocol.

use meshcore_rs::events::{Contact, SelfInfo};
use meshcore_rs::{EventPayload, EventType, MeshCore, MeshCoreEvent};
use serde::{Deserialize, Serialize};

use crate::config::ConnectionConfig;
use crate::error::Result;

/// Encodes a byte slice as a lowercase hexadecimal string.
pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Information about the local node (the controller itself), ready to display.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SelfInfoDto {
    pub name: String,
    pub public_key_hex: String,
    pub radio_freq_mhz: f64,
    pub spreading_factor: u8,
    pub coding_rate: u8,
    pub tx_power_dbm: u8,
    pub lat: f64,
    pub lon: f64,
}

impl From<&SelfInfo> for SelfInfoDto {
    fn from(info: &SelfInfo) -> Self {
        Self {
            name: info.name.clone(),
            public_key_hex: hex_encode(&info.public_key),
            radio_freq_mhz: info.radio_freq as f64 / 1000.0,
            spreading_factor: info.sf,
            coding_rate: info.cr,
            tx_power_dbm: info.tx_power,
            lat: info.adv_lat as f64 / 1_000_000.0,
            lon: info.adv_lon as f64 / 1_000_000.0,
        }
    }
}

/// A contact (remote node) known to the mesh network, ready to display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactDto {
    pub name: String,
    pub public_key_prefix_hex: String,
    pub last_advert_unix: u32,
    pub lat: f64,
    pub lon: f64,
}

impl From<&Contact> for ContactDto {
    fn from(c: &Contact) -> Self {
        Self {
            name: c.adv_name.clone(),
            public_key_prefix_hex: hex_encode(&c.prefix()),
            last_advert_unix: c.last_advert,
            lat: c.adv_lat as f64 / 1_000_000.0,
            lon: c.adv_lon as f64 / 1_000_000.0,
        }
    }
}

/// Simplified, serializable version of a `meshcore-rs` event, as broadcast
/// by the daemon to IPC clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MeshEventKind {
    Connected,
    Disconnected,
    Advertisement {
        name: String,
        prefix_hex: String,
        lat: f64,
        lon: f64,
    },
    NewContact {
        name: String,
    },
    ContactMessage {
        from_prefix_hex: String,
        text: String,
    },
    ChannelMessage {
        channel: u8,
        text: String,
    },
    MessageSent,
    /// Fallback for less critical event types (low-level protocol,
    /// statistics...) that we still want to trace.
    Other {
        label: String,
    },
}

/// Converts a raw `meshcore-rs` event into a serializable event, or `None`
/// if the event carries no actionable information.
pub fn map_event(event: &MeshCoreEvent) -> Option<MeshEventKind> {
    Some(match (&event.event_type, &event.payload) {
        (EventType::Connected, _) => MeshEventKind::Connected,
        (EventType::Disconnected, _) => MeshEventKind::Disconnected,
        (EventType::Advertisement, EventPayload::Advertisement(a)) => {
            MeshEventKind::Advertisement {
                name: a.name.clone(),
                prefix_hex: hex_encode(&a.prefix),
                lat: a.lat as f64 / 1_000_000.0,
                lon: a.lon as f64 / 1_000_000.0,
            }
        }
        (EventType::NewContact, EventPayload::Contact(c)) => MeshEventKind::NewContact {
            name: c.adv_name.clone(),
        },
        (EventType::ContactMsgRecv, EventPayload::ContactMessage(m)) => {
            MeshEventKind::ContactMessage {
                from_prefix_hex: hex_encode(&m.sender_prefix),
                text: m.text.clone(),
            }
        }
        (EventType::ChannelMsgRecv, EventPayload::ChannelMessage(m)) => {
            MeshEventKind::ChannelMessage {
                channel: m.channel_idx,
                text: m.text.clone(),
            }
        }
        (EventType::MsgSent, _) => MeshEventKind::MessageSent,
        (EventType::Ok, _) | (EventType::NextContact, _) => return None,
        (other, _) => MeshEventKind::Other {
            label: format!("{other:?}"),
        },
    })
}

/// Connected MeshCore client, providing simplified, serializable access to
/// the local node's state and its contacts.
pub struct MeshClient {
    inner: MeshCore,
}

impl MeshClient {
    /// Establishes the connection (serial, TCP or BLE depending on the
    /// configuration) and primes the internal caches (local node info,
    /// contacts).
    pub async fn connect(cfg: &ConnectionConfig) -> Result<Self> {
        let inner = match cfg {
            ConnectionConfig::Serial { port, baud_rate } => {
                MeshCore::serial(port, *baud_rate).await?
            }
            ConnectionConfig::Tcp { host, port } => MeshCore::tcp(host, *port).await?,
            ConnectionConfig::Ble { name } => MeshCore::ble_connect(name).await?,
        };

        inner.commands().lock().await.send_appstart().await?;
        inner.ensure_contacts().await?;

        Ok(Self { inner })
    }

    pub async fn is_connected(&self) -> bool {
        self.inner.is_connected().await
    }

    pub async fn self_info(&self) -> Option<SelfInfoDto> {
        self.inner.self_info().await.as_ref().map(SelfInfoDto::from)
    }

    pub async fn contacts(&self) -> Vec<ContactDto> {
        self.inner
            .contacts()
            .await
            .values()
            .map(ContactDto::from)
            .collect()
    }

    /// Forces a refresh of the contact cache from the node.
    pub async fn refresh_contacts(&self) -> Result<()> {
        self.inner.ensure_contacts().await?;
        Ok(())
    }

    /// Stream of raw events emitted by the MeshCore node.
    pub fn event_stream(&self) -> impl futures::Stream<Item = MeshCoreEvent> + Unpin + '_ {
        self.inner.event_stream()
    }

    pub async fn disconnect(&self) -> Result<()> {
        self.inner.disconnect().await?;
        Ok(())
    }
}
