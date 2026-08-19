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

use std::collections::HashMap;
use std::time::{Duration, Instant};

use meshcore_rs::events::{Contact, Neighbour, NeighboursData, SelfInfo, StatusData};
use meshcore_rs::parsing::hex_decode;
use meshcore_rs::{EventPayload, EventType, MeshCore, MeshCoreEvent, PayloadType};
use serde::{Deserialize, Serialize};

use crate::config::{ConnectionConfig, ManagedRepeater, RepeaterStatus};
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
    pub radio_bw_khz: f64,
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
            radio_bw_khz: info.radio_bw as f64 / 1000.0,
            spreading_factor: info.sf,
            coding_rate: info.cr,
            tx_power_dbm: info.tx_power,
            lat: info.adv_lat as f64 / 1_000_000.0,
            lon: info.adv_lon as f64 / 1_000_000.0,
        }
    }
}

/// Device model/firmware info, ready to display — see
/// [`MeshClient::device_info`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfoDto {
    pub model: String,
    pub firmware_version: String,
}

impl From<&meshcore_rs::events::DeviceInfoData> for DeviceInfoDto {
    fn from(info: &meshcore_rs::events::DeviceInfoData) -> Self {
        let model = info.model.clone().unwrap_or_else(|| "unknown".to_string());
        let firmware_version = match (&info.version, &info.fw_build) {
            (Some(version), Some(build)) => {
                let version = version.strip_prefix('v').unwrap_or(version);
                format!("v{version} (Build: {build})")
            }
            _ => "unknown".to_string(),
        };
        Self {
            model,
            firmware_version,
        }
    }
}

/// Core device stats — see [`MeshClient::node_stats`]. Field names match
/// `meshcore-rs`'s `CoreStatsData` (itself matching `meshcore_py`'s
/// `reader.py` dict keys), for MQTT/JSON compatibility with downstream
/// tools that already consume that shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreStatsDto {
    pub battery_mv: u16,
    pub uptime_secs: u32,
    pub errors: u16,
    pub queue_len: u8,
}

impl From<meshcore_rs::events::CoreStatsData> for CoreStatsDto {
    fn from(stats: meshcore_rs::events::CoreStatsData) -> Self {
        Self {
            battery_mv: stats.battery_mv,
            uptime_secs: stats.uptime_secs,
            errors: stats.errors,
            queue_len: stats.queue_len,
        }
    }
}

/// Radio stats — see [`MeshClient::node_stats`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RadioStatsDto {
    pub noise_floor: i16,
    pub last_rssi: i8,
    pub last_snr: f32,
    pub tx_air_secs: u32,
    pub rx_air_secs: u32,
}

impl From<meshcore_rs::events::RadioStatsData> for RadioStatsDto {
    fn from(stats: meshcore_rs::events::RadioStatsData) -> Self {
        Self {
            noise_floor: stats.noise_floor,
            last_rssi: stats.last_rssi,
            last_snr: stats.last_snr,
            tx_air_secs: stats.tx_air_secs,
            rx_air_secs: stats.rx_air_secs,
        }
    }
}

/// Packet counters — see [`MeshClient::node_stats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PacketStatsDto {
    pub recv: u32,
    pub sent: u32,
    pub flood_tx: u32,
    pub direct_tx: u32,
    pub flood_rx: u32,
    pub direct_rx: u32,
    pub recv_errors: Option<u32>,
}

impl From<meshcore_rs::events::PacketStatsData> for PacketStatsDto {
    fn from(stats: meshcore_rs::events::PacketStatsData) -> Self {
        Self {
            recv: stats.recv,
            sent: stats.sent,
            flood_tx: stats.flood_tx,
            direct_tx: stats.direct_tx,
            flood_rx: stats.flood_rx,
            direct_rx: stats.direct_rx,
            recv_errors: stats.recv_errors,
        }
    }
}

/// Node stats, fetched best-effort per category — see
/// [`MeshClient::node_stats`]. Each category is `None` if its RPC failed
/// (e.g. older firmware pre-dating `CMD_GET_STATS`), independent of
/// whether the others succeeded. Flattened when serialized (JSON keys
/// merge into one flat object, matching `agessaman/meshcore-packet-capture`'s
/// own `stats` shape) rather than nested `core`/`radio`/`packets` objects.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeStatsDto {
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub core: Option<CoreStatsDto>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub radio: Option<RadioStatsDto>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub packets: Option<PacketStatsDto>,
}

/// A telemetry read from a remote node — see [`MeshClient::request_telemetry`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelemetryDto {
    pub fetched_at_unix: i64,
    pub readings: Vec<crate::telemetry::TelemetryReading>,
}

/// A remote node's live status (battery, radio, packet counters) fetched
/// on demand over the mesh — see [`MeshClient::request_status`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusDto {
    pub fetched_at_unix: i64,
    pub battery_mv: u16,
    pub tx_queue_len: u16,
    pub noise_floor: i16,
    pub last_rssi: i16,
    pub last_snr: f32,
    pub packets_received: u32,
    pub packets_sent: u32,
    pub duplicate_packets: u32,
    pub airtime_secs: u32,
    pub rx_airtime_secs: u32,
    pub uptime_secs: u32,
    pub flood_sent: u32,
    pub direct_sent: u32,
}

/// A single mesh neighbour reported by a remote node — see
/// [`NeighboursDto`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NeighbourDto {
    pub public_key_prefix_hex: String,
    pub secs_ago: i32,
    pub snr: f32,
}

/// A remote node's list of mesh neighbours (nodes it has directly heard),
/// fetched on demand over the mesh — see [`MeshClient::request_neighbours`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NeighboursDto {
    pub fetched_at_unix: i64,
    pub total: u16,
    pub neighbours: Vec<NeighbourDto>,
}

/// A single entry in a remote node's configured region hierarchy — see
/// [`RegionHierarchyDto`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegionHierarchyEntryDto {
    pub name: String,
    /// Nesting depth in the hierarchy (0 = top-level).
    pub depth: u8,
    /// Whether this is the node's configured "home" region.
    pub is_home: bool,
    pub flood_allowed: bool,
}

/// A remote node's own configured region hierarchy, fetched on demand —
/// see [`MeshClient::request_region_hierarchy`]. Distinct from this
/// project's own `regions` config (`Config::regions`): this is what's
/// actually configured on the physical node's firmware.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegionHierarchyDto {
    pub fetched_at_unix: i64,
    pub entries: Vec<RegionHierarchyEntryDto>,
    /// The raw reply text, kept alongside the parsed entries -- the
    /// firmware's reply buffer is a fixed ~160 characters, so a large
    /// hierarchy can truncate mid-line; surfacing the raw text makes that
    /// visible instead of silently swallowed by parsing.
    pub raw_text: String,
}

/// Result of fetching a repeater's status/telemetry/neighbours/region
/// hierarchy together in one combined command (one login, then all four
/// requests sequentially — see `daemon::mesh_task::request_repeater_detail`).
/// Best-effort per category, like [`NodeStatsDto`]: `errors` explains any
/// category that came back `None`, in the order attempted (status,
/// telemetry, neighbours, regions). This is the CLI-side *accumulator* that
/// [`RepeaterDetailCategory`] updates get folded into via
/// [`RepeaterDetailDto::apply_category`] as they stream in — it's no
/// longer sent over IPC as one complete value (see
/// `ServerMessage::RepeaterDetailCategory`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RepeaterDetailDto {
    pub status: Option<StatusDto>,
    pub telemetry: Option<TelemetryDto>,
    pub neighbours: Option<NeighboursDto>,
    pub regions: Option<RegionHierarchyDto>,
    pub errors: Vec<String>,
}

