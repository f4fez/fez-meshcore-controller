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

//! Small formatting helpers shared by `status` and the TUI.

pub fn format_last_seen(unix_ts: u32) -> String {
    if unix_ts == 0 {
        return "never".to_string();
    }
    // Heuristic: below this threshold the value is probably not a usable
    // Unix timestamp (the node's clock isn't synchronized).
    if (unix_ts as i64) < 1_600_000_000 {
        return format!("#{unix_ts}");
    }

    let now = chrono::Utc::now().timestamp();
    let diff = now - unix_ts as i64;
    if diff < 0 {
        "just now".to_string()
    } else if diff < 60 {
        format!("{diff}s ago")
    } else if diff < 3600 {
        format!("{}min ago", diff / 60)
    } else if diff < 86_400 {
        format!("{}h ago", diff / 3600)
    } else {
        format!("{}d ago", diff / 86_400)
    }
}

pub fn format_coords(lat: f64, lon: f64) -> String {
    if lat == 0.0 && lon == 0.0 {
        "—".to_string()
    } else {
        format!("{lat:.4}, {lon:.4}")
    }
}
