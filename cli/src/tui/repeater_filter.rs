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

//! Filtering, grouping and sorting for the TUI's "Repeaters" panel
//! (`cli/src/tui/ui.rs::draw_repeaters`), configured through a popup (`f`
//! key) — see [`App::repeater_filter`](super::app::App::repeater_filter).

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use fez_mesh_controller_core::mesh::ContactDto;
use serde::{Deserialize, Serialize};

use crate::format::strip_flag_emoji;

/// The four management-tier categories a repeater can fall into for
/// display purposes — shared by the Repeaters panel, the repeater-detail
/// popup, and `repeater list`, so they can never disagree. Declared in
/// display/group order (see [`RepeaterFilter::group_by_type`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RepeaterGroup {
    Managed,
    Supervised,
    Known,
    Discovered,
}

/// Classifies a contact into its [`RepeaterGroup`] — the single source of
/// truth other code should use instead of re-deriving
/// `(c.repeater_status, c.registered)` itself.
pub fn repeater_group(c: &ContactDto) -> RepeaterGroup {
    use fez_mesh_controller_core::RepeaterStatus;
    match (c.repeater_status, c.registered) {
        (Some(RepeaterStatus::Managed), _) => RepeaterGroup::Managed,
        (Some(RepeaterStatus::Supervised), _) => RepeaterGroup::Supervised,
        (Some(RepeaterStatus::Known), _) | (None, true) => RepeaterGroup::Known,
        (None, false) => RepeaterGroup::Discovered,
    }
}

/// Sort order for the Repeaters panel — see [`sort_repeaters`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepeaterSort {
    #[default]
    LastHeard,
    Name,
    Distance,
}

/// The Repeaters panel's active filter/sort configuration, configured
/// through the `f`-key popup and persisted for the TUI session (not reset
/// on a fresh `Snapshot`) -- and across restarts, see
/// [`load_repeater_filter_prefs`]/[`save_repeater_filter_prefs`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RepeaterFilter {
    pub show_managed: bool,
    pub show_supervised: bool,
    pub show_known: bool,
    pub show_discovered: bool,
    pub sort: RepeaterSort,
    pub group_by_type: bool,
}

impl Default for RepeaterFilter {
    fn default() -> Self {
        Self {
            show_managed: true,
            show_supervised: true,
            show_known: true,
            show_discovered: true,
            sort: RepeaterSort::LastHeard,
            group_by_type: false,
        }
    }
}

impl RepeaterFilter {
    /// Whether a contact in `group` should be shown.
    pub fn shows(&self, group: RepeaterGroup) -> bool {
        match group {
            RepeaterGroup::Managed => self.show_managed,
            RepeaterGroup::Supervised => self.show_supervised,
            RepeaterGroup::Known => self.show_known,
            RepeaterGroup::Discovered => self.show_discovered,
        }
    }

    /// Whether any category is currently hidden — drives the Repeaters
    /// panel header's "filtered" indicator.
    pub fn is_filtering(&self) -> bool {
        !(self.show_managed && self.show_supervised && self.show_known && self.show_discovered)
    }
}

/// `~/.fez-mesh-controller/repeater_filter.toml` -- deliberately under the
/// user's home directory (`dirs::home_dir()`), not
/// `core::config::config_dir()` (the XDG config dir `config.toml` uses,
/// not guaranteed home-relative on every platform): this is a local UI
/// preference, not part of the daemon's config, so it isn't added to
/// `Config`/`config.toml` and has no `SIGHUP` reload concern.
pub fn prefs_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".fez-mesh-controller")
        .join("repeater_filter.toml")
}

/// Loads the persisted filter/sort preferences, if any. Best-effort: a
/// missing file (first run), a parse error (e.g. an older/incompatible
/// version), or any I/O error all silently fall back to
/// [`RepeaterFilter::default`] rather than blocking TUI startup.
pub fn load_repeater_filter_prefs() -> RepeaterFilter {
    load_repeater_filter_prefs_from(&prefs_path())
}

fn load_repeater_filter_prefs_from(path: &Path) -> RepeaterFilter {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| toml::from_str(&content).ok())
        .unwrap_or_default()
}

/// Persists the current filter/sort preferences — called when the popup
/// closes (see `App::close_repeater_filter`), not on every toggle.
pub fn save_repeater_filter_prefs(filter: &RepeaterFilter) -> std::io::Result<()> {
    save_repeater_filter_prefs_to(filter, &prefs_path())
}

