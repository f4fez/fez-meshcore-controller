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

use std::collections::VecDeque;

use fez_mesh_controller_core::channel;
use fez_mesh_controller_core::ipc::{MeshEvent, Snapshot};
use fez_mesh_controller_core::mesh::{
    is_repeater_or_room, ContactDto, PacketLogEntry, RepeaterDetailCategory, RepeaterDetailDto,
};
use fez_mesh_controller_core::region;
use ratatui::widgets::ListState;

use super::packet_group::{group_packets, PacketGroup};

/// The repeater detail popup's content — see [`App::repeater_detail`].
pub struct RepeaterDetailView {
    pub public_key_prefix_hex: String,
    pub name: String,
    /// Starts as `RepeaterDetailDto::default()` (every category `None`,
    /// pending) and is filled in progressively, one category at a time, by
    /// [`App::apply_repeater_detail_category`] as each
    /// `ServerMessage::RepeaterDetailCategory` arrives — rather than
    /// waiting for all four before showing anything.
    pub detail: RepeaterDetailDto,
}

pub const MAX_EVENTS: usize = 200;
/// Client-side cap on the packet log buffer, kept generous compared to the
/// daemon's default rotating cache (500) in case it's configured larger.
pub const MAX_PACKETS: usize = 2000;

/// The TUI layout currently shown, switched with F2 / F3 / ...
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    /// The main dashboard: local node info, contacts, event log.
    Dashboard,
    /// Raw packet log, fed by the meshcore-rs `LogData` RF monitor.
    PacketLog,
}

pub struct App {
    pub snapshot: Snapshot,
    /// Most recent events first.
    pub events: VecDeque<MeshEvent>,
    pub daemon_connected: bool,
    pub should_quit: bool,
    pub last_status: Option<String>,
    pub contacts_state: ratatui::widgets::TableState,
    /// Set when the user has pressed the delete key once for the given
    /// contact (public key prefix, name); a second press confirms. Cleared
    /// by any other key press.
    pub pending_delete: Option<(String, String)>,

    pub page: Page,

    /// Raw packet log, most recent first, mirrored from the daemon's own
    /// rotating cache (initial backlog + live pushes).
    pub packets: Vec<PacketLogEntry>,
    pub packet_table_state: ratatui::widgets::TableState,
    /// When set, the packet table view is frozen at this snapshot so the
    /// user can scroll through it undisturbed while `packets` keeps
    /// growing in the background ("scroll lock").
    pub locked_view: Option<Vec<PacketLogEntry>>,
    /// The packet detail popup's content, snapshotted at the moment it was
    /// opened so it stays stable even as new packets keep arriving in the
    /// background. `None` when the popup is closed.
    pub packet_detail: Option<PacketGroup>,

    /// Whether the F1 help popup is open, showing shortcuts for the
    /// current page.
    pub help_open: bool,

    /// The repeater detail popup's content: contact/configuration info is
    /// always available locally, so the popup opens immediately on an Enter
    /// key press (`detail` starting fully pending); each category then
    /// fills in independently as its
    /// `ServerMessage::RepeaterDetailCategory` arrives for the matching
    /// prefix. `None` (the outer `Option`) when the popup is closed.
    pub repeater_detail: Option<RepeaterDetailView>,
    /// Scroll/selection state for the repeater detail popup's neighbours
    /// list (the right-hand column) — reset whenever the popup is (re)opened.
    pub neighbours_list_state: ListState,

    /// Each configured region's precomputed transport key (name, key),
    /// derived once whenever a fresh `Snapshot` arrives (see
    /// [`Self::apply_snapshot`]) rather than per packet — see
    /// [`fez_mesh_controller_core::region::precompute_region_keys`].
    pub region_keys: Vec<(String, [u8; 16])>,
    /// Every candidate `GroupText` channel's precomputed secret and hash
    /// (name, secret, hash) — the well-known "Public" channel plus each
    /// configured hashtag channel — derived once whenever a fresh
    /// `Snapshot` arrives, not per packet — see
    /// [`fez_mesh_controller_core::channel::precompute_channel_keys`].
    pub channel_keys: Vec<(String, [u8; 32], u8)>,
}

