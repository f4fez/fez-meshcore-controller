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

/// A single raw RF packet, decoded from RF log data
/// ([`EventType::LogData`], pushed for *every* packet the radio receives),
/// for the packet log page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacketLogEntry {
    /// Monotonically increasing ID assigned by the daemon, stable even as
    /// older entries are evicted from its rotating cache — used to detect
    /// how many new packets arrived while the view is scroll-locked.
    pub id: u64,
    pub at_unix: i64,
    pub snr: f32,
    pub rssi: i16,
    /// `None` if the payload was too short to contain a decodable header.
    pub header: Option<PacketHeaderInfo>,
    /// Inner payload (after the header and path), as hex. Opaque/encrypted
    /// for message and channel payload types; empty for e.g. `Ack`.
    pub payload_hex: String,
    pub payload_len: usize,
}

/// Decoded over-the-air packet header, human-readable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacketHeaderInfo {
    pub route_type: String,
    pub payload_type: String,
    /// Raw numeric payload type byte (see `meshcore_rs::packets::PayloadType`),
    /// preserved alongside the human-readable `payload_type` string —
    /// needed to recompute a packet's transport code for region matching
    /// (see `crate::meshcore_crypto::calc_transport_code`).
    pub payload_type_raw: u8,
    pub payload_version: u8,
    pub hops: u8,
    pub path_hash_size: u8,
    pub path_hex: String,
    pub transport_code_hex: Option<String>,
    /// Destination node's address hash, present when `payload_type` is one
    /// that addresses a specific node (`Req`, `Response`, `TextMsg`,
    /// `Path`, `AnonReq`) and the payload is long enough to contain it.
    /// Always exactly 1 byte, per the firmware's `PAYLOAD_VER_1` wire
    /// format — independent of the header's `path_hash_size`, which only
    /// sizes the path's own hop hashes. See `extract_dest_src_hashes`.
    pub dest_hash_hex: Option<String>,
    /// Sender node's address hash, immediately following `dest_hash_hex` in
    /// the payload — same 1-byte sizing, but absent for `AnonReq` (the
    /// sender's full public key follows instead of a compact hash).
    pub src_hash_hex: Option<String>,
    /// Channel hash for `GroupText`/`GroupData` payloads — identifies which
    /// channel the message was sent to, *not* a node. Deliberately a
    /// separate field from `dest_hash_hex`: unlike a node's address hash,
    /// it must never be matched against a managed repeater's public key
    /// (see `cli/src/tui/packet_group.rs`).
    pub channel_hash_hex: Option<String>,
    /// Populated when `payload_type` is `"Advert"` and the inner payload
    /// could be decoded.
    pub advertisement: Option<PacketAdvertInfo>,
}

/// Advertiser identity decoded from an ADVERT payload, only present on
/// packets whose [`PacketHeaderInfo::payload_type`] is `"Advert"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacketAdvertInfo {
    pub public_key_hex: String,
    pub name: Option<String>,
    /// Human-readable advertiser type: "Chat", "Repeater", "Room", "Sensor"
    /// or "Unknown(N)".
    pub adv_type_name: String,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

fn adv_type_name(adv_type: u8) -> String {
    match adv_type {
        1 => "Chat".to_string(),
        2 => "Repeater".to_string(),
        3 => "Room".to_string(),
        4 => "Sensor".to_string(),
        other => format!("Unknown({other})"),
    }
}

/// Size in bytes of the destination/source/channel address hash prefixing
/// certain payload types. Fixed by the firmware's `PAYLOAD_VER_1` wire
/// format (`Mesh.cpp`: `uint8_t dest_hash = pkt->payload[i++];` etc., read
/// as single bytes, never scaled by `getPathHashSize()`) — *not* the same
/// as the header's `path_hash_size`, which only sizes the path's own hop
/// hashes and can be 1-4 bytes. A future `PAYLOAD_VER_2` may widen this,
/// which is why extraction is gated on `payload_version == 0` below.
const ADDRESS_HASH_SIZE: usize = 1;

