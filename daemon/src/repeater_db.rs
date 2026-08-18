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

//! Persists overheard-but-not-yet-a-contact repeaters/room-servers ("heard"
//! network state) in SQLite, surviving daemon restarts. This is the source
//! of truth for that data — `AppState.discovered_repeaters` is just a
//! write-through, startup-hydrated in-memory mirror of it (`state.rs`), not
//! a separate cache with its own eviction policy.
//!
//! `rusqlite` is a synchronous API; every call here runs inside
//! `tokio::task::spawn_blocking`, and the underlying [`rusqlite::Connection`]
//! is only ever touched from within one of those blocking closures, never
//! held across an `.await`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use fez_mesh_controller_core::mesh::DiscoveredNode;
use rusqlite::{params, Connection};

/// A repeater/room-server's persisted "heard" state — current-state only,
/// one row per public key (no sightings-history table, to avoid unbounded
/// growth). See the module doc for why the DB, not this table's schema
/// alone, is described as the "source of truth".
#[derive(Clone)]
pub struct RepeaterDb {
    conn: Arc<Mutex<Connection>>,
}

impl RepeaterDb {
    /// Opens (creating if needed) the SQLite file at `path` and ensures the
    /// schema exists.
    pub async fn open(path: PathBuf) -> rusqlite::Result<Self> {
        tokio::task::spawn_blocking(move || {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = std::fs::create_dir_all(parent);
                }
            }
            let conn = Connection::open(path)?;
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS repeaters (
                    public_key_hex        TEXT PRIMARY KEY,
                    public_key_prefix_hex TEXT NOT NULL,
                    name                   TEXT NOT NULL,
                    adv_type               INTEGER NOT NULL,
                    lat                    REAL,
                    lon                    REAL,
                    last_snr               REAL,
                    last_rssi              INTEGER,
                    last_hop_count         INTEGER,
                    first_seen_unix        INTEGER NOT NULL,
                    last_seen_unix         INTEGER NOT NULL,
                    sighting_count         INTEGER NOT NULL DEFAULT 1
                );
                CREATE INDEX IF NOT EXISTS idx_repeaters_prefix
                    ON repeaters(public_key_prefix_hex);",
            )?;
            Ok(Self {
                conn: Arc::new(Mutex::new(conn)),
            })
        })
        .await
        .expect("repeater_db open task panicked")
    }

    /// Upserts a sighting: merges into any existing row for the same
    /// public key rather than overwriting it wholesale. `lat`/`lon` (and
    /// the signal/hop fields) only replace a previously-known value when
    /// the new sighting actually carries one (`COALESCE(excluded.x,
    /// repeaters.x)`) -- an advert without a position must not erase a
    /// position learned from an earlier one. [`DiscoveredNode::lat`]/`lon`
    /// use `0.0` as their own "unknown" sentinel (matching
    /// `extract_discovered_node`'s existing convention), so that exact
    /// pair is treated as "no position" here too and stored as `NULL`.
    /// `sighting_count` increments; `first_seen_unix` is left untouched on
    /// conflict (not part of the `ON CONFLICT` `SET` clause).
    pub async fn upsert(&self, node: &DiscoveredNode) -> rusqlite::Result<()> {
        let node = node.clone();
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().expect("repeater_db mutex poisoned");
            let (lat, lon) = if node.lat == 0.0 && node.lon == 0.0 {
                (None, None)
            } else {
                (Some(node.lat), Some(node.lon))
            };
            conn.execute(
                "INSERT INTO repeaters (
                    public_key_hex, public_key_prefix_hex, name, adv_type,
                    lat, lon, last_snr, last_rssi, last_hop_count,
                    first_seen_unix, last_seen_unix, sighting_count
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, 1)
                ON CONFLICT(public_key_hex) DO UPDATE SET
                    public_key_prefix_hex = excluded.public_key_prefix_hex,
                    name = excluded.name,
                    adv_type = excluded.adv_type,
                    lat = COALESCE(excluded.lat, repeaters.lat),
                    lon = COALESCE(excluded.lon, repeaters.lon),
                    last_snr = COALESCE(excluded.last_snr, repeaters.last_snr),
                    last_rssi = COALESCE(excluded.last_rssi, repeaters.last_rssi),
                    last_hop_count = COALESCE(excluded.last_hop_count, repeaters.last_hop_count),
                    last_seen_unix = excluded.last_seen_unix,
                    sighting_count = repeaters.sighting_count + 1",
                params![
                    node.public_key_hex,
                    node.public_key_prefix_hex,
                    node.name,
                    node.adv_type,
                    lat,
                    lon,
                    node.last_snr,
                    node.last_rssi,
                    node.last_hop_count,
                    node.last_seen_unix,
                ],
            )?;
            Ok(())
        })
        .await
        .expect("repeater_db upsert task panicked")
    }

    /// Loads every row, for hydrating `AppState.discovered_repeaters` at
    /// startup — keyed by `public_key_prefix_hex`, matching the in-memory
    /// mirror's own key.
    pub async fn load_all(&self) -> rusqlite::Result<HashMap<String, DiscoveredNode>> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().expect("repeater_db mutex poisoned");
            let mut stmt = conn.prepare(
                "SELECT public_key_hex, public_key_prefix_hex, name, adv_type,
                        lat, lon, last_snr, last_rssi, last_hop_count, last_seen_unix
                 FROM repeaters",
            )?;
            let rows = stmt.query_map([], |row| {
                let adv_type: u8 = row.get(3)?;
                let lat: Option<f64> = row.get(4)?;
                let lon: Option<f64> = row.get(5)?;
                Ok(DiscoveredNode {
                    public_key_hex: row.get(0)?,
                    public_key_prefix_hex: row.get(1)?,
                    name: row.get(2)?,
                    adv_type,
                    is_repeater: adv_type == 2, // see declare_contact's CONTACT_TYPENAMES note
                    lat: lat.unwrap_or(0.0),
                    lon: lon.unwrap_or(0.0),
                    last_snr: row.get(6)?,
                    last_rssi: row.get(7)?,
                    last_hop_count: row.get(8)?,
                    last_seen_unix: row.get(9)?,
                })
            })?;

            let mut nodes = HashMap::new();
            for row in rows {
                let node = row?;
                nodes.insert(node.public_key_prefix_hex.clone(), node);
            }
            Ok(nodes)
        })
        .await
        .expect("repeater_db load_all task panicked")
    }
}