impl RepeaterDetailDto {
    /// Folds one streamed [`RepeaterDetailCategory`] update into this
    /// accumulator: a success fills in the matching field, a failure is
    /// recorded as `"{category}: {err}"` in `errors` — the exact format
    /// `error_line_for` (`cli/src/tui/ui.rs`) matches against by prefix, so
    /// keep both in sync if this changes.
    pub fn apply_category(&mut self, category: RepeaterDetailCategory) {
        match category {
            RepeaterDetailCategory::Status(Ok(status)) => self.status = Some(status),
            RepeaterDetailCategory::Status(Err(err)) => self.errors.push(format!("status: {err}")),
            RepeaterDetailCategory::Telemetry(Ok(telemetry)) => self.telemetry = Some(telemetry),
            RepeaterDetailCategory::Telemetry(Err(err)) => {
                self.errors.push(format!("telemetry: {err}"))
            }
            RepeaterDetailCategory::Neighbours(Ok(neighbours)) => {
                self.neighbours = Some(neighbours)
            }
            RepeaterDetailCategory::Neighbours(Err(err)) => {
                self.errors.push(format!("neighbours: {err}"))
            }
            RepeaterDetailCategory::Regions(Ok(regions)) => self.regions = Some(regions),
            RepeaterDetailCategory::Regions(Err(err)) => {
                self.errors.push(format!("regions: {err}"))
            }
        }
    }
}

/// One category of a [`RepeaterDetailDto`] fetch's outcome, streamed to the
/// IPC client as soon as it's available rather than batched into one final
/// message — see `daemon::mesh_task::request_repeater_detail` and
/// `ServerMessage::RepeaterDetailCategory`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RepeaterDetailCategory {
    Status(std::result::Result<StatusDto, String>),
    Telemetry(std::result::Result<TelemetryDto, String>),
    Neighbours(std::result::Result<NeighboursDto, String>),
    Regions(std::result::Result<RegionHierarchyDto, String>),
}

fn status_dto_from(status: StatusData, fetched_at_unix: i64) -> StatusDto {
    StatusDto {
        fetched_at_unix,
        battery_mv: status.battery_mv,
        tx_queue_len: status.tx_queue_len,
        noise_floor: status.noise_floor,
        last_rssi: status.last_rssi,
        last_snr: status.snr,
        packets_received: status.nb_recv,
        packets_sent: status.nb_sent,
        duplicate_packets: status.dup_count,
        airtime_secs: status.airtime / 1000,
        rx_airtime_secs: status.rx_airtime / 1000,
        uptime_secs: status.uptime,
        flood_sent: status.flood_sent,
        direct_sent: status.direct_sent,
    }
}

fn neighbours_dto_from(data: NeighboursData, fetched_at_unix: i64) -> NeighboursDto {
    NeighboursDto {
        fetched_at_unix,
        total: data.total,
        neighbours: data
            .neighbours
            .iter()
            .map(|n: &Neighbour| NeighbourDto {
                public_key_prefix_hex: hex_encode(&n.pubkey),
                secs_ago: n.secs_ago,
                snr: n.snr,
            })
            .collect(),
    }
}

/// Parses the reply text of a repeater's `"region"` admin CLI command
/// (`RegionMap::printChildRegions`/`exportTo`, firmware
/// `src/helpers/RegionMap.cpp:287-308`) into structured entries.
///
/// Each line's leading-space count is the entry's depth; a trailing `" F"`
/// marks flood as allowed (stripped first, matching the firmware's
/// `"%s%s F\n"` format — the space is part of the marker, not padding);
/// a further trailing `"^"` marks the node's home region. Best-effort: an
/// empty or entirely blank line is skipped rather than erroring, since the
/// firmware's fixed ~160-byte reply buffer can truncate a large hierarchy
/// mid-line.
fn parse_region_hierarchy(raw_text: &str, fetched_at_unix: i64) -> RegionHierarchyDto {
    let entries = raw_text
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim_start_matches(' ');
            let depth = (line.len() - trimmed.len()) as u8;
            let trimmed = trimmed.trim_end_matches('\r');
            if trimmed.is_empty() {
                return None;
            }
            let flood_allowed = trimmed.ends_with(" F");
            let without_flood = trimmed.strip_suffix(" F").unwrap_or(trimmed);
            let is_home = without_flood.ends_with('^');
            let name = without_flood.strip_suffix('^').unwrap_or(without_flood);
            Some(RegionHierarchyEntryDto {
                name: name.to_string(),
                depth,
                is_home,
                flood_allowed,
            })
        })
        .collect();
    RegionHierarchyDto {
        fetched_at_unix,
        entries,
        raw_text: raw_text.to_string(),
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
    /// Whether this node is in the config's `managed_repeaters` list with
    /// status `Managed` or `Supervised` (both get the same automation/
    /// highlighting treatment today — see [`RepeaterStatus`]).
    pub managed: bool,
    /// This repeater's configured management tier, if it matches a
    /// `managed_repeaters` entry — `None` for a contact that's registered/
    /// discovered but not configured at all. Needed (not just `managed`)
    /// so the TUI can tell "Known" apart from "not configured" and can
    /// toggle each tier independently — see
    /// `cli::tui::mod::set_repeater_status`.
    #[serde(default)]
    pub repeater_status: Option<RepeaterStatus>,
    /// Advertiser type byte (see [`adv_type_name`]/`CONTACT_TYPENAMES`:
    /// 1=Chat, 2=Repeater, 3=Room, 4=Sensor).
    pub contact_type: u8,
    /// Last telemetry fetched on demand from this node (see
    /// `DaemonCommand::RequestTelemetry`), if any — populated from the
    /// daemon's cache, not re-fetched from the node on every snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_telemetry: Option<TelemetryDto>,
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
            repeater_status: None,
            contact_type: c.contact_type,
            last_telemetry: None,
        }
    }
}

/// Whether an advertiser type byte (`ContactDto::contact_type`/
/// `DiscoveredNode::adv_type`) identifies a repeater or a room server —
/// the two node types the TUI's "Repeaters" panel is scoped to, as opposed
/// to plain chat clients or sensors.
pub fn is_repeater_or_room(contact_type: u8) -> bool {
    matches!(contact_type, 2 | 3)
}

/// Contacts that aren't in `managed_repeaters` and should be pruned when
/// `observer_node_managed_config` is enforced.
///
/// Deliberately status-agnostic: a contact matching *any* `managed_repeaters`
/// entry is protected, regardless of that entry's [`RepeaterStatus`] --
/// `Known`/`Supervised` repeaters must survive observer-node pruning
/// exactly like `Managed` ones, not just fully-managed contacts.
pub fn contacts_to_prune<'a>(
    contacts: &'a [ContactDto],
    managed_repeaters: &[ManagedRepeater],
) -> Vec<&'a ContactDto> {
    contacts
        .iter()
        .filter(|c| {
            !managed_repeaters
                .iter()
                .any(|r| r.matches(&c.public_key_prefix_hex))
        })
        .collect()
}

/// The `managed_repeaters` entry's status matching this prefix, if any —
/// used to populate [`ContactDto::repeater_status`] (and, derived from it,
/// [`ContactDto::managed`]) in `daemon::mesh_task::build_snapshot_contacts`.
pub fn matching_repeater_status(
    managed_repeaters: &[ManagedRepeater],
    public_key_prefix_hex: &str,
) -> Option<RepeaterStatus> {
    managed_repeaters
        .iter()
        .find(|r| r.matches(public_key_prefix_hex))
        .map(|r| r.status)
}