/// Extracts the destination/source address hashes from the front of a
/// packet's inner payload, for payload types that carry them: `Req`,
/// `Response`, `TextMsg` and `Path` are each prefixed with a 1-byte
/// destination hash followed by a 1-byte source hash. `AnonReq` carries
/// only a destination hash — the sender's full public key follows instead
/// of a compact source hash. Every other payload type either broadcasts
/// (`Advert`), addresses a channel rather than a node (`GroupText`,
/// `GroupData` — see `extract_channel_hash`), or carries no addressing of
/// its own (`Ack`) — those return `(None, None)`, as does a payload too
/// short to hold the hash(es) or a non-`PAYLOAD_VER_1` payload version.
fn extract_dest_src_hashes(
    payload_type: PayloadType,
    payload_version: u8,
    payload: &[u8],
) -> (Option<String>, Option<String>) {
    if payload_version != 0 {
        return (None, None);
    }

    match payload_type {
        PayloadType::Req | PayloadType::Response | PayloadType::TextMsg | PayloadType::Path => {
            if payload.len() < ADDRESS_HASH_SIZE * 2 {
                return (None, None);
            }
            let dest_hash_hex = hex_encode(&payload[..ADDRESS_HASH_SIZE]);
            let src_hash_hex = hex_encode(&payload[ADDRESS_HASH_SIZE..ADDRESS_HASH_SIZE * 2]);
            (Some(dest_hash_hex), Some(src_hash_hex))
        }
        PayloadType::AnonReq => {
            if payload.len() < ADDRESS_HASH_SIZE {
                return (None, None);
            }
            (Some(hex_encode(&payload[..ADDRESS_HASH_SIZE])), None)
        }
        _ => (None, None),
    }
}

/// Extracts the channel hash from the front of a `GroupText`/`GroupData`
/// packet's inner payload — see [`PacketHeaderInfo::channel_hash_hex`].
/// `None` for any other payload type, a too-short payload, or a
/// non-`PAYLOAD_VER_1` payload version (same sizing caveat as
/// [`extract_dest_src_hashes`]).
fn extract_channel_hash(
    payload_type: PayloadType,
    payload_version: u8,
    payload: &[u8],
) -> Option<String> {
    if payload_version != 0 {
        return None;
    }
    if !matches!(
        payload_type,
        PayloadType::GroupText | PayloadType::GroupData
    ) {
        return None;
    }
    if payload.len() < ADDRESS_HASH_SIZE {
        return None;
    }
    Some(hex_encode(&payload[..ADDRESS_HASH_SIZE]))
}