impl App {
    pub fn new() -> Self {
        Self {
            snapshot: Snapshot::default(),
            events: VecDeque::with_capacity(MAX_EVENTS),
            daemon_connected: false,
            should_quit: false,
            last_status: None,
            contacts_state: ratatui::widgets::TableState::default(),
            pending_delete: None,
            page: Page::Dashboard,
            packets: Vec::new(),
            packet_table_state: ratatui::widgets::TableState::default(),
            locked_view: None,
            packet_detail: None,
            help_open: false,
            repeater_detail: None,
            neighbours_list_state: ListState::default(),
            region_keys: Vec::new(),
            channel_keys: channel::precompute_channel_keys(&[]),
        }
    }

    /// Applies a fresh `Snapshot` from the daemon, precomputing the
    /// configured regions' transport keys and candidate channels' secrets
    /// once (not per packet — see [`Self::region_keys`]/[`Self::channel_keys`]).
    pub fn apply_snapshot(&mut self, snapshot: Snapshot) {
        self.region_keys = region::precompute_region_keys(&snapshot.regions);
        self.channel_keys = channel::precompute_channel_keys(&snapshot.hashtag_channels);
        self.snapshot = snapshot;
    }

    /// Repeaters and room servers (the "Repeaters" panel's scope — plain
    /// chat clients and sensors are excluded), sorted the way they're
    /// displayed (most recently seen first), so table row indices line up
    /// with selection/action logic.
    pub fn sorted_contacts(&self) -> Vec<&ContactDto> {
        let mut contacts: Vec<&ContactDto> = self
            .snapshot
            .contacts
            .iter()
            .filter(|c| is_repeater_or_room(c.contact_type))
            .collect();
        contacts.sort_by_key(|c| std::cmp::Reverse(c.last_advert_unix));
        contacts
    }

    /// The currently-selected contact in the (sorted) table, if any.
    pub fn selected_contact(&self) -> Option<&ContactDto> {
        let index = self.contacts_state.selected()?;
        self.sorted_contacts().get(index).copied()
    }

    pub fn push_event(&mut self, event: MeshEvent) {
        if self.events.len() >= MAX_EVENTS {
            self.events.pop_back();
        }
        self.events.push_front(event);
    }

    pub fn select_next_contact(&mut self) {
        let len = self.sorted_contacts().len();
        if len == 0 {
            return;
        }
        let next = match self.contacts_state.selected() {
            Some(i) if i + 1 < len => i + 1,
            Some(_) => 0,
            None => 0,
        };
        self.contacts_state.select(Some(next));
    }

    pub fn select_prev_contact(&mut self) {
        let len = self.sorted_contacts().len();
        if len == 0 {
            return;
        }
        let prev = match self.contacts_state.selected() {
            Some(0) | None => len - 1,
            Some(i) => i - 1,
        };
        self.contacts_state.select(Some(prev));
    }

    /// The packet list currently shown: the frozen snapshot while scroll
    /// lock is engaged, otherwise the live buffer.
    pub fn visible_packets(&self) -> &[PacketLogEntry] {
        match &self.locked_view {
            Some(frozen) => frozen,
            None => &self.packets,
        }
    }

    /// Packets in the visible (possibly frozen) list, grouped by
    /// transmission: the same packet relayed and independently overheard
    /// via several repeaters collapses into a single [`PacketGroup`].
    pub fn packet_groups(&self) -> Vec<PacketGroup> {
        group_packets(self.visible_packets())
    }

    /// The currently-selected packet group in the (grouped) table, if any.
    pub fn selected_group(&self) -> Option<PacketGroup> {
        let index = self.packet_table_state.selected()?;
        self.packet_groups().into_iter().nth(index)
    }

    /// Replaces the packet log with the daemon's initial backlog (already
    /// newest-first).
    pub fn set_packet_log(&mut self, backlog: Vec<PacketLogEntry>) {
        self.packets = backlog;
        self.packets.truncate(MAX_PACKETS);
    }

