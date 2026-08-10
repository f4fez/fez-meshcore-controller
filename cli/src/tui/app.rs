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

pub const MAX_EVENTS: usize = 200;

pub struct App {
    pub snapshot: Snapshot,
    /// Most recent events first.
    pub events: VecDeque<MeshEvent>,
    pub daemon_connected: bool,
    pub should_quit: bool,
    pub last_status: Option<String>,
    pub contacts_state: ratatui::widgets::TableState,
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
        }
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
}