/// Whether `public_key_prefix_hex` matches one of `contacts` — used before
/// attempting a login/request against a contact that might only be
/// "discovered" (overheard on the mesh) but never actually declared to the
/// companion, which would otherwise fail deep inside `MeshClient` with a
/// confusing low-level "no contact matches prefix" error repeated across
/// every category. See `daemon::mesh_task::request_repeater_detail`.
pub fn is_registered_contact(contacts: &[ContactDto], public_key_prefix_hex: &str) -> bool {
    contacts.iter().any(|c| {
        c.public_key_prefix_hex
            .eq_ignore_ascii_case(public_key_prefix_hex)
    })
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
    /// Advertiser type byte, see [`ContactDto::contact_type`].
    pub adv_type: u8,
    pub lat: f64,
    pub lon: f64,
    pub last_seen_unix: i64,
    /// Signal-to-noise ratio of the RF log data this sighting was decoded
    /// from — `None` if never captured alongside an advertisement.
    pub last_snr: Option<f32>,
    /// Received signal strength (dBm) of the RF log data this sighting was
    /// decoded from.
    pub last_rssi: Option<i16>,
    /// Hop count (`path_len`) of the RF log data this sighting was decoded
    /// from.
    pub last_hop_count: Option<u8>,
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
        adv_type: adv.adv_type,
        lat: adv.lat.map(|v| v as f64 / 1_000_000.0).unwrap_or(0.0),
        lon: adv.lon.map(|v| v as f64 / 1_000_000.0).unwrap_or(0.0),
        last_seen_unix: now_unix,
        last_snr: Some(log.snr),
        last_rssi: Some(log.rssi),
        last_hop_count: Some(header.path_len),
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
    /// Raw numeric route type byte (see `meshcore_rs::packets::RouteType`),
    /// preserved alongside the human-readable `route_type` string — needed
    /// to reconstruct the original packet header byte for
    /// [`reconstruct_raw_packet_hex`].
    pub route_type_raw: u8,
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
    /// The sender's full public key, present only on `AnonReq` payloads —
    /// see [`extract_anon_req_sender_public_key`]. Unlike the encrypted
    /// body that follows it (a pairwise ECDH secret between the sender
    /// and recipient identities, unreadable to a passive RF observer),
    /// this is sent in the clear.
    pub anon_req_sender_public_key_hex: Option<String>,
    /// Populated when `payload_type` is `"Advert"` and the inner payload
    /// could be decoded.
    pub advertisement: Option<PacketAdvertInfo>,
    /// Decoded `Control` payload, for the two sub-types documented in
    /// `docs/payloads.md` (`docs.meshcore.io/payloads`) — see
    /// [`decode_control_payload`]. Unlike `Req`/`Response`/`Path`/
    /// `AnonReq`, `Control` data is plaintext, not ECDH-encrypted.
    pub control: Option<ControlPayloadInfo>,
}

/// A decoded `Control` payload — see [`PacketHeaderInfo::control`] and
/// [`decode_control_payload`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ControlPayloadInfo {
    /// A node discovery request, broadcast zero-hop
    /// (`CTL_TYPE_NODE_DISCOVER_REQ` = `0x8` sub-type,
    /// `examples/simple_repeater/MyMesh.cpp`).
    DiscoverReq {
        /// If set, a `DiscoverResp` should reply with an 8-byte public key
        /// prefix instead of the full 32 bytes.
        prefix_only: bool,
        /// Bitmask over advertiser types (see [`adv_type_name`]) the
        /// sender is looking for — bit `N` set means type `N` matches.
        type_filter: u8,
        tag_hex: String,
        /// Only nodes modified since this Unix timestamp should reply;
        /// `None` when the (optional) field was omitted (matches any).
        since_unix: Option<u32>,
    },
    /// A reply to a `DiscoverReq`, echoing its tag
    /// (`CTL_TYPE_NODE_DISCOVER_RESP` = `0x9` sub-type).
    DiscoverResp {
        /// Human-readable advertiser type of the responding node.
        node_type_name: String,
        /// Signal quality of the request as heard by the responder.
        snr: f32,
        tag_hex: String,
        /// The responding node's public key — full 32 bytes, or an
        /// 8-byte prefix if the request set `prefix_only`.
        pubkey_hex: String,
    },
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