/// Builds a packet log entry from a raw `meshcore-rs` event, if it's RF log
/// data. Returns `None` for anything else. Unlike [`extract_discovered_node`],
/// this captures *every* decodable packet, not just advertisements.
pub fn build_packet_log_entry(
    event: &MeshCoreEvent,
    id: u64,
    now_unix: i64,
) -> Option<PacketLogEntry> {
    let EventPayload::LogData(log) = &event.payload else {
        return None;
    };

    let header = log.header.as_ref().map(|h| {
        let (dest_hash_hex, src_hash_hex) =
            extract_dest_src_hashes(h.payload_type, h.payload_version, &log.payload);
        let channel_hash_hex =
            extract_channel_hash(h.payload_type, h.payload_version, &log.payload);
        PacketHeaderInfo {
            route_type: format!("{:?}", h.route_type),
            payload_type: format!("{:?}", h.payload_type),
            payload_type_raw: h.payload_type as u8,
            payload_version: h.payload_version,
            hops: h.path_len,
            path_hash_size: h.path_hash_size,
            path_hex: hex_encode(&h.path),
            transport_code_hex: h.transport_code.map(|c| hex_encode(&c)),
            dest_hash_hex,
            src_hash_hex,
            channel_hash_hex,
            advertisement: log.advertisement.as_ref().map(|a| PacketAdvertInfo {
                public_key_hex: hex_encode(&a.public_key),
                name: a.name.clone(),
                adv_type_name: adv_type_name(a.adv_type),
                lat: a.lat.map(|v| v as f64 / 1_000_000.0),
                lon: a.lon.map(|v| v as f64 / 1_000_000.0),
            }),
        }
    });

    Some(PacketLogEntry {
        id,
        at_unix: now_unix,
        snr: log.snr,
        rssi: log.rssi,
        header,
        payload_hex: hex_encode(&log.payload),
        payload_len: log.payload.len(),
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
        // Every received packet is already captured, with far more detail,
        // by the daemon's packet log (see `build_packet_log_entry`) — no
        // need to also duplicate it as a dashboard event.
        (EventType::Ok, _) | (EventType::NextContact, _) | (EventType::LogData, _) => return None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use meshcore_rs::events::{
        AdvertisementData, ChannelMessage, ContactMessage, LogData, MeshPacketHeader,
        PathUpdateData, RawAdvertisement, SelfInfo,
    };
    use meshcore_rs::packets::RouteType;
    use std::collections::HashMap;

    fn event(event_type: EventType, payload: EventPayload) -> MeshCoreEvent {
        MeshCoreEvent {
            event_type,
            payload,
            attributes: HashMap::new(),
        }
    }

    fn advert_header(payload_type: PayloadType) -> MeshPacketHeader {
        MeshPacketHeader {
            route_type: RouteType::Flood,
            payload_type,
            payload_version: 1,
            transport_code: None,
            path_len: 2,
            path_hash_size: 1,
            path: vec![0x11, 0x22],
        }
    }

    fn sample_advertisement(adv_type: u8) -> RawAdvertisement {
        RawAdvertisement {
            public_key: [0xab; 32],
            timestamp: 1_700_000_000,
            signature: [0u8; 64],
            adv_type,
            lat: Some(48_850_000),
            lon: Some(2_350_000),
            name: Some("Node A".to_string()),
        }
    }

    // --- hex_encode ---------------------------------------------------

    #[test]
    fn hex_encode_formats_lowercase_padded() {
        assert_eq!(hex_encode(&[0x00, 0xab, 0xff]), "00abff");
        assert_eq!(hex_encode(&[]), "");
    }

    // --- extract_discovered_node ---------------------------------------

    #[test]
    fn extract_discovered_node_ignores_non_log_data_events() {
        assert!(
            extract_discovered_node(&event(EventType::Connected, EventPayload::None), 0).is_none()
        );
    }

    #[test]
    fn extract_discovered_node_ignores_log_data_without_header() {
        let log = LogData {
            snr: 4.0,
            rssi: -90,
            header: None,
            advertisement: None,
            payload: vec![],
        };
        assert!(
            extract_discovered_node(&event(EventType::LogData, EventPayload::LogData(log)), 0)
                .is_none()
        );
    }

    #[test]
    fn extract_discovered_node_ignores_non_advert_payload_type() {
        let log = LogData {
            snr: 4.0,
            rssi: -90,
            header: Some(advert_header(PayloadType::TextMsg)),
            advertisement: None,
            payload: vec![],
        };
        assert!(
            extract_discovered_node(&event(EventType::LogData, EventPayload::LogData(log)), 0)
                .is_none()
        );
    }

    #[test]
    fn extract_discovered_node_ignores_advert_header_without_decoded_advertisement() {
        let log = LogData {
            snr: 4.0,
            rssi: -90,
            header: Some(advert_header(PayloadType::Advert)),
            advertisement: None,
            payload: vec![],
        };
        assert!(
            extract_discovered_node(&event(EventType::LogData, EventPayload::LogData(log)), 0)
                .is_none()
        );
    }

    #[test]
    fn extract_discovered_node_resolves_full_identity_for_a_repeater() {
        let log = LogData {
            snr: 4.0,
            rssi: -90,
            header: Some(advert_header(PayloadType::Advert)),
            advertisement: Some(sample_advertisement(2)),
            payload: vec![],
        };
        let node = extract_discovered_node(
            &event(EventType::LogData, EventPayload::LogData(log)),
            1_700_000_042,
        )
        .expect("should resolve a discovered node");

        assert_eq!(node.name, "Node A");
        assert_eq!(node.public_key_hex, "ab".repeat(32));
        assert_eq!(node.public_key_prefix_hex, "abababababab");
        assert!(node.is_repeater);
        assert_eq!(node.lat, 48.85);
        assert_eq!(node.lon, 2.35);
        assert_eq!(node.last_seen_unix, 1_700_000_042);
    }

    #[test]
    fn extract_discovered_node_non_repeater_advert_type_is_not_a_repeater() {
        let log = LogData {
            snr: 4.0,
            rssi: -90,
            header: Some(advert_header(PayloadType::Advert)),
            advertisement: Some(sample_advertisement(1)), // Chat, not Repeater
            payload: vec![],
        };
        let node =
            extract_discovered_node(&event(EventType::LogData, EventPayload::LogData(log)), 0)
                .unwrap();
        assert!(!node.is_repeater);
    }

    #[test]
    fn extract_discovered_node_defaults_name_and_position_when_absent() {
        let mut adv = sample_advertisement(2);
        adv.name = None;
        adv.lat = None;
        adv.lon = None;
        let log = LogData {
            snr: 4.0,
            rssi: -90,
            header: Some(advert_header(PayloadType::Advert)),
            advertisement: Some(adv),
            payload: vec![],
        };
        let node =
            extract_discovered_node(&event(EventType::LogData, EventPayload::LogData(log)), 0)
                .unwrap();
        assert_eq!(node.name, "");
        assert_eq!(node.lat, 0.0);
        assert_eq!(node.lon, 0.0);
    }

    // --- build_packet_log_entry -----------------------------------------

    #[test]
    fn build_packet_log_entry_ignores_non_log_data_events() {
        assert!(
            build_packet_log_entry(&event(EventType::Connected, EventPayload::None), 1, 0)
                .is_none()
        );
    }

    #[test]
    fn build_packet_log_entry_handles_undecodable_header() {
        let log = LogData {
            snr: 1.5,
            rssi: -80,
            header: None,
            advertisement: None,
            payload: vec![0xde, 0xad],
        };
        let entry = build_packet_log_entry(
            &event(EventType::LogData, EventPayload::LogData(log)),
            7,
            42,
        )
        .expect("entry should still be built without a header");

        assert_eq!(entry.id, 7);
        assert_eq!(entry.at_unix, 42);
        assert_eq!(entry.snr, 1.5);
        assert_eq!(entry.rssi, -80);
        assert!(entry.header.is_none());
        assert_eq!(entry.payload_hex, "dead");
        assert_eq!(entry.payload_len, 2);
    }

    #[test]
    fn build_packet_log_entry_decodes_header_without_advertisement() {
        let mut header = advert_header(PayloadType::Ack);
        header.transport_code = Some([1, 2, 3, 4]);
        let log = LogData {
            snr: 2.0,
            rssi: -70,
            header: Some(header),
            advertisement: None,
            payload: vec![],
        };
        let entry =
            build_packet_log_entry(&event(EventType::LogData, EventPayload::LogData(log)), 1, 0)
                .unwrap();
        let header = entry.header.expect("header should be decoded");

        assert_eq!(header.route_type, "Flood");
        assert_eq!(header.payload_type, "Ack");
        assert_eq!(header.hops, 2);
        assert_eq!(header.path_hash_size, 1);
        assert_eq!(header.path_hex, "1122");
        assert_eq!(header.transport_code_hex.as_deref(), Some("01020304"));
        assert!(header.advertisement.is_none());
        // Ack doesn't address a specific node, so no dest/src hashes.
        assert!(header.dest_hash_hex.is_none());
        assert!(header.src_hash_hex.is_none());
    }

    #[test]
    fn build_packet_log_entry_decodes_dest_and_src_hashes_for_addressed_payload_types() {
        let mut header = advert_header(PayloadType::TextMsg);
        header.payload_version = 0; // PAYLOAD_VER_1: dest/src hashes present
        let log = LogData {
            snr: 2.0,
            rssi: -70,
            header: Some(header),
            advertisement: None,
            payload: vec![0xde, 0xad, 0xbe, 0xef],
        };
        let entry =
            build_packet_log_entry(&event(EventType::LogData, EventPayload::LogData(log)), 1, 0)
                .unwrap();
        let header = entry.header.expect("header should be decoded");

        assert_eq!(header.dest_hash_hex.as_deref(), Some("de"));
        assert_eq!(header.src_hash_hex.as_deref(), Some("ad"));
        assert!(header.channel_hash_hex.is_none());
    }

    #[test]
    fn build_packet_log_entry_dest_and_src_hashes_are_always_one_byte_regardless_of_path_hash_size()
    {
        // The firmware fixes dest/src hashes at 1 byte each (PAYLOAD_VER_1),
        // independent of the header's path_hash_size, which only sizes the
        // path's own hop hashes and can be 1-4 bytes.
        let mut header = advert_header(PayloadType::Path);
        header.payload_version = 0;
        header.path_hash_size = 2;
        let log = LogData {
            snr: 2.0,
            rssi: -70,
            header: Some(header),
            advertisement: None,
            payload: vec![0x11, 0x22, 0x33],
        };
        let entry =
            build_packet_log_entry(&event(EventType::LogData, EventPayload::LogData(log)), 1, 0)
                .unwrap();
        let header = entry.header.expect("header should be decoded");

        assert_eq!(header.dest_hash_hex.as_deref(), Some("11"));
        assert_eq!(header.src_hash_hex.as_deref(), Some("22"));
    }

    #[test]
    fn build_packet_log_entry_no_dest_src_hashes_when_payload_too_short() {
        let mut header = advert_header(PayloadType::TextMsg);
        header.payload_version = 0;
        let log = LogData {
            snr: 2.0,
            rssi: -70,
            header: Some(header),
            advertisement: None,
            // Only 1 byte, but two 1-byte hashes need 2.
            payload: vec![0x11],
        };
        let entry =
            build_packet_log_entry(&event(EventType::LogData, EventPayload::LogData(log)), 1, 0)
                .unwrap();
        let header = entry.header.expect("header should be decoded");

        assert!(header.dest_hash_hex.is_none());
        assert!(header.src_hash_hex.is_none());
    }

    #[test]
    fn build_packet_log_entry_no_dest_src_hashes_for_non_addressed_payload_types() {
        let mut header = advert_header(PayloadType::GroupText);
        header.payload_version = 0;
        let log = LogData {
            snr: 2.0,
            rssi: -70,
            header: Some(header),
            advertisement: None,
            payload: vec![0xde, 0xad, 0xbe, 0xef],
        };
        let entry =
            build_packet_log_entry(&event(EventType::LogData, EventPayload::LogData(log)), 1, 0)
                .unwrap();
        let header = entry.header.expect("header should be decoded");

        assert!(header.dest_hash_hex.is_none());
        assert!(header.src_hash_hex.is_none());
    }

    #[test]
    fn build_packet_log_entry_no_dest_src_hashes_for_a_non_payload_ver_1_packet() {
        // advert_header defaults to payload_version 1 (not PAYLOAD_VER_1 /
        // 0), so the 1-byte hash layout assumed by extract_dest_src_hashes
        // isn't guaranteed — must not guess.
        let header = advert_header(PayloadType::TextMsg);
        assert_eq!(header.payload_version, 1);
        let log = LogData {
            snr: 2.0,
            rssi: -70,
            header: Some(header),
            advertisement: None,
            payload: vec![0xde, 0xad, 0xbe, 0xef],
        };
        let entry =
            build_packet_log_entry(&event(EventType::LogData, EventPayload::LogData(log)), 1, 0)
                .unwrap();
        let header = entry.header.expect("header should be decoded");

        assert!(header.dest_hash_hex.is_none());
        assert!(header.src_hash_hex.is_none());
    }

    #[test]
    fn build_packet_log_entry_anon_req_has_a_destination_hash_but_no_source_hash() {
        let mut header = advert_header(PayloadType::AnonReq);
        header.payload_version = 0;
        let log = LogData {
            snr: 2.0,
            rssi: -70,
            header: Some(header),
            advertisement: None,
            payload: vec![0xde, 0xad, 0xbe, 0xef],
        };
        let entry =
            build_packet_log_entry(&event(EventType::LogData, EventPayload::LogData(log)), 1, 0)
                .unwrap();
        let header = entry.header.expect("header should be decoded");

        assert_eq!(header.dest_hash_hex.as_deref(), Some("de"));
        assert!(header.src_hash_hex.is_none());
    }

    #[test]
    fn build_packet_log_entry_decodes_channel_hash_for_group_payload_types() {
        for payload_type in [PayloadType::GroupText, PayloadType::GroupData] {
            let mut header = advert_header(payload_type);
            header.payload_version = 0;
            let log = LogData {
                snr: 2.0,
                rssi: -70,
                header: Some(header),
                advertisement: None,
                payload: vec![0xab, 0xcd],
            };
            let entry = build_packet_log_entry(
                &event(EventType::LogData, EventPayload::LogData(log)),
                1,
                0,
            )
            .unwrap();
            let header = entry.header.expect("header should be decoded");

            assert_eq!(
                header.channel_hash_hex.as_deref(),
                Some("ab"),
                "{payload_type:?}"
            );
            // A channel isn't a node: no dest/src hash alongside it.
            assert!(header.dest_hash_hex.is_none());
            assert!(header.src_hash_hex.is_none());
        }
    }

    #[test]
    fn build_packet_log_entry_no_channel_hash_when_payload_empty_or_wrong_version() {
        let mut too_short = advert_header(PayloadType::GroupText);
        too_short.payload_version = 0;
        let log = LogData {
            snr: 2.0,
            rssi: -70,
            header: Some(too_short),
            advertisement: None,
            payload: vec![],
        };
        let entry =
            build_packet_log_entry(&event(EventType::LogData, EventPayload::LogData(log)), 1, 0)
                .unwrap();
        assert!(entry
            .header
            .expect("header should be decoded")
            .channel_hash_hex
            .is_none());

        // payload_version 1 (not PAYLOAD_VER_1) — default from advert_header.
        let wrong_version = advert_header(PayloadType::GroupText);
        let log = LogData {
            snr: 2.0,
            rssi: -70,
            header: Some(wrong_version),
            advertisement: None,
            payload: vec![0xab],
        };
        let entry =
            build_packet_log_entry(&event(EventType::LogData, EventPayload::LogData(log)), 1, 0)
                .unwrap();
        assert!(entry
            .header
            .expect("header should be decoded")
            .channel_hash_hex
            .is_none());
    }

    #[test]
    fn build_packet_log_entry_addressed_payload_types_never_get_a_channel_hash() {
        let mut header = advert_header(PayloadType::TextMsg);
        header.payload_version = 0;
        let log = LogData {
            snr: 2.0,
            rssi: -70,
            header: Some(header),
            advertisement: None,
            payload: vec![0xde, 0xad, 0xbe, 0xef],
        };
        let entry =
            build_packet_log_entry(&event(EventType::LogData, EventPayload::LogData(log)), 1, 0)
                .unwrap();
        assert!(entry
            .header
            .expect("header should be decoded")
            .channel_hash_hex
            .is_none());
    }

    #[test]
    fn build_packet_log_entry_decodes_advertisement_with_human_readable_type() {
        let log = LogData {
            snr: 2.0,
            rssi: -70,
            header: Some(advert_header(PayloadType::Advert)),
            advertisement: Some(sample_advertisement(4)), // Sensor
            payload: vec![],
        };
        let entry =
            build_packet_log_entry(&event(EventType::LogData, EventPayload::LogData(log)), 1, 0)
                .unwrap();
        let adv = entry
            .header
            .expect("header")
            .advertisement
            .expect("advertisement");

        assert_eq!(adv.public_key_hex, "ab".repeat(32));
        assert_eq!(adv.name.as_deref(), Some("Node A"));
        assert_eq!(adv.adv_type_name, "Sensor");
        assert_eq!(adv.lat, Some(48.85));
        assert_eq!(adv.lon, Some(2.35));
    }

    #[test]
    fn build_packet_log_entry_labels_unknown_advertiser_type() {
        let log = LogData {
            snr: 0.0,
            rssi: 0,
            header: Some(advert_header(PayloadType::Advert)),
            advertisement: Some(sample_advertisement(9)),
            payload: vec![],
        };
        let entry =
            build_packet_log_entry(&event(EventType::LogData, EventPayload::LogData(log)), 1, 0)
                .unwrap();
        assert_eq!(
            entry.header.unwrap().advertisement.unwrap().adv_type_name,
            "Unknown(9)"
        );
    }

    #[test]
    fn adv_type_name_labels_known_types() {
        assert_eq!(adv_type_name(1), "Chat");
        assert_eq!(adv_type_name(2), "Repeater");
        assert_eq!(adv_type_name(3), "Room");
        assert_eq!(adv_type_name(4), "Sensor");
        assert_eq!(adv_type_name(9), "Unknown(9)");
    }

    // --- map_event -------------------------------------------------------

    #[test]
    fn map_event_connected_and_disconnected() {
        assert!(matches!(
            map_event(&event(EventType::Connected, EventPayload::None)),
            Some(MeshEventKind::Connected)
        ));
        assert!(matches!(
            map_event(&event(EventType::Disconnected, EventPayload::None)),
            Some(MeshEventKind::Disconnected)
        ));
    }

    #[test]
    fn map_event_advertisement_converts_prefix_and_position() {
        let adv = AdvertisementData {
            prefix: [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
            name: "Repeater A".to_string(),
            lat: 48_850_000,
            lon: 2_350_000,
        };
        let mapped = map_event(&event(
            EventType::Advertisement,
            EventPayload::Advertisement(adv),
        ))
        .unwrap();
        match mapped {
            MeshEventKind::Advertisement {
                name,
                prefix_hex,
                lat,
                lon,
            } => {
                assert_eq!(name, "Repeater A");
                assert_eq!(prefix_hex, "aabbccddeeff");
                assert_eq!(lat, 48.85);
                assert_eq!(lon, 2.35);
            }
            other => panic!("expected Advertisement, got {other:?}"),
        }
    }

    #[test]
    fn map_event_contact_message() {
        let msg = ContactMessage {
            sender_prefix: [1, 2, 3, 4, 5, 6],
            path_len: 3,
            txt_type: 0,
            sender_timestamp: 0,
            text: "hello".to_string(),
            snr: None,
            signature: None,
        };
        let mapped = map_event(&event(
            EventType::ContactMsgRecv,
            EventPayload::ContactMessage(msg),
        ))
        .unwrap();
        match mapped {
            MeshEventKind::ContactMessage {
                from_prefix_hex,
                hops,
                text,
            } => {
                assert_eq!(from_prefix_hex, "010203040506");
                assert_eq!(hops, 3);
                assert_eq!(text, "hello");
            }
            other => panic!("expected ContactMessage, got {other:?}"),
        }
    }

    #[test]
    fn map_event_channel_message() {
        let msg = ChannelMessage {
            channel_idx: 2,
            path_len: 1,
            txt_type: 0,
            sender_timestamp: 0,
            text: "hi all".to_string(),
            snr: None,
        };
        let mapped = map_event(&event(
            EventType::ChannelMsgRecv,
            EventPayload::ChannelMessage(msg),
        ))
        .unwrap();
        match mapped {
            MeshEventKind::ChannelMessage {
                channel,
                hops,
                text,
            } => {
                assert_eq!(channel, 2);
                assert_eq!(hops, 1);
                assert_eq!(text, "hi all");
            }
            other => panic!("expected ChannelMessage, got {other:?}"),
        }
    }

    #[test]
    fn map_event_path_update() {
        let update = PathUpdateData {
            prefix: [1, 2, 3, 4, 5, 6],
            path_len: -1,
            path: vec![0xaa, 0xbb],
        };
        let mapped = map_event(&event(
            EventType::PathUpdate,
            EventPayload::PathUpdate(update),
        ))
        .unwrap();
        match mapped {
            MeshEventKind::PathUpdate {
                prefix_hex,
                hops,
                path_hex,
            } => {
                assert_eq!(prefix_hex, "010203040506");
                assert_eq!(hops, -1);
                assert_eq!(path_hex, "aabb");
            }
            other => panic!("expected PathUpdate, got {other:?}"),
        }
    }

    #[test]
    fn map_event_ack() {
        let mapped = map_event(&event(
            EventType::Ack,
            EventPayload::Ack { tag: [1, 2, 3, 4] },
        ))
        .unwrap();
        match mapped {
            MeshEventKind::Ack { tag_hex } => assert_eq!(tag_hex, "01020304"),
            other => panic!("expected Ack, got {other:?}"),
        }
    }

    #[test]
    fn map_event_log_data_is_filtered_out() {
        // Every received packet already shows up, with far more detail, on
        // the daemon's packet log (F3 page) — the dashboard event log must
        // not also carry a duplicate, lower-detail entry for it.
        let log = LogData {
            snr: 3.5,
            rssi: -95,
            header: None,
            advertisement: None,
            payload: vec![],
        };
        assert!(map_event(&event(EventType::LogData, EventPayload::LogData(log))).is_none());
    }

    #[test]
    fn map_event_ok_and_next_contact_are_filtered_out() {
        assert!(map_event(&event(EventType::Ok, EventPayload::None)).is_none());
        assert!(map_event(&event(EventType::NextContact, EventPayload::None)).is_none());
    }

    #[test]
    fn map_event_new_contact() {
        let mut public_key = [0u8; 32];
        public_key[..6].copy_from_slice(&[1, 2, 3, 4, 5, 6]);
        let contact = Contact {
            public_key,
            contact_type: 2,
            flags: 0,
            path_len: -1,
            out_path: vec![],
            adv_name: "Repeater A".to_string(),
            last_advert: 0,
            adv_lat: 0,
            adv_lon: 0,
            last_modification_timestamp: 0,
        };
        let mapped = map_event(&event(
            EventType::NewContact,
            EventPayload::Contact(contact),
        ))
        .unwrap();
        match mapped {
            MeshEventKind::NewContact { name } => assert_eq!(name, "Repeater A"),
            other => panic!("expected NewContact, got {other:?}"),
        }
    }

    #[test]
    fn map_event_msg_sent() {
        let mapped = map_event(&event(EventType::MsgSent, EventPayload::None)).unwrap();
        assert!(matches!(mapped, MeshEventKind::MessageSent));
    }

    #[test]
    fn map_event_unhandled_type_falls_back_to_other() {
        let mapped = map_event(&event(EventType::Battery, EventPayload::None)).unwrap();
        match mapped {
            MeshEventKind::Other { label } => assert_eq!(label, "Battery"),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    // --- DTO conversions --------------------------------------------------

    #[test]
    fn self_info_dto_converts_units() {
        let info = SelfInfo {
            adv_type: 1,
            tx_power: 22,
            max_tx_power: 22,
            public_key: [0xcd; 32],
            adv_lat: 48_850_000,
            adv_lon: 2_350_000,
            multi_acks: 0,
            adv_loc_policy: 0,
            telemetry_mode_base: 0,
            telemetry_mode_loc: 0,
            telemetry_mode_env: 0,
            manual_add_contacts: false,
            radio_freq: 869_525,
            radio_bw: 250_000,
            sf: 10,
            cr: 5,
            name: "Base station".to_string(),
        };
        let dto = SelfInfoDto::from(&info);

        assert_eq!(dto.name, "Base station");
        assert_eq!(dto.public_key_hex, "cd".repeat(32));
        assert_eq!(dto.radio_freq_mhz, 869.525);
        assert_eq!(dto.spreading_factor, 10);
        assert_eq!(dto.coding_rate, 5);
        assert_eq!(dto.tx_power_dbm, 22);
        assert_eq!(dto.lat, 48.85);
        assert_eq!(dto.lon, 2.35);
    }

    #[test]
    fn contact_dto_from_contact_marks_registered_and_unmanaged() {
        let mut public_key = [0u8; 32];
        public_key[..6].copy_from_slice(&[1, 2, 3, 4, 5, 6]);
        let contact = Contact {
            public_key,
            contact_type: 2,
            flags: 0,
            path_len: -1,
            out_path: vec![],
            adv_name: "Repeater".to_string(),
            last_advert: 1_700_000_000,
            adv_lat: 48_850_000,
            adv_lon: 2_350_000,
            last_modification_timestamp: 0,
        };
        let dto = ContactDto::from(&contact);

        assert_eq!(dto.name, "Repeater");
        assert_eq!(dto.public_key_prefix_hex, "010203040506");
        assert_eq!(dto.last_advert_unix, 1_700_000_000);
        assert_eq!(dto.lat, 48.85);
        assert_eq!(dto.lon, 2.35);
        assert!(dto.registered);
        assert!(!dto.managed);
    }
}
