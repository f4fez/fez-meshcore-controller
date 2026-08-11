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
use meshcore_rs::parsing::hex_decode;
use meshcore_rs::{EventPayload, EventType, MeshCore, MeshCoreEvent, PayloadType};
use serde::{Deserialize, Serialize};

use crate::config::{ConnectionConfig, ManagedRepeater};
use crate::error::{Error, Result};

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
    /// Whether this is an actual contact in the companion's own contact
    /// list, or merely a node whose advertisement has been overheard (see
    /// [`DiscoveredNode`]) but never added.
    pub registered: bool,
    /// Whether this node is in the config's `managed_repeaters` list.
    pub managed: bool,
}

impl From<&Contact> for ContactDto {
    fn from(c: &Contact) -> Self {
        Self {
            name: c.adv_name.clone(),
            public_key_prefix_hex: hex_encode(&c.prefix()),
            last_advert_unix: c.last_advert,
            lat: c.adv_lat as f64 / 1_000_000.0,
            lon: c.adv_lon as f64 / 1_000_000.0,
            registered: true,
            managed: false,
        }
    }
}

/// A node's full identity, resolved from an overheard advertisement via RF
/// log data (see [`extract_discovered_node`]) rather than from the
/// companion's own (prefix-only) [`EventType::Advertisement`] push. This is
/// the only way to learn a node's *full* public key before it becomes a
/// companion contact, and is what makes it possible to register (declare)
/// a repeater that's merely been heard.
#[derive(Debug, Clone)]
pub struct DiscoveredNode {
    pub name: String,
    /// Full 64-hex-char public key.
    pub public_key_hex: String,
    /// First 12 hex chars of `public_key_hex`, matches [`ContactDto::public_key_prefix_hex`].
    pub public_key_prefix_hex: String,
    pub is_repeater: bool,
    pub lat: f64,
    pub lon: f64,
    pub last_seen_unix: i64,
}

/// Extracts a node's full identity from a raw `meshcore-rs` event, if it's
/// RF log data ([`EventType::LogData`], pushed for *every* packet the radio
/// receives) decoding to an advertisement. Returns `None` for anything
/// else, including a plain [`EventType::Advertisement`] push (which only
/// carries a 6-byte prefix and can't be used to declare a contact).
pub fn extract_discovered_node(event: &MeshCoreEvent, now_unix: i64) -> Option<DiscoveredNode> {
    let EventPayload::LogData(log) = &event.payload else {
        return None;
    };
    let header = log.header.as_ref()?;
    if header.payload_type != PayloadType::Advert {
        return None;
    }
    let adv = log.advertisement.as_ref()?;
    let public_key_hex = hex_encode(&adv.public_key);

    Some(DiscoveredNode {
        name: adv.name.clone().unwrap_or_default(),
        public_key_prefix_hex: public_key_hex.chars().take(12).collect(),
        public_key_hex,
        is_repeater: adv.adv_type == 2, // see declare_contact's CONTACT_TYPENAMES note
        lat: adv.lat.map(|v| v as f64 / 1_000_000.0).unwrap_or(0.0),
        lon: adv.lon.map(|v| v as f64 / 1_000_000.0).unwrap_or(0.0),
        last_seen_unix: now_unix,
    })
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
        /// Number of hops the message travelled through the mesh.
        hops: u8,
        text: String,
    },
    ChannelMessage {
        channel: u8,
        /// Number of hops the message travelled through the mesh.
        hops: u8,
        text: String,
    },
    MessageSent,
    /// The node learned or refreshed the route to a remote node.
    PathUpdate {
        prefix_hex: String,
        hops: i8,
        path_hex: String,
    },
    /// Acknowledgement received for a previously sent message.
    Ack {
        tag_hex: String,
    },
    /// RF-level log emitted by the node's firmware for a received packet
    /// (signal-to-noise ratio and received signal strength).
    RfLog {
        snr: f32,
        rssi: i16,
    },
    /// A managed repeater (from the config file) was declared to the node
    /// because it wasn't already a known contact.
    ManagedRepeaterDeclared {
        name: String,
    },
    /// A repeater's advertisement was overheard for the first time (its
    /// full identity resolved via RF log data); it now shows up in the
    /// contact list as "discovered" until registered.
    RepeaterHeard {
        name: String,
        prefix_hex: String,
    },
    /// A contact was removed from the node's own contact list.
    ContactRemoved {
        name: String,
        prefix_hex: String,
    },
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
                hops: m.path_len,
                text: m.text.clone(),
            }
        }
        (EventType::ChannelMsgRecv, EventPayload::ChannelMessage(m)) => {
            MeshEventKind::ChannelMessage {
                channel: m.channel_idx,
                hops: m.path_len,
                text: m.text.clone(),
            }
        }
        (EventType::MsgSent, _) => MeshEventKind::MessageSent,
        (EventType::PathUpdate, EventPayload::PathUpdate(p)) => MeshEventKind::PathUpdate {
            prefix_hex: hex_encode(&p.prefix),
            hops: p.path_len,
            path_hex: hex_encode(&p.path),
        },
        (EventType::Ack, EventPayload::Ack { tag }) => MeshEventKind::Ack {
            tag_hex: hex_encode(tag),
        },
        (EventType::LogData, EventPayload::LogData(l)) => MeshEventKind::RfLog {
            snr: l.snr,
            rssi: l.rssi,
        },
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

    /// Fetches the contact list directly from the node, bypassing the
    /// cache. Needed after [`Self::remove_contact`] or
    /// [`Self::declare_contact`]: the node has no push notification for
    /// contact list changes it wasn't the one to initiate, so the cache
    /// would otherwise stay stale.
    pub async fn fetch_contacts(&self) -> Result<Vec<ContactDto>> {
        let contacts = self.inner.commands().lock().await.get_contacts(0).await?;
        Ok(contacts.iter().map(ContactDto::from).collect())
    }

    /// Removes a contact from the node's own contact list, identified by
    /// its public key prefix (hex).
    pub async fn remove_contact(&self, public_key_prefix_hex: &str) -> Result<()> {
        self.inner
            .commands()
            .lock()
            .await
            .remove_contact(public_key_prefix_hex)
            .await?;
        Ok(())
    }

    /// Declares a managed repeater to the node, so it's recognized even
    /// before ever being directly heard from. Requires the repeater's full
    /// 32-byte public key in the config (a mere prefix isn't enough to
    /// construct a valid contact).
    pub async fn declare_contact(&self, repeater: &ManagedRepeater) -> Result<()> {
        let key_bytes = hex_decode(&repeater.public_key_hex)?;
        if key_bytes.len() != 32 {
            return Err(Error::Other(format!(
                "managed repeater \"{}\" needs its full 32-byte public key to be declared (got {} bytes)",
                repeater.name,
                key_bytes.len()
            )));
        }
        let mut public_key = [0u8; 32];
        public_key.copy_from_slice(&key_bytes);

        let contact = Contact {
            public_key,
            // Repeater, see meshcore firmware's CONTACT_TYPENAMES
            // (NONE, CLI, REP, ROOM, SENS).
            contact_type: 2,
            flags: 0,
            path_len: -1, // unknown route: flood
            out_path: Vec::new(),
            adv_name: repeater.name.clone(),
            last_advert: 0,
            adv_lat: 0,
            adv_lon: 0,
            last_modification_timestamp: 0,
        };

        self.inner
            .commands()
            .lock()
            .await
            .add_contact(&contact)
            .await?;
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
