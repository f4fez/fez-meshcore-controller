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

use fez_mesh_controller_core::ipc::{MeshEvent, Snapshot};
use fez_mesh_controller_core::mesh::{ContactDto, PacketLogEntry};

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
    /// Whether the packet detail popup is open for the currently selected
    /// packet.
    pub packet_detail_open: bool,
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
            packet_detail_open: false,
        }
    }

    /// Contacts sorted the way they're displayed (most recently seen
    /// first), so table row indices line up with selection/action logic.
    pub fn sorted_contacts(&self) -> Vec<&ContactDto> {
        let mut contacts: Vec<&ContactDto> = self.snapshot.contacts.iter().collect();
        contacts.sort_by(|a, b| b.last_advert_unix.cmp(&a.last_advert_unix));
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
        let len = self.snapshot.contacts.len();
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
        let len = self.snapshot.contacts.len();
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

    /// The currently-selected packet in the visible (possibly frozen) list.
    pub fn selected_packet(&self) -> Option<&PacketLogEntry> {
        let index = self.packet_table_state.selected()?;
        self.visible_packets().get(index)
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
        let len = self.visible_packets().len();
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
        let len = self.visible_packets().len();
        if len == 0 {
            return;
        }
        let prev = match self.packet_table_state.selected() {
            Some(0) | None => len - 1,
            Some(i) => i - 1,
        };
        self.packet_table_state.select(Some(prev));
    }
}