#[cfg(test)]
impl RepeaterDb {
    /// Test-only: reads the raw `sighting_count` column, which
    /// [`DiscoveredNode`] doesn't carry (it's a DB-internal bookkeeping
    /// value, not part of the daemon's public "heard node" shape).
    async fn sighting_count(&self, public_key_hex: &str) -> i64 {
        let key = public_key_hex.to_string();
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().expect("repeater_db mutex poisoned");
            conn.query_row(
                "SELECT sighting_count FROM repeaters WHERE public_key_hex = ?1",
                params![key],
                |row| row.get(0),
            )
            .expect("row present")
        })
        .await
        .expect("sighting_count task panicked")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_node(public_key_hex: &str, last_seen_unix: i64) -> DiscoveredNode {
        DiscoveredNode {
            name: "Node A".to_string(),
            public_key_hex: public_key_hex.to_string(),
            public_key_prefix_hex: public_key_hex.chars().take(12).collect(),
            is_repeater: true,
            adv_type: 2,
            lat: 48.85,
            lon: 2.35,
            last_seen_unix,
            last_snr: Some(4.0),
            last_rssi: Some(-90),
            last_hop_count: Some(2),
        }
    }

    async fn in_memory_db() -> RepeaterDb {
        RepeaterDb::open(PathBuf::from(":memory:"))
            .await
            .expect("open in-memory db")
    }

    #[tokio::test]
    async fn open_creates_an_empty_schema() {
        let db = in_memory_db().await;
        assert!(db.load_all().await.expect("load_all").is_empty());
    }

    #[tokio::test]
    async fn upsert_then_load_all_round_trips() {
        let db = in_memory_db().await;
        let node = sample_node(&"ab".repeat(32), 1_700_000_000);

        db.upsert(&node).await.expect("upsert");
        let loaded = db.load_all().await.expect("load_all");

        let stored = loaded
            .get(&node.public_key_prefix_hex)
            .expect("row present");
        assert_eq!(stored.public_key_hex, node.public_key_hex);
        assert_eq!(stored.name, "Node A");
        assert!(stored.is_repeater);
        assert_eq!(stored.adv_type, 2);
        assert_eq!(stored.lat, 48.85);
        assert_eq!(stored.lon, 2.35);
        assert_eq!(stored.last_snr, Some(4.0));
        assert_eq!(stored.last_rssi, Some(-90));
        assert_eq!(stored.last_hop_count, Some(2));
        assert_eq!(stored.last_seen_unix, 1_700_000_000);
    }

    #[tokio::test]
    async fn upsert_without_a_position_does_not_clear_a_previously_known_one() {
        let db = in_memory_db().await;
        let key = "ab".repeat(32);
        db.upsert(&sample_node(&key, 1_700_000_000)).await.unwrap();

        let mut without_position = sample_node(&key, 1_700_000_100);
        without_position.lat = 0.0;
        without_position.lon = 0.0;
        db.upsert(&without_position).await.unwrap();

        let loaded = db.load_all().await.unwrap();
        let stored = &loaded[&without_position.public_key_prefix_hex];
        assert_eq!(stored.lat, 48.85);
        assert_eq!(stored.lon, 2.35);
        assert_eq!(stored.last_seen_unix, 1_700_000_100);
    }

    #[tokio::test]
    async fn repeated_upserts_increment_sighting_count() {
        let db = in_memory_db().await;
        let key = "ab".repeat(32);
        db.upsert(&sample_node(&key, 1_700_000_000)).await.unwrap();
        db.upsert(&sample_node(&key, 1_700_000_100)).await.unwrap();
        db.upsert(&sample_node(&key, 1_700_000_200)).await.unwrap();

        let loaded = db.load_all().await.unwrap();
        let prefix = sample_node(&key, 0).public_key_prefix_hex;
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[&prefix].last_seen_unix, 1_700_000_200);
        assert_eq!(db.sighting_count(&key).await, 3);
    }

    #[tokio::test]
    async fn upsert_replaces_latest_signal_and_hop_fields() {
        let db = in_memory_db().await;
        let key = "ab".repeat(32);
        db.upsert(&sample_node(&key, 1_700_000_000)).await.unwrap();

        let mut second = sample_node(&key, 1_700_000_100);
        second.last_snr = Some(-1.5);
        second.last_rssi = Some(-110);
        second.last_hop_count = Some(4);
        db.upsert(&second).await.unwrap();

        let loaded = db.load_all().await.unwrap();
        let stored = &loaded[&second.public_key_prefix_hex];
        assert_eq!(stored.last_snr, Some(-1.5));
        assert_eq!(stored.last_rssi, Some(-110));
        assert_eq!(stored.last_hop_count, Some(4));
    }

    #[tokio::test]
    async fn two_different_public_keys_produce_two_rows() {
        let db = in_memory_db().await;
        db.upsert(&sample_node(&"ab".repeat(32), 1_700_000_000))
            .await
            .unwrap();
        db.upsert(&sample_node(&"cd".repeat(32), 1_700_000_000))
            .await
            .unwrap();

        let loaded = db.load_all().await.unwrap();
        assert_eq!(loaded.len(), 2);
    }
}
