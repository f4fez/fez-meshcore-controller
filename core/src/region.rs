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

//! Region hierarchy business logic: precomputing region keys, matching a
//! packet's transport code against configured regions, and flattening the
//! region tree for display. The actual crypto primitives live in
//! [`crate::meshcore_crypto`].

use std::collections::HashSet;

use crate::config::RegionConfig;
use crate::mesh::PacketLogEntry;
use crate::meshcore_crypto::{calc_transport_code, derive_region_key};

/// Precomputes every configured region's transport key, keyed by name.
/// Call once when a fresh region list arrives (e.g. on a new `Snapshot`),
/// not per packet — only the key derivation is region-only; the transport
/// code itself still depends on each packet's own payload (see
/// [`matching_region_name`]).
pub fn precompute_region_keys(regions: &[RegionConfig]) -> Vec<(String, [u8; 16])> {
    regions
        .iter()
        .map(|r| (r.name.clone(), derive_region_key(&r.name)))
        .collect()
}

/// Finds which configured region (if any) produced a packet's observed
/// transport code — the header's *first* transport code half only (the
/// second is reserved, see [`crate::mesh::PacketHeaderInfo::transport_code_hex`]).
/// Returns the matching region's name, or `None` if the packet has no
/// decoded header, no transport code, or no configured region matches.
pub fn matching_region_name<'a>(
    entry: &PacketLogEntry,
    region_keys: &'a [(String, [u8; 16])],
) -> Option<&'a str> {
    let header = entry.header.as_ref()?;
    let transport_code_hex = header.transport_code_hex.as_deref()?;
    if transport_code_hex.len() < 4 {
        return None;
    }
    let observed = meshcore_rs::parsing::hex_decode(&transport_code_hex[..4]).ok()?;
    let payload = meshcore_rs::parsing::hex_decode(&entry.payload_hex).ok()?;

    region_keys.iter().find_map(|(name, key)| {
        let computed = calc_transport_code(key, header.payload_type_raw, &payload);
        (computed[..] == observed[..]).then_some(name.as_str())
    })
}

/// Flattens a region hierarchy (parent-referenced by name, mirroring the
/// MeshCore firmware's `RegionMap` id/parent tree) into a stable display
/// order: roots first (in config order), each immediately followed by its
/// descendants, paired with depth for indentation.
///
/// Defensive against malformed config: a region whose declared `parent`
/// name isn't itself configured is treated as a root rather than dropped
/// or panicking; a visited-name guard prevents infinite recursion if a
/// config accidentally has a parent cycle (a cyclic region is silently
/// omitted rather than hanging).
pub fn flatten_region_tree(regions: &[RegionConfig]) -> Vec<(usize, &RegionConfig)> {
    let known_names: HashSet<&str> = regions.iter().map(|r| r.name.as_str()).collect();
    let mut visited: HashSet<&str> = HashSet::new();
    let mut out = Vec::with_capacity(regions.len());

    let is_root = |r: &RegionConfig| match &r.parent {
        None => true,
        Some(parent) => !known_names.contains(parent.as_str()),
    };

    for region in regions.iter().filter(|r| is_root(r)) {
        push_with_children(regions, region, 0, &mut visited, &mut out);
    }

    out
}

