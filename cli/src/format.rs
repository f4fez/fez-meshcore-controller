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

/// Formats a duration in seconds as a compact "Xd Yh"/"Xh Ym"/"Xm" (the two
/// most significant non-zero units) — e.g. a node's reported uptime.
pub fn format_uptime(total_secs: u32) -> String {
    let days = total_secs / 86_400;
    let hours = (total_secs % 86_400) / 3600;
    let minutes = (total_secs % 3600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

pub fn format_coords(lat: f64, lon: f64) -> String {
    if lat == 0.0 && lon == 0.0 {
        "—".to_string()
    } else {
        format!("{lat:.4}, {lon:.4}")
    }
}

/// Strips flag emoji (a pair of "Regional Indicator Symbol" codepoints,
/// U+1F1E6-U+1F1FF) from remotely-supplied display text (node/contact
/// names), before rendering with ratatui.
///
/// ratatui (this project pins 0.29) miscalculates the display width of
/// this specific kind of multi-codepoint grapheme cluster: it places both
/// codepoints into a single buffer cell but only advances the cursor by
/// one column, while real terminals draw the combined flag two columns
/// wide — so everything rendered after it on the same line shifts one
/// column left of where it should be. This is a confirmed, long-standing,
/// unresolved upstream limitation shared by other `tui`-style crates
/// (`ratatui/ratatui#75`, "Buffer: unicode-width and emojis" — open since
/// 2024, `Status: On Hold`, `Effort: Difficult`), not something fixable in
/// application code, so remote names (which we don't control) are
/// sanitized here instead.
pub fn strip_flag_emoji(text: &str) -> String {
    text.chars()
        .filter(|c| !('\u{1F1E6}'..='\u{1F1FF}').contains(c))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_flag_emoji_removes_a_leading_flag() {
        assert_eq!(
            strip_flag_emoji("🇨🇵30 D\u{e9}partement"),
            "30 D\u{e9}partement"
        );
    }

    #[test]
    fn strip_flag_emoji_removes_multiple_flags() {
        assert_eq!(strip_flag_emoji("🇫🇷🇩🇪 Border repeater"), " Border repeater");
    }

    #[test]
    fn strip_flag_emoji_leaves_ordinary_text_and_emoji_untouched() {
        assert_eq!(strip_flag_emoji("F4FEZ Repeater 🛰️"), "F4FEZ Repeater 🛰️");
        assert_eq!(strip_flag_emoji(""), "");
    }

    #[test]
    fn format_uptime_shows_the_two_most_significant_units() {
        assert_eq!(format_uptime(45), "0m");
        assert_eq!(format_uptime(125), "2m");
        assert_eq!(format_uptime(3_725), "1h 2m");
        assert_eq!(format_uptime(93_784), "1d 2h");
    }
}