    /// Records a newly-pushed packet at the front of the live buffer. The
    /// frozen view (if scroll-locked) is left untouched.
    pub fn push_packet(&mut self, entry: PacketLogEntry) {
        self.packets.insert(0, entry);
        self.packets.truncate(MAX_PACKETS);
    }

    /// Toggles scroll lock: freezes (or releases) the packet table view.
    pub fn toggle_scroll_lock(&mut self) {
        if self.locked_view.is_some() {
            self.locked_view = None;
        } else {
            self.locked_view = Some(self.packets.clone());
        }
    }

    /// Number of packets captured since scroll lock was engaged (0 when
    /// not locked).
    pub fn new_packets_since_lock(&self) -> usize {
        let Some(frozen) = &self.locked_view else {
            return 0;
        };
        let Some(newest_locked_id) = frozen.first().map(|p| p.id) else {
            return self.packets.len();
        };
        self.packets
            .iter()
            .take_while(|p| p.id > newest_locked_id)
            .count()
    }

    pub fn select_next_packet(&mut self) {
        let len = self.packet_groups().len();
        if len == 0 {
            return;
        }
        let next = match self.packet_table_state.selected() {
            Some(i) if i + 1 < len => i + 1,
            Some(_) => 0,
            None => 0,
        };
        self.packet_table_state.select(Some(next));
    }

    pub fn select_prev_packet(&mut self) {
        let len = self.packet_groups().len();
        if len == 0 {
            return;
        }
        let prev = match self.packet_table_state.selected() {
            Some(0) | None => len - 1,
            Some(i) => i - 1,
        };
        self.packet_table_state.select(Some(prev));
    }

    /// Opens the detail popup for the currently selected packet,
    /// snapshotting its content so it stays stable even as new packets
    /// keep arriving in the background.
    pub fn open_packet_detail(&mut self) {
        self.packet_detail = self.selected_group();
    }

    pub fn close_packet_detail(&mut self) {
        self.packet_detail = None;
    }

    /// Opens the repeater detail popup for an Enter-key request,
    /// immediately (before any category's fetch has resolved) so the popup
    /// can show local Contact/Configuration info right away, with every
    /// fetched section pending.
    pub fn open_repeater_detail_pending(&mut self, public_key_prefix_hex: String, name: String) {
        self.repeater_detail = Some(RepeaterDetailView {
            public_key_prefix_hex,
            name,
            detail: RepeaterDetailDto::default(),
        });
        self.neighbours_list_state = ListState::default();
    }

    /// Folds one streamed category update into the popup's accumulated
    /// data, but only if it's still showing the contact this update is for
    /// — discards a stale update for a request the user has since replaced
    /// (opened the popup on a different contact) or closed.
    pub fn apply_repeater_detail_category(
        &mut self,
        public_key_prefix_hex: String,
        category: RepeaterDetailCategory,
    ) {
        let Some(view) = &mut self.repeater_detail else {
            return;
        };
        if view.public_key_prefix_hex != public_key_prefix_hex {
            return;
        }
        view.detail.apply_category(category);
        let has_neighbours = view
            .detail
            .neighbours
            .as_ref()
            .map(|n| !n.neighbours.is_empty())
            .unwrap_or(false);
        if has_neighbours {
            self.neighbours_list_state.select(Some(0));
        }
    }

    pub fn close_repeater_detail(&mut self) {
        self.repeater_detail = None;
    }

    /// Number of neighbours currently shown in the repeater detail popup's
    /// scrollable list, if the fetch has resolved with any.
    fn neighbour_count(&self) -> usize {
        self.repeater_detail
            .as_ref()
            .and_then(|v| v.detail.neighbours.as_ref())
            .map(|n| n.neighbours.len())
            .unwrap_or(0)
    }

    /// Scrolls the repeater detail popup's neighbours list down one row.
    pub fn select_next_neighbour(&mut self) {
        let len = self.neighbour_count();
        if len == 0 {
            return;
        }
        let next = match self.neighbours_list_state.selected() {
            Some(i) if i + 1 < len => i + 1,
            Some(_) => 0,
            None => 0,
        };
        self.neighbours_list_state.select(Some(next));
    }