fn save_repeater_filter_prefs_to(filter: &RepeaterFilter, path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = toml::to_string_pretty(filter).map_err(std::io::Error::other)?;
    std::fs::write(path, content)
}

/// Earth's mean radius in km, for [`haversine_km`].
const EARTH_RADIUS_KM: f64 = 6371.0;

/// Haversine great-circle distance in km between two `(lat, lon)` pairs in
/// degrees.
pub fn haversine_km(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (lat1, lon1) = (a.0.to_radians(), a.1.to_radians());
    let (lat2, lon2) = (b.0.to_radians(), b.1.to_radians());
    let (dlat, dlon) = (lat2 - lat1, lon2 - lon1);

    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_KM * h.sqrt().asin()
}

/// Whether `(lat, lon)` is the codebase-wide "unknown position" sentinel —
/// see `crate::format::format_coords`.
fn is_unknown_position(pos: (f64, f64)) -> bool {
    pos.0 == 0.0 && pos.1 == 0.0
}

/// Distance in km from `observer_position` to `c`, or `None` if either
/// position is unknown (see [`is_unknown_position`]) — contacts (or an
/// observer) with no known position sort last under
/// [`RepeaterSort::Distance`] rather than clustering at "null island".
fn distance_from_observer(c: &ContactDto, observer_position: Option<(f64, f64)>) -> Option<f64> {
    let observer_position = observer_position.filter(|&p| !is_unknown_position(p))?;
    let contact_position = (c.lat, c.lon);
    if is_unknown_position(contact_position) {
        return None;
    }
    Some(haversine_km(observer_position, contact_position))
}

fn compare_by_sort(
    a: &ContactDto,
    b: &ContactDto,
    sort: RepeaterSort,
    observer_position: Option<(f64, f64)>,
) -> Ordering {
    match sort {
        RepeaterSort::LastHeard => b.last_advert_unix.cmp(&a.last_advert_unix),
        RepeaterSort::Name => strip_flag_emoji(&a.name)
            .to_lowercase()
            .cmp(&strip_flag_emoji(&b.name).to_lowercase()),
        RepeaterSort::Distance => {
            let (da, db) = (
                distance_from_observer(a, observer_position),
                distance_from_observer(b, observer_position),
            );
            match (da, db) {
                (Some(da), Some(db)) => da.partial_cmp(&db).unwrap_or(Ordering::Equal),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            }
        }
    }
}