pub fn adv_type_name(adv_type: u8) -> String {
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

/// Size in bytes of a full MeshCore public key (`PUB_KEY_SIZE`, `src/MeshCore.h`).
const PUB_KEY_SIZE: usize = 32;

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

/// Extracts the sender's full public key from an `AnonReq` payload — the
/// only payload type where it's sent in the clear (verified against
/// `Mesh.cpp::onRecvPacket`'s `PAYLOAD_TYPE_ANON_REQ` case: `uint8_t*
/// sender_pub_key = &pkt->payload[i]; i += PUB_KEY_SIZE;`, right after the
/// 1-byte destination hash, unlike `Req`/`Response`/`TextMsg`/`Path`
/// which reference an already-known contact instead). This is metadata,
/// not decrypted content: the rest of the payload (MAC + ciphertext) is
/// encrypted with an ECDH secret pairwise between the sender and
/// recipient identities, which a passive RF observer never has access to
/// — see [`PacketHeaderInfo::anon_req_sender_public_key_hex`].
///
/// `None` for any other payload type, a too-short payload, or a
/// non-`PAYLOAD_VER_1` payload version (same sizing caveat as
/// [`extract_dest_src_hashes`]).
pub fn extract_anon_req_sender_public_key(
    payload_type: PayloadType,
    payload_version: u8,
    payload: &[u8],
) -> Option<String> {
    if payload_type != PayloadType::AnonReq || payload_version != 0 {
        return None;
    }
    if payload.len() < ADDRESS_HASH_SIZE + PUB_KEY_SIZE {
        return None;
    }
    Some(hex_encode(
        &payload[ADDRESS_HASH_SIZE..ADDRESS_HASH_SIZE + PUB_KEY_SIZE],
    ))
}

/// Decodes a `Control` payload's two documented sub-types
/// (`docs/payloads.md`, cross-checked against `examples/simple_repeater/MyMesh.cpp`'s
/// `onControlDataRecv`): the sub-type is the upper 4 bits of the first
/// byte (`0x8` = `DiscoverReq`, `0x9` = `DiscoverResp`).
///
/// ```text
/// DiscoverReq:  [flags: 1 (0x8_ | prefix_only)][type_filter: 1][tag: 4 LE][since: 4 LE, optional]
/// DiscoverResp: [flags: 1 (0x9_ | node_type)][snr: 1 (i8, *4)][tag: 4 LE][pubkey: 8 or 32]
/// ```
///
/// Returns `None` for any other payload type, a non-`PAYLOAD_VER_1`
/// payload version, an unrecognized sub-type, or a payload too short to
/// hold its sub-type's fixed fields.
pub fn decode_control_payload(
    payload_type: PayloadType,
    payload_version: u8,
    payload: &[u8],
) -> Option<ControlPayloadInfo> {
    if payload_type != PayloadType::Control || payload_version != 0 {
        return None;
    }
    if payload.len() < 6 {
        return None;
    }
    let flags = payload[0];
    let tag_hex = hex_encode(&payload[2..6]);

    match flags & 0xF0 {
        0x80 => {
            let since_unix = (payload.len() >= 10)
                .then(|| u32::from_le_bytes(payload[6..10].try_into().expect("checked length")));
            Some(ControlPayloadInfo::DiscoverReq {
                prefix_only: flags & 0x01 != 0,
                type_filter: payload[1],
                tag_hex,
                since_unix,
            })
        }
        0x90 => {
            let pubkey = if payload.len() >= 6 + PUB_KEY_SIZE {
                &payload[6..6 + PUB_KEY_SIZE]
            } else if payload.len() >= 6 + 8 {
                &payload[6..14]
            } else {
                return None;
            };
            Some(ControlPayloadInfo::DiscoverResp {
                node_type_name: adv_type_name(flags & 0x0F),
                snr: (payload[1] as i8) as f32 / 4.0,
                tag_hex,
                pubkey_hex: hex_encode(pubkey),
            })
        }
        _ => None,
    }
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
        let anon_req_sender_public_key_hex =
            extract_anon_req_sender_public_key(h.payload_type, h.payload_version, &log.payload);
        let control = decode_control_payload(h.payload_type, h.payload_version, &log.payload);
        PacketHeaderInfo {
            route_type: format!("{:?}", h.route_type),
            route_type_raw: h.route_type as u8,
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
            anon_req_sender_public_key_hex,
            advertisement: log.advertisement.as_ref().map(|a| PacketAdvertInfo {
                public_key_hex: hex_encode(&a.public_key),
                name: a.name.clone(),
                adv_type_name: adv_type_name(a.adv_type),
                lat: a.lat.map(|v| v as f64 / 1_000_000.0),
                lon: a.lon.map(|v| v as f64 / 1_000_000.0),
            }),
            control,
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

/// Reconstructs the exact over-the-air packet bytes (as hex) a
/// [`PacketLogEntry`] was decoded from — the inverse of
/// `meshcore_rs::parsing::parse_mesh_packet_header`, whose header byte
/// layout is bits 0-1 = route type, bits 2-5 = payload type, bits 6-7 =
/// payload version, followed by an optional 4-byte transport code, a path
/// byte (bits 6-7 = `path_hash_size - 1`, bits 0-5 = path length), the path
/// hop hashes, then the inner payload. `None` if `entry.header` is `None`
/// (payload too short to have contained a decodable header in the first
/// place — nothing to reconstruct).
pub fn reconstruct_raw_packet_hex(entry: &PacketLogEntry) -> Option<String> {
    let header = entry.header.as_ref()?;
    let header_byte = (header.payload_version << 6)
        | ((header.payload_type_raw & 0x0F) << 2)
        | (header.route_type_raw & 0x03);
    let path_byte = (header.path_hash_size.saturating_sub(1) << 6) | (header.hops & 0x3F);

    let mut raw_hex = hex_encode(&[header_byte]);
    if let Some(transport_code_hex) = &header.transport_code_hex {
        raw_hex.push_str(transport_code_hex);
    }
    raw_hex.push_str(&hex_encode(&[path_byte]));
    raw_hex.push_str(&header.path_hex);
    raw_hex.push_str(&entry.payload_hex);
    Some(raw_hex)
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
    /// A repeater's configured management tier (`RepeaterStatus`) was
    /// created, changed or cleared via `ClientMessage::SetRepeaterStatus`
    /// (the TUI's `m`/`k`/`s` keys or `repeater manage`/`unmanage`).
    /// Distinct from `ManagedRepeaterDeclared` (which only fires when the
    /// repeater becomes a companion contact for the first time): this
    /// fires on *every* status change, including changing an
    /// already-registered repeater's tier or clearing it -- clients need
    /// this to know to refresh, since a tier change on an already-known
    /// contact wouldn't otherwise trigger any event at all.
    RepeaterStatusChanged {
        name: String,
        /// `None` if cleared (removed from `managed_repeaters` entirely).
        status: Option<RepeaterStatus>,
    },
    /// `observer_node_managed_config` corrected something on the connected
    /// node (auto-add, a channel, or a non-managed contact) to keep it in
    /// an observer-only state.
    ObserverNodeConfigEnforced {
        detail: String,
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
    /// The daemon reloaded `config.toml` in response to `SIGHUP`. Synthesized
    /// by the daemon itself, not derived from a node/firmware event.
    ConfigReloaded {
        summary: String,
    },
    /// Telemetry was fetched on demand from a remote node (see
    /// `DaemonCommand::RequestTelemetry`).
    TelemetryReceived {
        name: String,
        summary: String,
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

/// Timeout for a single mesh-routed request (status/telemetry/neighbours)
/// to a remote node -- meshcore-rs's own default (`DEFAULT_TIMEOUT`, 5s)
/// is sized for companion-local RPCs, a bit tight for a real multi-hop
/// mesh round trip in practice. Kept modest (rather than e.g. 20s) because
/// `daemon::mesh_task::request_repeater_detail` runs status/telemetry/
/// neighbours *sequentially*, not concurrently -- meshcore-rs's
/// `commands()` mutex is held for a request's entire round trip (send +
/// wait), so three concurrent calls would serialize behind it anyway
/// (confirmed while implementing this) without actually overlapping.
const MESH_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

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

    /// Queries the connected device for its model/firmware info. Unlike
    /// [`Self::self_info`] (populated automatically on connect), this is an
    /// explicit RPC — best-effort, `None` if the device doesn't answer.
    pub async fn device_info(&self) -> Option<DeviceInfoDto> {
        self.inner
            .commands()
            .lock()
            .await
            .send_device_query()
            .await
            .ok()
            .as_ref()
            .map(DeviceInfoDto::from)
    }

    /// Number of channel slots the node has, if it answers the query.
    /// Not exposed via [`DeviceInfoDto`]/the IPC snapshot -- only needed
    /// internally by `observer_node_managed_config` enforcement, which
    /// must iterate every slot.
    pub async fn max_channels(&self) -> Option<u8> {
        self.inner
            .commands()
            .lock()
            .await
            .send_device_query()
            .await
            .ok()
            .and_then(|info| info.max_channels)
    }

    /// Fetches core/radio/packet stats from the node, best-effort per
    /// category: one RPC failing (e.g. older firmware pre-dating
    /// `CMD_GET_STATS`, companion-v1.11.0) doesn't blank out the others,
    /// matching `agessaman/meshcore-packet-capture`'s own per-category
    /// try/except when refreshing stats.
    pub async fn node_stats(&self) -> NodeStatsDto {
        let commands = self.inner.commands();
        let core = commands.lock().await.get_core_stats().await.ok();
        let radio = commands.lock().await.get_radio_stats().await.ok();
        let packets = commands.lock().await.get_packet_stats().await.ok();

        NodeStatsDto {
            core: core.map(CoreStatsDto::from),
            radio: radio.map(RadioStatsDto::from),
            packets: packets.map(PacketStatsDto::from),
        }
    }

    /// Signs arbitrary caller-supplied bytes on-device (`CMD_SIGN_START`/
    /// `CMD_SIGN_DATA`/`CMD_SIGN_FINISH`) — the node's private key never
    /// leaves the device. Used for MQTT device-signed auth tokens (see
    /// [`crate::mqtt_jwt`]); `SIGN_CHUNK_SIZE` has no real effect on a
    /// signing input this small, but a value is required by the underlying
    /// `meshcore-rs` API.
    pub async fn sign(&self, data: &[u8]) -> Result<Vec<u8>> {
        const SIGN_CHUNK_SIZE: usize = 128;
        let signature = self
            .inner
            .commands()
            .lock()
            .await
            .sign(data, SIGN_CHUNK_SIZE)
            .await?;
        Ok(signature)
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
    /// would otherwise stay stale. The daemon's snapshot-building code
    /// always uses this rather than [`Self::contacts`] for exactly this
    /// reason — see `daemon::mesh_task::build_snapshot_contacts`.
    pub async fn fetch_contacts(&self) -> Result<Vec<ContactDto>> {
        let contacts = self.inner.commands().lock().await.get_contacts(0).await?;
        Ok(contacts.iter().map(ContactDto::from).collect())
    }

    /// Removes a contact from the node's own contact list, identified by
    /// its public key prefix (hex).
    ///
    /// The companion's `CMD_REMOVE_CONTACT` requires the contact's *full*
    /// 32-byte public key, not just the prefix carried over IPC — so this
    /// resolves the prefix against the cached contact list first, rather
    /// than forwarding the prefix straight through (which would either
    /// fail outright or, with an older `meshcore-rs`, send a malformed,
    /// too-short command that the node never responds to).
    pub async fn remove_contact(&self, public_key_prefix_hex: &str) -> Result<()> {
        let prefix = hex_decode(public_key_prefix_hex)?;
        let contact = self
            .inner
            .get_contact_by_prefix(&prefix)
            .await
            .ok_or_else(|| {
                Error::Other(format!("no contact matches prefix {public_key_prefix_hex}"))
            })?;

        self.inner
            .commands()
            .lock()
            .await
            .remove_contact(&contact)
            .await?;
        Ok(())
    }

    /// Resolves a public key prefix against the companion's cached contact
    /// list, for commands that need the contact's full 32-byte public key
    /// (a mere prefix isn't enough to build a mesh-routed request).
    async fn resolve_contact(&self, public_key_prefix_hex: &str) -> Result<Contact> {
        let prefix = hex_decode(public_key_prefix_hex)?;
        self.inner
            .get_contact_by_prefix(&prefix)
            .await
            .ok_or_else(|| {
                Error::Other(format!("no contact matches prefix {public_key_prefix_hex}"))
            })
    }

    /// Logs into a remote node (typically a repeater) using its admin or
    /// guest password, required before it will answer authenticated
    /// requests such as telemetry (`CMD_SEND_LOGIN`). Unlike a plain send,
    /// this waits for the node's own login confirmation
    /// (`LoginSuccess`/`LoginFailed`) rather than just the companion's
    /// local send-ack, since [`Self::request_telemetry`] sent immediately
    /// after would otherwise race the repeater's ACL registration.
    pub async fn login(&self, public_key_prefix_hex: &str, password: &str) -> Result<()> {
        const LOGIN_TIMEOUT: Duration = Duration::from_secs(20);

        let contact = self.resolve_contact(public_key_prefix_hex).await?;
        let commands = self.inner.commands();
        commands.lock().await.send_login(&contact, password).await?;

        let event = commands
            .lock()
            .await
            .wait_for_any_event(
                &[EventType::LoginSuccess, EventType::LoginFailed],
                LOGIN_TIMEOUT,
            )
            .await?;

        match event.event_type {
            EventType::LoginSuccess => Ok(()),
            _ => Err(Error::Other(format!(
                "login to {public_key_prefix_hex} was rejected (wrong password?)"
            ))),
        }
    }

    /// Logs out of a remote node previously logged into via [`Self::login`].
    /// Best-effort by design (fire-and-forget on the wire, matching
    /// `meshcore-rs`'s own `send_logout`) — considerate towards a
    /// repeater's limited ACL table size, not required for correctness.
    pub async fn logout(&self, public_key_prefix_hex: &str) -> Result<()> {
        let contact = self.resolve_contact(public_key_prefix_hex).await?;
        self.inner
            .commands()
            .lock()
            .await
            .send_logout(&contact)
            .await?;
        Ok(())
    }

    /// Requests and decodes CayenneLPP telemetry (battery voltage,
    /// temperature, ...) from a remote node over the mesh. Most repeaters
    /// require [`Self::login`] first — see `ManagedRepeater::password`.
    pub async fn request_telemetry(
        &self,
        public_key_prefix_hex: &str,
    ) -> Result<Vec<crate::telemetry::TelemetryReading>> {
        let contact = self.resolve_contact(public_key_prefix_hex).await?;
        let raw = self
            .inner
            .commands()
            .lock()
            .await
            .request_telemetry_with_timeout(&contact, MESH_REQUEST_TIMEOUT)
            .await?;
        Ok(crate::telemetry::decode(&raw))
    }

    /// Requests a remote node's live status (battery, uptime, radio/packet
    /// counters) over the mesh. Most repeaters require [`Self::login`]
    /// first — see `ManagedRepeater::password`.
    pub async fn request_status(&self, public_key_prefix_hex: &str) -> Result<StatusDto> {
        let contact = self.resolve_contact(public_key_prefix_hex).await?;
        let status: StatusData = self
            .inner
            .commands()
            .lock()
            .await
            .request_status_with_timeout(&contact, MESH_REQUEST_TIMEOUT)
            .await?;
        Ok(status_dto_from(status, chrono::Utc::now().timestamp()))
    }

    /// Requests a remote node's list of mesh neighbours (nodes it has
    /// directly heard) over the mesh. Most repeaters require
    /// [`Self::login`] first — see `ManagedRepeater::password`.
    pub async fn request_neighbours(&self, public_key_prefix_hex: &str) -> Result<NeighboursDto> {
        let contact = self.resolve_contact(public_key_prefix_hex).await?;
        let neighbours: NeighboursData = self
            .inner
            .commands()
            .lock()
            .await
            .request_neighbours_with_timeout(
                &contact,
                32, // count
                0,  // offset
                0,  // order_by
                6,  // pubkey_prefix_length -- this codebase's usual 6-byte/12-hex-char prefix
                MESH_REQUEST_TIMEOUT,
            )
            .await?;
        Ok(neighbours_dto_from(
            neighbours,
            chrono::Utc::now().timestamp(),
        ))
    }

    /// Requests a remote node's configured region hierarchy over the mesh
    /// — its own firmware `RegionMap`, not this project's local `regions`
    /// config. Unlike [`Self::request_status`]/[`Self::request_telemetry`]/
    /// [`Self::request_neighbours`] (a binary request with a tag-correlated
    /// response), this sends a plain admin CLI text command
    /// (`"region"`) and waits for the next incoming text reply from this
    /// same contact — there's no request/response correlation at the
    /// protocol level for this path. **Requires an admin login**
    /// specifically (see [`Self::login`]) — a guest login means the
    /// firmware never replies at all, indistinguishable client-side from
    /// "no response arrived in time".
    pub async fn request_region_hierarchy(
        &self,
        public_key_prefix_hex: &str,
    ) -> Result<RegionHierarchyDto> {
        const REPLY_TIMEOUT: Duration = Duration::from_secs(15);

        let contact = self.resolve_contact(public_key_prefix_hex).await?;
        self.inner
            .commands()
            .lock()
            .await
            .send_msg(&contact, "region", None)
            .await?;

        let deadline = Instant::now() + REPLY_TIMEOUT;
        let no_reply = || {
            Error::Other(format!(
                "no reply from {public_key_prefix_hex} to the \"region\" command \
                 (wrong login role, or genuinely no response)"
            ))
        };
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(no_reply());
            }
            let event = self
                .inner
                .dispatcher()
                .wait_for_event(Some(EventType::ContactMsgRecv), HashMap::new(), remaining)
                .await
                .ok_or_else(no_reply)?;
            if let EventPayload::ContactMessage(msg) = &event.payload {
                if msg.sender_prefix == contact.prefix() {
                    return Ok(parse_region_hierarchy(
                        &msg.text,
                        chrono::Utc::now().timestamp(),
                    ));
                }
            }
            // Not the reply we're waiting for (e.g. an unrelated incoming
            // chat message from someone else) -- keep waiting against the
            // same deadline.
        }
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

    /// Whether the node currently auto-adds contacts from overheard
    /// adverts (any bit set in its auto-add configuration bitmask).
    pub async fn autoadd_enabled(&self) -> Result<bool> {
        Ok(self
            .inner
            .commands()
            .lock()
            .await
            .get_autoadd_config()
            .await?
            != 0)
    }

    /// Disables the node's own contact auto-add for every contact type.
    pub async fn disable_auto_add_contacts(&self) -> Result<()> {
        self.inner
            .commands()
            .lock()
            .await
            .set_autoadd_config(0, None)
            .await?;
        Ok(())
    }

    /// Fetches a channel slot's current name/secret from the node.
    pub async fn get_channel(
        &self,
        channel_idx: u8,
    ) -> Result<meshcore_rs::events::ChannelInfoData> {
        Ok(self
            .inner
            .commands()
            .lock()
            .await
            .get_channel(channel_idx)
            .await?)
    }

    /// Removes a channel slot from the node (per `docs/companion_protocol.md`'s
    /// "Channel Lifecycle": setting an empty name and all-zero secret deletes
    /// the channel).
    pub async fn remove_channel(&self, channel_idx: u8) -> Result<()> {
        self.inner
            .commands()
            .lock()
            .await
            .set_channel(channel_idx, "", &[0u8; meshcore_rs::CHANNEL_SECRET_LEN])
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
        assert_eq!(node.adv_type, 2);
        assert_eq!(node.lat, 48.85);
        assert_eq!(node.lon, 2.35);
        assert_eq!(node.last_seen_unix, 1_700_000_042);
        assert_eq!(node.last_snr, Some(4.0));
        assert_eq!(node.last_rssi, Some(-90));
        assert_eq!(node.last_hop_count, Some(2));
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
        assert_eq!(node.adv_type, 1);
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
        // Too short to also contain the sender's full 32-byte public key.
        assert!(header.anon_req_sender_public_key_hex.is_none());
    }

    #[test]
    fn build_packet_log_entry_decodes_the_anon_req_sender_public_key() {
        let mut header = advert_header(PayloadType::AnonReq);
        header.payload_version = 0;
        let mut payload = vec![0xde]; // dest hash
        payload.extend_from_slice(&[0xab; 32]); // sender's full public key
        payload.extend_from_slice(&[0x11, 0x22]); // mac
        payload.extend_from_slice(&[0x33, 0x44]); // ciphertext (opaque)
        let log = LogData {
            snr: 2.0,
            rssi: -70,
            header: Some(header),
            advertisement: None,
            payload,
        };
        let entry =
            build_packet_log_entry(&event(EventType::LogData, EventPayload::LogData(log)), 1, 0)
                .unwrap();
        let header = entry.header.expect("header should be decoded");

        assert_eq!(header.dest_hash_hex.as_deref(), Some("de"));
        assert_eq!(
            header.anon_req_sender_public_key_hex.as_deref(),
            Some("ab".repeat(32).as_str())
        );
    }

    // --- decode_control_payload ------------------------------------------

    #[test]
    fn decode_control_payload_decodes_a_discover_req() {
        // Mirrors `MyMesh::sendNodeDiscoverReq()`
        // (examples/simple_repeater/MyMesh.cpp): flags=0x80 (prefix_only=0),
        // type_filter=(1<<ADV_TYPE_REPEATER)=0x04, tag, since=0.
        let payload = [0x80, 0x04, 0x11, 0x22, 0x33, 0x44, 0x00, 0x00, 0x00, 0x00];

        let decoded =
            decode_control_payload(PayloadType::Control, 0, &payload).expect("should decode");

        assert_eq!(
            decoded,
            ControlPayloadInfo::DiscoverReq {
                prefix_only: false,
                type_filter: 0x04,
                tag_hex: "11223344".to_string(),
                since_unix: Some(0),
            }
        );
    }

    #[test]
    fn decode_control_payload_discover_req_prefix_only_flag_and_missing_since() {
        let payload = [0x81, 0x04, 0x11, 0x22, 0x33, 0x44]; // no optional `since`

        let decoded =
            decode_control_payload(PayloadType::Control, 0, &payload).expect("should decode");

        assert_eq!(
            decoded,
            ControlPayloadInfo::DiscoverReq {
                prefix_only: true,
                type_filter: 0x04,
                tag_hex: "11223344".to_string(),
                since_unix: None,
            }
        );
    }

    #[test]
    fn decode_control_payload_decodes_a_discover_resp_with_a_full_public_key() {
        // Mirrors the repeater's reply construction: flags=0x90|ADV_TYPE_REPEATER(2),
        // snr raw byte 20 (-> 20/4 = 5.0), tag echoed, full 32-byte pubkey.
        let mut payload = vec![0x92, 20, 0x11, 0x22, 0x33, 0x44];
        payload.extend_from_slice(&[0xcd; 32]);

        let decoded =
            decode_control_payload(PayloadType::Control, 0, &payload).expect("should decode");

        assert_eq!(
            decoded,
            ControlPayloadInfo::DiscoverResp {
                node_type_name: "Repeater".to_string(),
                snr: 5.0,
                tag_hex: "11223344".to_string(),
                pubkey_hex: "cd".repeat(32),
            }
        );
    }

    #[test]
    fn decode_control_payload_discover_resp_with_a_prefix_only_public_key() {
        let mut payload = vec![
            0x91, 0xEC, /* -20 as i8 -> -5.0 */
            0x11, 0x22, 0x33, 0x44,
        ];
        payload.extend_from_slice(&[0xcd; 8]);

        let decoded =
            decode_control_payload(PayloadType::Control, 0, &payload).expect("should decode");

        assert_eq!(
            decoded,
            ControlPayloadInfo::DiscoverResp {
                node_type_name: "Chat".to_string(),
                snr: -5.0,
                tag_hex: "11223344".to_string(),
                pubkey_hex: "cd".repeat(8),
            }
        );
    }

    #[test]
    fn decode_control_payload_none_for_unrecognized_sub_type_too_short_payload_or_wrong_type() {
        // Unknown sub-type (upper nibble 0x0).
        assert!(decode_control_payload(
            PayloadType::Control,
            0,
            &[0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
        )
        .is_none());
        // Too short even for the common 6-byte prefix.
        assert!(decode_control_payload(PayloadType::Control, 0, &[0x80, 0x00]).is_none());
        // Not a Control payload at all.
        assert!(decode_control_payload(
            PayloadType::TextMsg,
            0,
            &[0x80, 0x00, 0x00, 0x00, 0x00, 0x00]
        )
        .is_none());
        // Non-PAYLOAD_VER_1.
        assert!(decode_control_payload(
            PayloadType::Control,
            1,
            &[0x80, 0x00, 0x00, 0x00, 0x00, 0x00]
        )
        .is_none());
    }

    #[test]
    fn build_packet_log_entry_decodes_control_discover_req() {
        let mut header = advert_header(PayloadType::Control);
        header.payload_version = 0;
        let log = LogData {
            snr: 2.0,
            rssi: -70,
            header: Some(header),
            advertisement: None,
            payload: vec![0x80, 0x04, 0x11, 0x22, 0x33, 0x44, 0x00, 0x00, 0x00, 0x00],
        };
        let entry =
            build_packet_log_entry(&event(EventType::LogData, EventPayload::LogData(log)), 1, 0)
                .unwrap();

        assert_eq!(
            entry.header.expect("header should be decoded").control,
            Some(ControlPayloadInfo::DiscoverReq {
                prefix_only: false,
                type_filter: 0x04,
                tag_hex: "11223344".to_string(),
                since_unix: Some(0),
            })
        );
    }

    #[test]
    fn build_packet_log_entry_no_anon_req_sender_public_key_for_other_payload_types() {
        let mut header = advert_header(PayloadType::TextMsg);
        header.payload_version = 0;
        let mut payload = vec![0xde, 0xad]; // dest + src hash
        payload.extend_from_slice(&[0xab; 32]);
        let log = LogData {
            snr: 2.0,
            rssi: -70,
            header: Some(header),
            advertisement: None,
            payload,
        };
        let entry =
            build_packet_log_entry(&event(EventType::LogData, EventPayload::LogData(log)), 1, 0)
                .unwrap();

        assert!(entry
            .header
            .expect("header should be decoded")
            .anon_req_sender_public_key_hex
            .is_none());
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
        assert_eq!(dto.radio_bw_khz, 250.0);
        assert_eq!(dto.spreading_factor, 10);
        assert_eq!(dto.coding_rate, 5);
        assert_eq!(dto.tx_power_dbm, 22);
        assert_eq!(dto.lat, 48.85);
        assert_eq!(dto.lon, 2.35);
    }

    #[test]
    fn status_dto_from_converts_airtime_from_milliseconds_and_keeps_fetched_at() {
        let status = StatusData {
            battery_mv: 4012,
            tx_queue_len: 2,
            noise_floor: -110,
            last_rssi: -80,
            nb_recv: 100,
            nb_sent: 50,
            airtime: 12_500,
            uptime: 3600,
            flood_sent: 10,
            direct_sent: 5,
            snr: 6.5,
            dup_count: 3,
            rx_airtime: 8_000,
            sender_prefix: [0xaa; 6],
        };
        let dto = status_dto_from(status, 1_000);

        assert_eq!(dto.fetched_at_unix, 1_000);
        assert_eq!(dto.battery_mv, 4012);
        assert_eq!(dto.tx_queue_len, 2);
        assert_eq!(dto.noise_floor, -110);
        assert_eq!(dto.last_rssi, -80);
        assert_eq!(dto.last_snr, 6.5);
        assert_eq!(dto.packets_received, 100);
        assert_eq!(dto.packets_sent, 50);
        assert_eq!(dto.duplicate_packets, 3);
        assert_eq!(dto.airtime_secs, 12); // 12_500ms -> 12s (truncated)
        assert_eq!(dto.rx_airtime_secs, 8);
        assert_eq!(dto.uptime_secs, 3600);
        assert_eq!(dto.flood_sent, 10);
        assert_eq!(dto.direct_sent, 5);
    }

    #[test]
    fn neighbours_dto_from_hex_encodes_each_pubkey_prefix() {
        let data = NeighboursData {
            total: 5,
            neighbours: vec![
                Neighbour {
                    pubkey: vec![0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
                    secs_ago: 42,
                    snr: 7.25,
                },
                Neighbour {
                    pubkey: vec![0x11, 0x22, 0x33],
                    secs_ago: 100,
                    snr: -2.0,
                },
            ],
        };
        let dto = neighbours_dto_from(data, 2_000);

        assert_eq!(dto.fetched_at_unix, 2_000);
        assert_eq!(dto.total, 5);
        assert_eq!(dto.neighbours.len(), 2);
        assert_eq!(dto.neighbours[0].public_key_prefix_hex, "aabbccddeeff");
        assert_eq!(dto.neighbours[0].secs_ago, 42);
        assert_eq!(dto.neighbours[0].snr, 7.25);
        assert_eq!(dto.neighbours[1].public_key_prefix_hex, "112233");
    }

    // --- parse_region_hierarchy ---------------------------------------------

    /// The exact example from `RegionMap::printChildRegions`'s format
    /// (firmware `src/helpers/RegionMap.cpp:287-308`): nested by leading
    /// spaces, home region marked `^`, flood-allowed marked trailing ` F`.
    #[test]
    fn parse_region_hierarchy_nested_home_and_flood_markers() {
        let raw = "World F\n Europe F\n  France^ F\n";
        let dto = parse_region_hierarchy(raw, 1_000);

        assert_eq!(dto.fetched_at_unix, 1_000);
        assert_eq!(dto.raw_text, raw);
        assert_eq!(dto.entries.len(), 3);

        assert_eq!(dto.entries[0].name, "World");
        assert_eq!(dto.entries[0].depth, 0);
        assert!(!dto.entries[0].is_home);
        assert!(dto.entries[0].flood_allowed);

        assert_eq!(dto.entries[1].name, "Europe");
        assert_eq!(dto.entries[1].depth, 1);
        assert!(!dto.entries[1].is_home);
        assert!(dto.entries[1].flood_allowed);

        assert_eq!(dto.entries[2].name, "France");
        assert_eq!(dto.entries[2].depth, 2);
        assert!(dto.entries[2].is_home);
        assert!(dto.entries[2].flood_allowed);
    }

    #[test]
    fn parse_region_hierarchy_flood_denied_region_has_no_trailing_marker() {
        // `printChildRegions` omits the " F" suffix entirely when flood is
        // denied for that region (`RegionMap.cpp:292-293`).
        let dto = parse_region_hierarchy("World\n Europe F\n", 0);

        assert_eq!(dto.entries[0].name, "World");
        assert!(!dto.entries[0].flood_allowed);
        assert_eq!(dto.entries[1].name, "Europe");
        assert!(dto.entries[1].flood_allowed);
    }

    #[test]
    fn parse_region_hierarchy_skips_blank_lines() {
        let dto = parse_region_hierarchy("World F\n\n Europe F\n", 0);
        assert_eq!(dto.entries.len(), 2);
    }

    #[test]
    fn parse_region_hierarchy_empty_reply_is_empty() {
        let dto = parse_region_hierarchy("", 0);
        assert!(dto.entries.is_empty());
        assert_eq!(dto.raw_text, "");
    }

    /// The firmware's reply buffer is a fixed ~160 characters -- a
    /// truncated line (cut off mid-name, no markers) should still parse
    /// as a best-effort entry rather than erroring the whole reply.
    #[test]
    fn parse_region_hierarchy_truncated_trailing_line_is_best_effort() {
        let dto = parse_region_hierarchy("World F\n  Fran", 0);
        assert_eq!(dto.entries.len(), 2);
        assert_eq!(dto.entries[1].name, "Fran");
        assert_eq!(dto.entries[1].depth, 2);
        assert!(!dto.entries[1].flood_allowed);
    }

    // --- RepeaterDetailDto::apply_category ----------------------------------

    #[test]
    fn apply_category_status_ok_fills_in_the_field() {
        let mut detail = RepeaterDetailDto::default();
        detail.apply_category(RepeaterDetailCategory::Status(Ok(StatusDto {
            fetched_at_unix: 1,
            battery_mv: 4012,
            tx_queue_len: 0,
            noise_floor: -110,
            last_rssi: -80,
            last_snr: 6.5,
            packets_received: 1,
            packets_sent: 1,
            duplicate_packets: 0,
            airtime_secs: 1,
            rx_airtime_secs: 1,
            uptime_secs: 1,
            flood_sent: 0,
            direct_sent: 0,
        })));

        assert!(detail.status.is_some());
        assert!(detail.errors.is_empty());
    }

    #[test]
    fn apply_category_err_records_a_prefixed_error_and_leaves_the_field_none() {
        let mut detail = RepeaterDetailDto::default();
        detail.apply_category(RepeaterDetailCategory::Neighbours(Err(
            "timed out".to_string()
        )));

        assert!(detail.neighbours.is_none());
        assert_eq!(detail.errors, vec!["neighbours: timed out".to_string()]);
    }

    #[test]
    fn apply_category_folds_updates_from_every_category_independently() {
        let mut detail = RepeaterDetailDto::default();
        detail.apply_category(RepeaterDetailCategory::Status(Err("a".to_string())));
        detail.apply_category(RepeaterDetailCategory::Telemetry(Ok(TelemetryDto {
            fetched_at_unix: 0,
            readings: vec![],
        })));
        detail.apply_category(RepeaterDetailCategory::Neighbours(Err("b".to_string())));
        detail.apply_category(RepeaterDetailCategory::Regions(Ok(RegionHierarchyDto {
            fetched_at_unix: 0,
            entries: vec![],
            raw_text: String::new(),
        })));

        assert!(detail.status.is_none());
        assert!(detail.telemetry.is_some());
        assert!(detail.neighbours.is_none());
        assert!(detail.regions.is_some());
        assert_eq!(
            detail.errors,
            vec!["status: a".to_string(), "neighbours: b".to_string()]
        );
    }

    #[test]
    fn node_stats_dto_serializes_flat_when_all_categories_present() {
        let dto = NodeStatsDto {
            core: Some(CoreStatsDto {
                battery_mv: 4012,
                uptime_secs: 123456,
                errors: 0,
                queue_len: 0,
            }),
            radio: Some(RadioStatsDto {
                noise_floor: -120,
                last_rssi: -80,
                last_snr: 8.25,
                tx_air_secs: 120,
                rx_air_secs: 340,
            }),
            packets: Some(PacketStatsDto {
                recv: 1000,
                sent: 500,
                flood_tx: 100,
                direct_tx: 400,
                flood_rx: 200,
                direct_rx: 800,
                recv_errors: None,
            }),
        };

        let json = serde_json::to_value(&dto).unwrap();
        // Flat, not nested under "core"/"radio"/"packets".
        assert_eq!(json["battery_mv"], 4012);
        assert_eq!(json["noise_floor"], -120);
        assert_eq!(json["recv"], 1000);
        // recv_errors stays present (explicit null), not omitted -- matches
        // meshcore_py's legacy-frame behavior.
        assert!(json.get("recv_errors").is_some());
        assert!(json["recv_errors"].is_null());
    }

    #[test]
    fn node_stats_dto_omits_missing_categories_entirely() {
        let dto = NodeStatsDto {
            core: Some(CoreStatsDto {
                battery_mv: 4012,
                uptime_secs: 1,
                errors: 0,
                queue_len: 0,
            }),
            radio: None,
            packets: None,
        };

        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["battery_mv"], 4012);
        assert!(json.get("noise_floor").is_none());
        assert!(json.get("recv").is_none());
    }

    #[test]
    fn node_stats_dto_default_serializes_to_empty_object() {
        let json = serde_json::to_value(NodeStatsDto::default()).unwrap();
        assert_eq!(json, serde_json::json!({}));
    }

    #[test]
    fn device_info_dto_formats_firmware_version_and_strips_v_prefix() {
        let info = meshcore_rs::events::DeviceInfoData {
            fw_version_code: 3,
            max_contacts: None,
            max_channels: None,
            ble_pin: None,
            fw_build: Some("06-Jun-2026".to_string()),
            model: Some("Seeed Xiao-nrf52".to_string()),
            version: Some("v1.16.0-07a3ca9".to_string()),
            repeat: None,
        };
        let dto = DeviceInfoDto::from(&info);

        assert_eq!(dto.model, "Seeed Xiao-nrf52");
        assert_eq!(dto.firmware_version, "v1.16.0-07a3ca9 (Build: 06-Jun-2026)");
    }

    #[test]
    fn device_info_dto_falls_back_to_unknown_when_fields_missing() {
        let info = meshcore_rs::events::DeviceInfoData {
            fw_version_code: 1,
            max_contacts: None,
            max_channels: None,
            ble_pin: None,
            fw_build: None,
            model: None,
            version: None,
            repeat: None,
        };
        let dto = DeviceInfoDto::from(&info);

        assert_eq!(dto.model, "unknown");
        assert_eq!(dto.firmware_version, "unknown");
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
        assert_eq!(dto.contact_type, 2);
    }

    #[test]
    fn is_repeater_or_room_accepts_only_types_two_and_three() {
        assert!(!is_repeater_or_room(1)); // Chat
        assert!(is_repeater_or_room(2)); // Repeater
        assert!(is_repeater_or_room(3)); // Room
        assert!(!is_repeater_or_room(4)); // Sensor
        assert!(!is_repeater_or_room(0));
    }

    // --- contacts_to_prune ---------------------------------------------------

    fn sample_contact(prefix_hex: &str) -> ContactDto {
        ContactDto {
            name: format!("Node {prefix_hex}"),
            public_key_prefix_hex: prefix_hex.to_string(),
            last_advert_unix: 0,
            lat: 0.0,
            lon: 0.0,
            registered: true,
            managed: false,
            repeater_status: None,
            contact_type: 2,
            last_telemetry: None,
        }
    }

    #[test]
    fn contacts_to_prune_keeps_managed_contacts() {
        let contacts = vec![sample_contact("aabbcc")];
        let managed = vec![ManagedRepeater {
            name: "Repeater A".to_string(),
            public_key_hex: "aabbcc".repeat(10) + "aaaa",
            password: None,
            status: RepeaterStatus::Managed,
        }];

        assert!(contacts_to_prune(&contacts, &managed).is_empty());
    }

    #[test]
    fn contacts_to_prune_prunes_non_managed_contacts() {
        let contacts = vec![sample_contact("aabbcc"), sample_contact("ddeeff")];
        let managed = vec![ManagedRepeater {
            name: "Repeater A".to_string(),
            public_key_hex: "aabbcc".repeat(10) + "aaaa",
            password: None,
            status: RepeaterStatus::Managed,
        }];

        let pruned = contacts_to_prune(&contacts, &managed);
        assert_eq!(pruned.len(), 1);
        assert_eq!(pruned[0].public_key_prefix_hex, "ddeeff");
    }

    #[test]
    fn contacts_to_prune_prunes_everything_when_nothing_is_managed() {
        let contacts = vec![sample_contact("aabbcc"), sample_contact("ddeeff")];
        assert_eq!(contacts_to_prune(&contacts, &[]).len(), 2);
    }

    // --- is_registered_contact -----------------------------------------------

    #[test]
    fn is_registered_contact_true_when_prefix_matches_case_insensitively() {
        let contacts = vec![sample_contact("aabbcc")];
        assert!(is_registered_contact(&contacts, "AABBCC"));
    }

    #[test]
    fn is_registered_contact_false_when_no_contact_matches() {
        let contacts = vec![sample_contact("aabbcc")];
        assert!(!is_registered_contact(&contacts, "ddeeff"));
    }

    #[test]
    fn is_registered_contact_false_for_an_empty_contact_list() {
        assert!(!is_registered_contact(&[], "aabbcc"));
    }

    // --- matching_repeater_status --------------------------------------------

    fn managed_repeater(prefix_hex: &str, status: RepeaterStatus) -> ManagedRepeater {
        ManagedRepeater {
            name: format!("Repeater {prefix_hex}"),
            public_key_hex: prefix_hex.repeat(10) + "aaaa",
            password: None,
            status,
        }
    }

    #[test]
    fn matching_repeater_status_finds_each_status() {
        for status in [
            RepeaterStatus::Managed,
            RepeaterStatus::Known,
            RepeaterStatus::Supervised,
        ] {
            let repeaters = vec![managed_repeater("aabbcc", status)];
            assert_eq!(matching_repeater_status(&repeaters, "aabbcc"), Some(status));
        }
    }

    #[test]
    fn matching_repeater_status_none_when_no_match() {
        let repeaters = vec![managed_repeater("aabbcc", RepeaterStatus::Managed)];
        assert_eq!(matching_repeater_status(&repeaters, "ddeeff"), None);
    }

    #[test]
    fn matching_repeater_status_none_for_an_empty_list() {
        assert_eq!(matching_repeater_status(&[], "aabbcc"), None);
    }

    // --- reconstruct_raw_packet_hex ----------------------------------------

    #[test]
    fn reconstruct_raw_packet_hex_round_trips_through_the_real_parser() {
        // header_byte: version=1 (bits 6-7) | TextMsg=2 (bits 2-5) | Direct=2 (bits 0-1)
        //            = 0b01_0010_10 = 0x4A
        let mut raw = vec![0x4Au8];
        // path_byte: path_hash_size=1 -> (1-1)<<6=0, path_len=2 -> 0x02
        raw.push(0x02);
        raw.extend_from_slice(&[0x11, 0x22]); // path (path_len=2, path_hash_size=1)
        raw.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]); // inner payload

        let (header, remaining) =
            meshcore_rs::parsing::parse_mesh_packet_header(&raw).expect("decodable header");
        let log = LogData {
            snr: 1.0,
            rssi: -80,
            header: Some(header),
            advertisement: None,
            payload: remaining.to_vec(),
        };
        let entry =
            build_packet_log_entry(&event(EventType::LogData, EventPayload::LogData(log)), 1, 0)
                .unwrap();

        let reconstructed = reconstruct_raw_packet_hex(&entry).expect("should reconstruct");
        assert_eq!(reconstructed, hex_encode(&raw));
    }

    #[test]
    fn reconstruct_raw_packet_hex_round_trips_with_a_transport_code() {
        // header_byte: version=0 | Advert=4 (bits 2-5) | TransportFlood=0 (bits 0-1)
        //            = 0b00_0100_00 = 0x10
        let mut raw = vec![0x10u8];
        raw.extend_from_slice(&[0xaa, 0xbb, 0xcc, 0xdd]); // transport code
        raw.push(0x41); // path_hash_size=2 ((2-1)<<6=0x40), path_len=1
        raw.extend_from_slice(&[0x33, 0x44]); // single 2-byte path hop hash
        raw.extend_from_slice(&[0x01, 0x02]); // inner payload

        let (header, remaining) =
            meshcore_rs::parsing::parse_mesh_packet_header(&raw).expect("decodable header");
        let log = LogData {
            snr: 1.0,
            rssi: -80,
            header: Some(header),
            advertisement: None,
            payload: remaining.to_vec(),
        };
        let entry =
            build_packet_log_entry(&event(EventType::LogData, EventPayload::LogData(log)), 1, 0)
                .unwrap();

        let reconstructed = reconstruct_raw_packet_hex(&entry).expect("should reconstruct");
        assert_eq!(reconstructed, hex_encode(&raw));
    }

    #[test]
    fn reconstruct_raw_packet_hex_none_when_header_is_none() {
        let log = LogData {
            snr: 1.0,
            rssi: -80,
            header: None,
            advertisement: None,
            payload: vec![],
        };
        let entry =
            build_packet_log_entry(&event(EventType::LogData, EventPayload::LogData(log)), 1, 0)
                .unwrap();

        assert!(reconstruct_raw_packet_hex(&entry).is_none());
    }
}