    /// Scrolls the repeater detail popup's neighbours list up one row.
    pub fn select_prev_neighbour(&mut self) {
        let len = self.neighbour_count();
        if len == 0 {
            return;
        }
        let prev = match self.neighbours_list_state.selected() {
            Some(0) | None => len - 1,
            Some(i) => i - 1,
        };
        self.neighbours_list_state.select(Some(prev));
    }
}

/// Estimated interval (seconds) between this contact's two most recent
/// advertisements, from `packets` (newest-first) — not a protocol field
/// (no node reports its own advert interval), just the delta between the
/// two most recent `Advertisement`-payload packets attributable to this
/// contact. `None` if fewer than two are cached.
pub fn contact_advert_interval_secs(
    packets: &[PacketLogEntry],
    contact: &ContactDto,
) -> Option<i64> {
    let prefix = contact.public_key_prefix_hex.to_ascii_lowercase();
    let mut adverts = packets.iter().filter_map(|entry| {
        let adv = entry.header.as_ref()?.advertisement.as_ref()?;
        adv.public_key_hex
            .to_ascii_lowercase()
            .starts_with(&prefix)
            .then_some(entry.at_unix)
    });
    let newest = adverts.next()?;
    let second = adverts.next()?;
    Some(newest - second)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fez_mesh_controller_core::mesh::{PacketAdvertInfo, PacketHeaderInfo};

    fn contact(public_key_prefix_hex: &str) -> ContactDto {
        ContactDto {
            name: "Node".to_string(),
            public_key_prefix_hex: public_key_prefix_hex.to_string(),
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

    fn header() -> PacketHeaderInfo {
        PacketHeaderInfo {
            route_type: "TransportFlood".to_string(),
            route_type_raw: 0,
            payload_type: "TextMsg".to_string(),
            payload_type_raw: 2,
            payload_version: 0,
            hops: 1,
            path_hash_size: 1,
            path_hex: String::new(),
            transport_code_hex: None,
            dest_hash_hex: None,
            src_hash_hex: None,
            channel_hash_hex: None,
            anon_req_sender_public_key_hex: None,
            control: None,
            advertisement: None,
        }
    }

    fn entry(id: u64, at_unix: i64, header: Option<PacketHeaderInfo>) -> PacketLogEntry {
        PacketLogEntry {
            id,
            at_unix,
            snr: 1.0,
            rssi: -90,
            header,
            payload_hex: "deadbeef".to_string(),
            payload_len: 4,
        }
    }

    // --- contact_advert_interval_secs --------------------------------------

    fn advert_entry(id: u64, at_unix: i64, public_key_hex: &str) -> PacketLogEntry {
        let mut h = header();
        h.payload_type = "Advert".to_string();
        h.advertisement = Some(PacketAdvertInfo {
            public_key_hex: public_key_hex.to_string(),
            name: Some("Node".to_string()),
            adv_type_name: "Repeater".to_string(),
            lat: None,
            lon: None,
        });
        entry(id, at_unix, Some(h))
    }

    #[test]
    fn contact_advert_interval_secs_none_with_fewer_than_two_adverts() {
        let c = contact("aabbccddeeff");
        let packets = vec![advert_entry(1, 100, "aabbccddeeff")];
        assert_eq!(contact_advert_interval_secs(&packets, &c), None);
    }

    #[test]
    fn contact_advert_interval_secs_computes_delta_between_two_most_recent() {
        let c = contact("aabbccddeeff");
        // Newest-first, as stored by the app.
        let packets = vec![
            advert_entry(3, 300, "aabbccddeeff"),
            advert_entry(2, 200, "aabbccddeeff"),
            advert_entry(1, 100, "aabbccddeeff"),
        ];
        assert_eq!(contact_advert_interval_secs(&packets, &c), Some(100));
    }

    #[test]
    fn contact_advert_interval_secs_ignores_other_contacts_adverts() {
        let c = contact("aabbccddeeff");
        let packets = vec![
            advert_entry(2, 200, "ffeeddccbbaa"),
            advert_entry(1, 100, "ffeeddccbbaa"),
        ];
        assert_eq!(contact_advert_interval_secs(&packets, &c), None);
    }
}