/// Sorts `contacts` in place per `filter`. If `filter.group_by_type`,
/// groups by [`RepeaterGroup`] first (in its declared
/// Managed/Supervised/Known/Discovered order) and sorts within each group;
/// otherwise sorts across all of them directly. `observer_position` is the
/// local node's own `(lat, lon)` (from `Snapshot.self_info`), used only by
/// [`RepeaterSort::Distance`].
pub fn sort_repeaters(
    contacts: &mut [&ContactDto],
    filter: &RepeaterFilter,
    observer_position: Option<(f64, f64)>,
) {
    contacts.sort_by(|a, b| {
        let group_ordering = if filter.group_by_type {
            repeater_group(a).cmp(&repeater_group(b))
        } else {
            Ordering::Equal
        };
        group_ordering.then_with(|| compare_by_sort(a, b, filter.sort, observer_position))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contact(name: &str, lat: f64, lon: f64, last_advert_unix: u32) -> ContactDto {
        ContactDto {
            name: name.to_string(),
            public_key_prefix_hex: "aabbccddeeff".to_string(),
            last_advert_unix,
            lat,
            lon,
            registered: true,
            managed: false,
            repeater_status: None,
            contact_type: 2,
            last_telemetry: None,
        }
    }

    // --- repeater_group --------------------------------------------------

    #[test]
    fn repeater_group_managed() {
        let c = ContactDto {
            repeater_status: Some(fez_mesh_controller_core::RepeaterStatus::Managed),
            ..contact("A", 0.0, 0.0, 0)
        };
        assert_eq!(repeater_group(&c), RepeaterGroup::Managed);
    }

    #[test]
    fn repeater_group_supervised() {
        let c = ContactDto {
            repeater_status: Some(fez_mesh_controller_core::RepeaterStatus::Supervised),
            ..contact("A", 0.0, 0.0, 0)
        };
        assert_eq!(repeater_group(&c), RepeaterGroup::Supervised);
    }

    #[test]
    fn repeater_group_known_explicit_status() {
        let c = ContactDto {
            repeater_status: Some(fez_mesh_controller_core::RepeaterStatus::Known),
            ..contact("A", 0.0, 0.0, 0)
        };
        assert_eq!(repeater_group(&c), RepeaterGroup::Known);
    }

    #[test]
    fn repeater_group_known_organic_registered() {
        let c = ContactDto {
            repeater_status: None,
            registered: true,
            ..contact("A", 0.0, 0.0, 0)
        };
        assert_eq!(repeater_group(&c), RepeaterGroup::Known);
    }

    #[test]
    fn repeater_group_discovered() {
        let c = ContactDto {
            repeater_status: None,
            registered: false,
            ..contact("A", 0.0, 0.0, 0)
        };
        assert_eq!(repeater_group(&c), RepeaterGroup::Discovered);
    }

    // --- RepeaterFilter ----------------------------------------------------

    #[test]
    fn default_filter_shows_everything_and_is_not_filtering() {
        let filter = RepeaterFilter::default();
        assert!(filter.shows(RepeaterGroup::Managed));
        assert!(filter.shows(RepeaterGroup::Supervised));
        assert!(filter.shows(RepeaterGroup::Known));
        assert!(filter.shows(RepeaterGroup::Discovered));
        assert!(!filter.is_filtering());
    }

    #[test]
    fn hiding_one_category_is_filtering() {
        let filter = RepeaterFilter {
            show_known: false,
            ..RepeaterFilter::default()
        };
        assert!(!filter.shows(RepeaterGroup::Known));
        assert!(filter.is_filtering());
    }

    // --- haversine_km --------------------------------------------------------

    #[test]
    fn haversine_km_zero_for_identical_points() {
        assert_eq!(haversine_km((48.8566, 2.3522), (48.8566, 2.3522)), 0.0);
    }

    #[test]
    fn haversine_km_paris_to_london_matches_known_distance() {
        // Paris (48.8566, 2.3522) to London (51.5074, -0.1278) is
        // published as ~344 km great-circle distance.
        let km = haversine_km((48.8566, 2.3522), (51.5074, -0.1278));
        assert!((340.0..=348.0).contains(&km), "got {km} km");
    }

    // --- sort_repeaters --------------------------------------------------

    #[test]
    fn sort_repeaters_last_heard_most_recent_first() {
        let a = contact("A", 0.0, 0.0, 100);
        let b = contact("B", 0.0, 0.0, 300);
        let c = contact("C", 0.0, 0.0, 200);
        let mut contacts = vec![&a, &b, &c];

        sort_repeaters(&mut contacts, &RepeaterFilter::default(), None);

        assert_eq!(
            contacts.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["B", "C", "A"]
        );
    }

    #[test]
    fn sort_repeaters_by_name_case_insensitive() {
        let a = contact("bravo", 0.0, 0.0, 0);
        let b = contact("Alpha", 0.0, 0.0, 0);
        let mut contacts = vec![&a, &b];

        let filter = RepeaterFilter {
            sort: RepeaterSort::Name,
            ..RepeaterFilter::default()
        };
        sort_repeaters(&mut contacts, &filter, None);

        assert_eq!(
            contacts.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["Alpha", "bravo"]
        );
    }

    #[test]
    fn sort_repeaters_by_distance_closest_first() {
        let observer = (48.8566, 2.3522); // Paris
        let near = contact("Near", 48.86, 2.35, 0); // ~a few hundred meters
        let far = contact("Far", 51.5074, -0.1278, 0); // London, ~344km
        let mut contacts = vec![&far, &near];

        let filter = RepeaterFilter {
            sort: RepeaterSort::Distance,
            ..RepeaterFilter::default()
        };
        sort_repeaters(&mut contacts, &filter, Some(observer));

        assert_eq!(
            contacts.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["Near", "Far"]
        );
    }

    #[test]
    fn sort_repeaters_by_distance_unknown_positions_sort_last() {
        let observer = (48.8566, 2.3522);
        let known = contact("Known", 48.86, 2.35, 0);
        let unknown = contact("Unknown", 0.0, 0.0, 0);
        let mut contacts = vec![&unknown, &known];

        let filter = RepeaterFilter {
            sort: RepeaterSort::Distance,
            ..RepeaterFilter::default()
        };
        sort_repeaters(&mut contacts, &filter, Some(observer));

        assert_eq!(
            contacts.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["Known", "Unknown"]
        );
    }

    #[test]
    fn sort_repeaters_by_distance_falls_back_when_observer_position_unknown() {
        let a = contact("A", 10.0, 10.0, 100);
        let b = contact("B", 20.0, 20.0, 200);
        let mut contacts = vec![&a, &b];

        let filter = RepeaterFilter {
            sort: RepeaterSort::Distance,
            ..RepeaterFilter::default()
        };
        // No observer position known -> every comparison is Equal, so the
        // stable sort leaves the original order untouched.
        sort_repeaters(&mut contacts, &filter, None);

        assert_eq!(
            contacts.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["A", "B"]
        );
    }

    #[test]
    fn sort_repeaters_groups_by_type_before_sorting_within_group() {
        let managed_old = ContactDto {
            repeater_status: Some(fez_mesh_controller_core::RepeaterStatus::Managed),
            ..contact("ManagedOld", 0.0, 0.0, 100)
        };
        let managed_new = ContactDto {
            repeater_status: Some(fez_mesh_controller_core::RepeaterStatus::Managed),
            ..contact("ManagedNew", 0.0, 0.0, 200)
        };
        let discovered = ContactDto {
            repeater_status: None,
            registered: false,
            ..contact("Discovered", 0.0, 0.0, 999) // most recent overall, but lowest-priority group
        };
        let mut contacts = vec![&discovered, &managed_old, &managed_new];

        let filter = RepeaterFilter {
            group_by_type: true,
            ..RepeaterFilter::default() // sort: LastHeard
        };
        sort_repeaters(&mut contacts, &filter, None);

        assert_eq!(
            contacts.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["ManagedNew", "ManagedOld", "Discovered"]
        );
    }

    // --- persistence -----------------------------------------------------

    #[test]
    fn repeater_filter_round_trips_through_toml() {
        let filter = RepeaterFilter {
            show_managed: true,
            show_supervised: false,
            show_known: true,
            show_discovered: false,
            sort: RepeaterSort::Distance,
            group_by_type: true,
        };

        let toml = toml::to_string(&filter).expect("serialize");
        let reloaded: RepeaterFilter = toml::from_str(&toml).expect("deserialize");

        assert_eq!(reloaded, filter);
    }

    #[test]
    fn repeater_sort_serializes_to_snake_case() {
        // TOML documents must be tables at the top level, so `RepeaterSort`
        // (a bare enum) is serialized here as it's actually used: as a
        // field of `RepeaterFilter`.
        for (sort, expected) in [
            (RepeaterSort::LastHeard, "\"last_heard\""),
            (RepeaterSort::Name, "\"name\""),
            (RepeaterSort::Distance, "\"distance\""),
        ] {
            let filter = RepeaterFilter {
                sort,
                ..RepeaterFilter::default()
            };
            let toml = toml::to_string(&filter).expect("serialize");
            assert!(
                toml.contains(&format!("sort = {expected}")),
                "expected `sort = {expected}` in:\n{toml}"
            );
        }
    }

    #[test]
    fn save_then_load_round_trips_through_a_real_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("repeater_filter.toml");

        let filter = RepeaterFilter {
            show_managed: false,
            sort: RepeaterSort::Name,
            group_by_type: true,
            ..RepeaterFilter::default()
        };
        save_repeater_filter_prefs_to(&filter, &path).expect("save");

        let reloaded = load_repeater_filter_prefs_from(&path);

        assert_eq!(reloaded, filter);
    }

    #[test]
    fn load_falls_back_to_default_for_a_missing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("does-not-exist.toml");

        assert_eq!(
            load_repeater_filter_prefs_from(&path),
            RepeaterFilter::default()
        );
    }

    #[test]
    fn load_falls_back_to_default_for_an_unparseable_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("corrupt.toml");
        std::fs::write(&path, "not valid toml {{{").expect("write");

        assert_eq!(
            load_repeater_filter_prefs_from(&path),
            RepeaterFilter::default()
        );
    }
}
