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

//! Groups repeated raw-packet log entries that are really the same
//! over-the-air transmission, relayed and independently overheard via
//! several repeaters. Flood routing means a single packet from a sender
//! shows up once per repeater that forwarded it; without grouping, the
//! packet log page is dominated by near-duplicate rows.

use std::collections::HashMap;

use fez_mesh_controller_core::mesh::PacketLogEntry;

/// Receptions of the same packet more than this many seconds apart are
/// treated as unrelated (e.g. a coincidentally identical payload much
/// later) rather than merged into one group.
const GROUP_WINDOW_SECS: i64 = 90;

/// One or more raw-packet log entries believed to be the same
/// transmission, heard directly and/or relayed by different repeaters.
/// `members` is newest-first, matching [`crate::tui::app::App::packets`].
#[derive(Debug, Clone)]
pub struct PacketGroup {
    pub members: Vec<PacketLogEntry>,
}

impl PacketGroup {
    /// The most recently received member — used as the representative
    /// entry for fields common to the whole group (payload type, content).
    pub fn latest(&self) -> &PacketLogEntry {
        &self.members[0]
    }

    pub fn count(&self) -> usize {
        self.members.len()
    }
}

/// Groups a newest-first packet list into [`PacketGroup`]s, preserving
/// newest-first order (a group is placed according to its most recent
/// member). Entries whose header couldn't be decoded are never grouped,
/// since payload type isn't known and payload bytes alone aren't a
/// reliable enough signal.
pub fn group_packets(packets: &[PacketLogEntry]) -> Vec<PacketGroup> {
    struct OpenGroup {
        index: usize,
        last_at_unix: i64,
    }

    let mut groups: Vec<PacketGroup> = Vec::new();
    let mut open: HashMap<String, OpenGroup> = HashMap::new();

    // Process oldest-first so the time-window check compares each entry
    // against the group's most recently added (i.e. previous) member.
    for entry in packets.iter().rev() {
        let key = group_key(entry);

        let reused = key
            .as_ref()
            .and_then(|k| open.get_mut(k))
            .filter(|open_group| entry.at_unix - open_group.last_at_unix <= GROUP_WINDOW_SECS);

        if let Some(open_group) = reused {
            groups[open_group.index].members.insert(0, entry.clone());
            open_group.last_at_unix = entry.at_unix;
        } else {
            groups.push(PacketGroup {
                members: vec![entry.clone()],
            });
            if let Some(k) = key {
                open.insert(
                    k,
                    OpenGroup {
                        index: groups.len() - 1,
                        last_at_unix: entry.at_unix,
                    },
                );
            }
        }
    }

    groups.reverse();
    groups
}

/// Grouping key: entries with the same decoded payload type and identical
/// inner payload bytes are, in practice, the same transmission — relays
/// forward the payload unchanged, while distinct events (e.g. two adverts
/// from the same node) carry different bytes (timestamp, signature, ...).
fn group_key(entry: &PacketLogEntry) -> Option<String> {
    let header = entry.header.as_ref()?;
    Some(format!("{}:{}", header.payload_type, entry.payload_hex))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fez_mesh_controller_core::mesh::PacketHeaderInfo;

    fn entry(id: u64, at_unix: i64, payload_type: &str, payload_hex: &str) -> PacketLogEntry {
        PacketLogEntry {
            id,
            at_unix,
            snr: 1.0,
            rssi: -90,
            header: Some(PacketHeaderInfo {
                route_type: "Flood".to_string(),
                payload_type: payload_type.to_string(),
                payload_version: 0,
                hops: 1,
                path_hash_size: 1,
                path_hex: String::new(),
                transport_code_hex: None,
                dest_hash_hex: None,
                src_hash_hex: None,
                advertisement: None,
            }),
            payload_hex: payload_hex.to_string(),
            payload_len: payload_hex.len() / 2,
        }
    }

    fn entry_without_header(id: u64, at_unix: i64) -> PacketLogEntry {
        PacketLogEntry {
            id,
            at_unix,
            snr: 1.0,
            rssi: -90,
            header: None,
            payload_hex: "ab".to_string(),
            payload_len: 1,
        }
    }

    #[test]
    fn groups_identical_payloads_seen_close_in_time() {
        // Newest-first, as stored by the app: id 3 (t=20) heard after id 2
        // (t=10) after id 1 (t=0), all relaying the same packet.
        let packets = vec![
            entry(3, 20, "TextMsg", "aabb"),
            entry(2, 10, "TextMsg", "aabb"),
            entry(1, 0, "TextMsg", "aabb"),
        ];

        let groups = group_packets(&packets);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].count(), 3);
        assert_eq!(groups[0].latest().id, 3);
    }

    #[test]
    fn does_not_group_different_payloads() {
        let packets = vec![
            entry(2, 10, "TextMsg", "cccc"),
            entry(1, 0, "TextMsg", "aabb"),
        ];
        let groups = group_packets(&packets);
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn does_not_group_identical_payloads_far_apart_in_time() {
        let packets = vec![
            entry(2, GROUP_WINDOW_SECS + 100, "TextMsg", "aabb"),
            entry(1, 0, "TextMsg", "aabb"),
        ];
        let groups = group_packets(&packets);
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn chains_across_interleaved_unrelated_packets() {
        // The same payload heard twice, with an unrelated packet received
        // in between, must still be grouped.
        let packets = vec![
            entry(3, 20, "TextMsg", "aabb"),
            entry(2, 10, "Advert", "ffff"),
            entry(1, 0, "TextMsg", "aabb"),
        ];
        let groups = group_packets(&packets);
        assert_eq!(groups.len(), 2);
        let text_group = groups.iter().find(|g| g.latest().id == 3).unwrap();
        assert_eq!(text_group.count(), 2);
    }

    #[test]
    fn never_groups_entries_without_a_decoded_header() {
        let packets = vec![entry_without_header(2, 10), entry_without_header(1, 0)];
        let groups = group_packets(&packets);
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn preserves_newest_first_group_order() {
        let packets = vec![
            entry(3, 30, "Ack", "01"),
            entry(2, 20, "TextMsg", "aabb"),
            entry(1, 10, "TextMsg", "aabb"),
        ];
        let groups = group_packets(&packets);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].latest().id, 3);
        assert_eq!(groups[1].latest().id, 2);
    }
}
