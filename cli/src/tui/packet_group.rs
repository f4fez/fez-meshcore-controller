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

use fez_mesh_controller_core::mesh::{ContactDto, PacketLogEntry};

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

    /// Whether this packet's destination or source hash belongs to a
    /// managed repeater. Checked against [`Self::latest`] only: the
    /// endpoint hashes are part of the inner payload, which is identical
    /// across every member of the group by construction (see
    /// [`group_key`]).
    pub fn endpoint_is_managed_repeater(&self, contacts: &[ContactDto]) -> bool {
        let Some(header) = &self.latest().header else {
            return false;
        };
        [&header.dest_hash_hex, &header.src_hash_hex]
            .into_iter()
            .flatten()
            .any(|hash| hash_matches_a_managed_repeater(hash, contacts))
    }

    /// Whether any repeater that relayed this packet — across *every*
    /// reception in the group, since each one may have taken a different
    /// path — is a managed repeater.
    pub fn relayed_by_managed_repeater(&self, contacts: &[ContactDto]) -> bool {
        self.members.iter().any(|member| {
            let Some(header) = &member.header else {
                return false;
            };
            path_hop_hashes(&header.path_hex, header.path_hash_size)
                .iter()
                .any(|hop| hash_matches_a_managed_repeater(hop, contacts))
        })
    }
}

/// Whether `hash_hex` (a truncated node address hash, as decoded onto
/// [`fez_mesh_controller_core::mesh::PacketHeaderInfo`]) is a prefix of a
/// managed contact's public key — the same "starts_with" matching
/// `ManagedRepeater::matches` uses, just against a shorter hash than a full
/// 6-byte contact prefix.
fn hash_matches_a_managed_repeater(hash_hex: &str, contacts: &[ContactDto]) -> bool {
    let hash_hex = hash_hex.to_ascii_lowercase();
    contacts.iter().any(|c| {
        c.managed
            && c.public_key_prefix_hex
                .to_ascii_lowercase()
                .starts_with(&hash_hex)
    })
}

/// Splits a header's concatenated hop-hash path into its individual
/// `hash_size`-byte hashes (hex-encoded), newest-hop-first as stored.
/// Empty if `hash_size` is zero or `path_hex` doesn't divide evenly (a
/// malformed/undecodable path).
pub fn path_hop_hashes(path_hex: &str, hash_size: u8) -> Vec<&str> {
    let chunk_len = hash_size as usize * 2;
    if chunk_len == 0 || !path_hex.len().is_multiple_of(chunk_len) {
        return Vec::new();
    }
    path_hex
        .as_bytes()
        .chunks(chunk_len)
        .map(|c| std::str::from_utf8(c).expect("hex string is always ASCII"))
        .collect()
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

    fn contact(public_key_prefix_hex: &str, managed: bool) -> ContactDto {
        ContactDto {
            name: "Node".to_string(),
            public_key_prefix_hex: public_key_prefix_hex.to_string(),
            last_advert_unix: 0,
            lat: 0.0,
            lon: 0.0,
            registered: true,
            managed,
            contact_type: 2, // Repeater
        }
    }

    fn entry(id: u64, at_unix: i64, payload_type: &str, payload_hex: &str) -> PacketLogEntry {
        PacketLogEntry {
            id,
            at_unix,
            snr: 1.0,
            rssi: -90,
            header: Some(PacketHeaderInfo {
                route_type: "Flood".to_string(),
                payload_type: payload_type.to_string(),
                payload_type_raw: 0,
                payload_version: 0,
                hops: 1,
                path_hash_size: 1,
                path_hex: String::new(),
                transport_code_hex: None,
                dest_hash_hex: None,
                src_hash_hex: None,
                channel_hash_hex: None,
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

    // --- endpoint_is_managed_repeater -----------------------------------

    #[test]
    fn endpoint_is_managed_repeater_matches_dest_or_src_hash() {
        let mut e = entry(1, 0, "TextMsg", "deadbeef");
        let h = e.header.as_mut().unwrap();
        h.dest_hash_hex = Some("de".to_string());
        h.src_hash_hex = Some("ad".to_string());
        let group = PacketGroup { members: vec![e] };

        assert!(group.endpoint_is_managed_repeater(&[contact("deadbeefcafe", true)]));
    }

    #[test]
    fn endpoint_is_managed_repeater_false_when_no_match() {
        let mut e = entry(1, 0, "TextMsg", "deadbeef");
        let h = e.header.as_mut().unwrap();
        h.dest_hash_hex = Some("11".to_string());
        h.src_hash_hex = Some("22".to_string());
        let group = PacketGroup { members: vec![e] };

        assert!(!group.endpoint_is_managed_repeater(&[contact("deadbeefcafe", true)]));
    }

    #[test]
    fn endpoint_is_managed_repeater_ignores_unmanaged_contacts() {
        let mut e = entry(1, 0, "TextMsg", "deadbeef");
        e.header.as_mut().unwrap().dest_hash_hex = Some("de".to_string());
        let group = PacketGroup { members: vec![e] };

        assert!(!group.endpoint_is_managed_repeater(&[contact("deadbeefcafe", false)]));
    }

    #[test]
    fn endpoint_is_managed_repeater_ignores_a_channel_hash_matching_a_managed_repeater() {
        // GroupText/GroupData carry a channel hash, not a node address hash
        // — even if it happens to share its prefix with a managed
        // repeater's public key, that's a coincidence in a different hash
        // space and must not trigger a false-positive endpoint match.
        let mut e = entry(1, 0, "GroupText", "deadbeef");
        e.header.as_mut().unwrap().channel_hash_hex = Some("de".to_string());
        let group = PacketGroup { members: vec![e] };

        assert!(!group.endpoint_is_managed_repeater(&[contact("deadbeefcafe", true)]));
    }

    // --- relayed_by_managed_repeater -------------------------------------

    #[test]
    fn relayed_by_managed_repeater_checks_every_members_path() {
        let mut newest = entry(2, 20, "TextMsg", "aabb");
        {
            let h = newest.header.as_mut().unwrap();
            h.path_hash_size = 1;
            h.path_hex = "1122".to_string();
        }
        let mut oldest = entry(1, 10, "TextMsg", "aabb");
        {
            let h = oldest.header.as_mut().unwrap();
            h.path_hash_size = 1;
            // Only this member's path includes the managed repeater's hash.
            h.path_hex = "deadbeef".to_string();
        }
        let group = PacketGroup {
            members: vec![newest, oldest],
        };

        assert!(group.relayed_by_managed_repeater(&[contact("deadbeefcafe", true)]));
    }

    #[test]
    fn relayed_by_managed_repeater_false_when_no_hop_matches() {
        let mut e = entry(1, 0, "TextMsg", "aabb");
        {
            let h = e.header.as_mut().unwrap();
            h.path_hash_size = 1;
            h.path_hex = "1122".to_string();
        }
        let group = PacketGroup { members: vec![e] };

        assert!(!group.relayed_by_managed_repeater(&[contact("deadbeefcafe", true)]));
    }

    // --- path_hop_hashes --------------------------------------------------

    #[test]
    fn path_hop_hashes_splits_into_hash_size_chunks() {
        assert_eq!(path_hop_hashes("11223344", 2), vec!["1122", "3344"]);
    }

    #[test]
    fn path_hop_hashes_empty_for_a_zero_hash_size_or_uneven_length() {
        assert!(path_hop_hashes("aabb", 0).is_empty());
        assert!(path_hop_hashes("112233", 2).is_empty()); // not divisible by 4
        assert!(path_hop_hashes("", 1).is_empty());
    }
}