fn push_with_children<'a>(
    regions: &'a [RegionConfig],
    region: &'a RegionConfig,
    depth: usize,
    visited: &mut HashSet<&'a str>,
    out: &mut Vec<(usize, &'a RegionConfig)>,
) {
    if !visited.insert(region.name.as_str()) {
        return; // already placed — guards against a parent cycle
    }
    out.push((depth, region));
    for child in regions
        .iter()
        .filter(|r| r.parent.as_deref() == Some(region.name.as_str()))
    {
        push_with_children(regions, child, depth + 1, visited, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::PacketHeaderInfo;

    fn region(name: &str, parent: Option<&str>) -> RegionConfig {
        RegionConfig {
            name: name.to_string(),
            parent: parent.map(str::to_string),
        }
    }

    fn header(transport_code_hex: Option<&str>) -> PacketHeaderInfo {
        PacketHeaderInfo {
            route_type: "TransportFlood".to_string(),
            payload_type: "TextMsg".to_string(),
            payload_type_raw: 2,
            payload_version: 0,
            hops: 1,
            path_hash_size: 1,
            path_hex: String::new(),
            transport_code_hex: transport_code_hex.map(str::to_string),
            dest_hash_hex: None,
            src_hash_hex: None,
            channel_hash_hex: None,
            advertisement: None,
        }
    }

    fn entry(header: Option<PacketHeaderInfo>, payload_hex: &str) -> PacketLogEntry {
        PacketLogEntry {
            id: 1,
            at_unix: 0,
            snr: 1.0,
            rssi: -90,
            header,
            payload_hex: payload_hex.to_string(),
            payload_len: payload_hex.len() / 2,
        }
    }

    // --- matching_region_name --------------------------------------------

    #[test]
    fn matching_region_name_finds_the_configured_region() {
        // Same verified vector as meshcore_crypto's tests: region
        // "TestRegion", payload_type 2, payload "deadbeef" -> code "a518".
        let regions = vec![region("TestRegion", None)];
        let region_keys = precompute_region_keys(&regions);
        let e = entry(Some(header(Some("a5180000"))), "deadbeef");

        assert_eq!(matching_region_name(&e, &region_keys), Some("TestRegion"));
    }

    #[test]
    fn matching_region_name_none_when_no_region_matches() {
        let regions = vec![region("SomeOtherRegion", None)];
        let region_keys = precompute_region_keys(&regions);
        let e = entry(Some(header(Some("a5180000"))), "deadbeef");

        assert_eq!(matching_region_name(&e, &region_keys), None);
    }

    #[test]
    fn matching_region_name_none_without_a_transport_code() {
        let regions = vec![region("TestRegion", None)];
        let region_keys = precompute_region_keys(&regions);
        let e = entry(Some(header(None)), "deadbeef");

        assert_eq!(matching_region_name(&e, &region_keys), None);
    }

    #[test]
    fn matching_region_name_none_without_a_decoded_header() {
        let regions = vec![region("TestRegion", None)];
        let region_keys = precompute_region_keys(&regions);
        let e = entry(None, "deadbeef");

        assert_eq!(matching_region_name(&e, &region_keys), None);
    }

    // --- flatten_region_tree ----------------------------------------------

    #[test]
    fn flatten_region_tree_orders_roots_then_descendants() {
        let regions = vec![
            region("World", None),
            region("Europe", Some("World")),
            region("France", Some("Europe")),
            region("Asia", Some("World")),
        ];

        let flat: Vec<(usize, &str)> = flatten_region_tree(&regions)
            .into_iter()
            .map(|(d, r)| (d, r.name.as_str()))
            .collect();

        assert_eq!(
            flat,
            vec![(0, "World"), (1, "Europe"), (2, "France"), (1, "Asia"),]
        );
    }

    #[test]
    fn flatten_region_tree_treats_an_orphan_parent_as_a_root() {
        let regions = vec![region("France", Some("DoesNotExist"))];

        let flat = flatten_region_tree(&regions);

        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0], (0, &regions[0]));
    }

    #[test]
    fn flatten_region_tree_does_not_hang_on_a_parent_cycle() {
        // A <-> B pointing at each other: neither is a "root" (both have a
        // configured parent), so this exercises the visited-name guard
        // inside push_with_children rather than the root-detection path.
        let regions = vec![region("A", Some("B")), region("B", Some("A"))];

        let flat = flatten_region_tree(&regions);

        // Neither is treated as a root (both have a known parent), so the
        // cycle is simply never entered — an empty, non-hanging result.
        assert!(flat.is_empty());
    }

    #[test]
    fn flatten_region_tree_empty_for_no_regions() {
        assert!(flatten_region_tree(&[]).is_empty());
    }
}
