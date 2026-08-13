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

use chrono::{Local, TimeZone};
use fez_mesh_controller_core::ipc::MeshEvent;
use fez_mesh_controller_core::mesh::{MeshEventKind, PacketLogEntry};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table, Wrap,
};
use ratatui::Frame;

use crate::format::{format_coords, format_last_seen};
use crate::tui::app::{App, Page};
use crate::tui::packet_group::PacketGroup;

const CYAN: Color = Color::Rgb(0x4d, 0xd0, 0xe1);
const MAGENTA: Color = Color::Rgb(0xe0, 0x67, 0xf2);
const GREEN: Color = Color::Rgb(0x66, 0xe3, 0x8a);
const RED: Color = Color::Rgb(0xf2, 0x5c, 0x5c);
const YELLOW: Color = Color::Rgb(0xf2, 0xc9, 0x4c);
const MUTED: Color = Color::Rgb(0x7a, 0x84, 0x94);

fn block(title: impl Into<String>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(MUTED))
        .title(Span::styled(
            format!(" {} ", title.into()),
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        ))
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(1),
        ])
        .split(frame.area());

    draw_title(frame, app, root[0]);
    match app.page {
        Page::Dashboard => draw_body(frame, app, root[1]),
        Page::PacketLog => draw_packet_log_page(frame, app, root[1]),
    }
    draw_footer(frame, app, root[2]);

    if app.page == Page::PacketLog {
        if let Some(group) = &app.packet_detail {
            draw_packet_detail_popup(frame, group);
        }
    }

    if app.help_open {
        draw_help_popup(frame, app.page);
    }
}

fn page_tab(label: &str, active: bool) -> Span<'static> {
    let style = if active {
        Style::default()
            .fg(Color::Black)
            .bg(CYAN)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(MUTED)
    };
    Span::styled(format!(" {label} "), style)
}

fn draw_title(frame: &mut Frame, app: &App, area: Rect) {
    let title = Paragraph::new(Line::from(vec![
        Span::raw("📡 "),
        Span::styled(
            "fez-mesh-controller",
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        page_tab("F1 Help", app.help_open),
        Span::raw(" "),
        page_tab("F2 Dashboard", app.page == Page::Dashboard),
        Span::raw(" "),
        page_tab("F3 Packet log", app.page == Page::PacketLog),
    ]))
    .alignment(Alignment::Center)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(MAGENTA)),
    );
    frame.render_widget(title, area);
}

fn draw_body(frame: &mut Frame, app: &mut App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(36), Constraint::Percentage(64)])
        .split(area);

    draw_self_info(frame, app, cols[0]);

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(cols[1]);

    draw_contacts(frame, app, right[0]);
    draw_events(frame, app, right[1]);
}

fn draw_self_info(frame: &mut Frame, app: &App, area: Rect) {
    let lines: Vec<Line> = match &app.snapshot.self_info {
        Some(info) => {
            let key_short: String = info.public_key_hex.chars().take(16).collect();
            vec![
                field_line("🏷️  Name", info.name.clone()),
                field_line("🔑 Public key", format!("{key_short}…")),
                field_line("📶 Frequency", format!("{:.3} MHz", info.radio_freq_mhz)),
                field_line(
                    "📐 SF / CR",
                    format!("SF{} / CR{}", info.spreading_factor, info.coding_rate),
                ),
                field_line("🔋 TX power", format!("{} dBm", info.tx_power_dbm)),
                field_line("🌍 Position", format_coords(info.lat, info.lon)),
            ]
        }
        None => vec![Line::from(Span::styled(
            "Waiting for node data…",
            Style::default().fg(MUTED),
        ))],
    };

    frame.render_widget(Paragraph::new(lines).block(block("🛰️  Local node")), area);
}

fn field_line(label: &str, value: impl Into<String>) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<16}"), Style::default().fg(MUTED)),
        Span::styled(value.into(), Style::default().fg(Color::White)),
    ])
}

fn draw_contacts(frame: &mut Frame, app: &mut App, area: Rect) {
    let contacts = app.sorted_contacts();

    let header = Row::new(vec![
        Cell::from("Name"),
        Cell::from("Status"),
        Cell::from("Prefix"),
        Cell::from("Seen"),
        Cell::from("Position"),
    ])
    .style(Style::default().fg(CYAN).add_modifier(Modifier::BOLD));

    let rows = contacts.iter().map(|c| {
        let prefix: String = c.public_key_prefix_hex.chars().take(8).collect();

        let name_cell = if c.managed {
            Cell::from(Line::from(vec![
                Span::raw("🛰️ "),
                Span::styled(
                    c.name.clone(),
                    Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
                ),
            ]))
        } else {
            Cell::from(c.name.clone())
        };

        let (status_text, status_color) = match (c.registered, c.managed) {
            (_, true) => ("🛰️ Managed", GREEN),
            (true, false) => ("✅ Known", MUTED),
            (false, false) => ("🔍 Discovered", YELLOW),
        };
        let status_cell = Cell::from(Span::styled(status_text, Style::default().fg(status_color)));

        Row::new(vec![
            name_cell,
            status_cell,
            Cell::from(prefix),
            Cell::from(format_last_seen(c.last_advert_unix)),
            Cell::from(format_coords(c.lat, c.lon)),
        ])
    });

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(28),
            Constraint::Percentage(16),
            Constraint::Percentage(16),
            Constraint::Percentage(14),
            Constraint::Percentage(26),
        ],
    )
    .header(header)
    .row_highlight_style(
        Style::default()
            .bg(Color::Rgb(0x2a, 0x2e, 0x3a))
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("➤ ")
    .block(block(format!(
        "👥 Contacts ({})",
        app.snapshot.contacts.len()
    )));

    frame.render_stateful_widget(table, area, &mut app.contacts_state);
}

fn draw_events(frame: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .events
        .iter()
        .map(|ev| ListItem::new(event_line(ev)))
        .collect();

    frame.render_widget(List::new(items).block(block("📜 Event log")), area);
}

fn event_line(ev: &MeshEvent) -> Line<'static> {
    let time = Local
        .timestamp_opt(ev.at_unix, 0)
        .single()
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "--:--:--".to_string());

    let (emoji, text, color) = match &ev.kind {
        MeshEventKind::Connected => ("🔌", "Connected to MeshCore node".to_string(), GREEN),
        MeshEventKind::Disconnected => ("🔌", "Disconnected from MeshCore node".to_string(), RED),
        MeshEventKind::Advertisement { name, .. } => {
            ("📶", format!("Advertisement received from {name}"), CYAN)
        }
        MeshEventKind::NewContact { name } => ("🆕", format!("New contact: {name}"), MAGENTA),
        MeshEventKind::ContactMessage {
            from_prefix_hex,
            hops,
            text,
        } => (
            "💬",
            format!(
                "[{}] ({hops} hop{}) {text}",
                &from_prefix_hex[..from_prefix_hex.len().min(8)],
                if *hops == 1 { "" } else { "s" }
            ),
            YELLOW,
        ),
        MeshEventKind::ChannelMessage {
            channel,
            hops,
            text,
        } => (
            "📢",
            format!(
                "Channel {channel} ({hops} hop{}): {text}",
                if *hops == 1 { "" } else { "s" }
            ),
            YELLOW,
        ),
        MeshEventKind::MessageSent => ("📤", "Message sent".to_string(), Color::Blue),
        MeshEventKind::PathUpdate {
            prefix_hex,
            hops,
            path_hex,
        } => (
            "🗺️",
            format!(
                "Path update for [{}]: {hops} hop(s) via {}",
                &prefix_hex[..prefix_hex.len().min(8)],
                if path_hex.is_empty() {
                    "flood"
                } else {
                    path_hex
                }
            ),
            CYAN,
        ),
        MeshEventKind::Ack { tag_hex } => ("✅", format!("Ack received [{tag_hex}]"), GREEN),
        MeshEventKind::ManagedRepeaterDeclared { name } => (
            "🛰️",
            format!("Declared managed repeater to the node: {name}"),
            GREEN,
        ),
        MeshEventKind::RepeaterHeard { name, prefix_hex } => (
            "🔍",
            format!(
                "Repeater heard: {name} [{}] — not yet registered",
                &prefix_hex[..prefix_hex.len().min(8)]
            ),
            YELLOW,
        ),
        MeshEventKind::ContactRemoved { name, prefix_hex } => (
            "🗑️",
            format!(
                "Contact removed: {name} [{}]",
                &prefix_hex[..prefix_hex.len().min(8)]
            ),
            RED,
        ),
        MeshEventKind::Other { label } => ("🔎", label.clone(), MUTED),
    };

    Line::from(vec![
        Span::styled(format!("{time} "), Style::default().fg(MUTED)),
        Span::raw(format!("{emoji} ")),
        Span::styled(text, Style::default().fg(color)),
    ])
}

/// Color associated with a decoded payload type, for quick visual scanning.
fn payload_type_color(payload_type: &str) -> Color {
    match payload_type {
        "Advert" => CYAN,
        "TextMsg" | "GroupText" => YELLOW,
        "Ack" => GREEN,
        "Trace" | "Path" => MAGENTA,
        _ => MUTED,
    }
}

/// One-line, human-readable summary of a packet's content, tailored to its
/// payload type since different types carry very different data.
fn packet_summary(entry: &PacketLogEntry) -> String {
    let Some(header) = &entry.header else {
        return format!("undecodable header ({} byte payload)", entry.payload_len);
    };

    if let Some(adv) = &header.advertisement {
        let name = adv.name.as_deref().unwrap_or("(unnamed)");
        let pos = format_coords(adv.lat.unwrap_or(0.0), adv.lon.unwrap_or(0.0));
        return format!("{} \"{name}\" @ {pos}", adv.adv_type_name);
    }

    match header.payload_type.as_str() {
        "Ack" => "acknowledgement".to_string(),
        "TextMsg" | "GroupText" => format!("{} bytes of (encrypted) text", entry.payload_len),
        _ => format!("{} bytes payload", entry.payload_len),
    }
}

fn format_time_short(unix: i64) -> String {
    Local
        .timestamp_opt(unix, 0)
        .single()
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "--:--:--".to_string())
}

fn format_time_full(unix: i64) -> String {
    Local
        .timestamp_opt(unix, 0)
        .single()
        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Hop count across a group's receptions: a single value when every
/// repeater reported the same, otherwise the observed range — itself a
/// useful signal (the packet traveled different distances to reach us).
fn hops_range(group: &PacketGroup) -> String {
    let hops: Vec<u8> = group
        .members
        .iter()
        .filter_map(|m| m.header.as_ref().map(|h| h.hops))
        .collect();

    match (hops.iter().min(), hops.iter().max()) {
        (Some(min), Some(max)) if min == max => min.to_string(),
        (Some(min), Some(max)) => format!("{min}-{max}"),
        _ => "-".to_string(),
    }
}

/// `dest_hash_hex`/`src_hash_hex` as displayed in the packet log: the hex
/// hash, or a dash for payload types that don't carry per-node addressing
/// (see `PacketHeaderInfo::dest_hash_hex`).
fn hash_or_dash(hash_hex: &Option<String>) -> String {
    hash_hex.clone().unwrap_or_else(|| "-".to_string())
}

fn packet_group_row(group: &PacketGroup) -> Row<'static> {
    let latest = group.latest();
    let time = format_time_short(latest.at_unix);

    let (route, ptype, color, dest, src) = match &latest.header {
        Some(h) => (
            h.route_type.clone(),
            h.payload_type.clone(),
            payload_type_color(&h.payload_type),
            hash_or_dash(&h.dest_hash_hex),
            hash_or_dash(&h.src_hash_hex),
        ),
        None => (
            "?".to_string(),
            "?".to_string(),
            MUTED,
            "-".to_string(),
            "-".to_string(),
        ),
    };

    let count_cell = if group.count() > 1 {
        Cell::from(Span::styled(
            format!("×{}", group.count()),
            Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
        ))
    } else {
        Cell::from(Span::styled("·", Style::default().fg(MUTED)))
    };

    Row::new(vec![
        Cell::from(time),
        Cell::from(format!("{:.1}/{}", latest.snr, latest.rssi)),
        Cell::from(route),
        Cell::from(Span::styled(ptype, Style::default().fg(color))),
        Cell::from(src),
        Cell::from(dest),
        Cell::from(hops_range(group)),
        count_cell,
        Cell::from(packet_summary(latest)),
    ])
}

fn draw_packet_log_page(frame: &mut Frame, app: &mut App, area: Rect) {
    let groups = app.packet_groups();
    let group_count = groups.len();
    let reception_count: usize = groups.iter().map(PacketGroup::count).sum();
    let rows: Vec<Row> = groups.iter().map(packet_group_row).collect();

    let header = Row::new(vec![
        Cell::from("Time"),
        Cell::from("SNR/RSSI"),
        Cell::from("Route"),
        Cell::from("Type"),
        Cell::from("Src"),
        Cell::from("Dst"),
        Cell::from("Hops"),
        Cell::from("×"),
        Cell::from("Summary"),
    ])
    .style(Style::default().fg(CYAN).add_modifier(Modifier::BOLD));

    let lock_suffix = if app.locked_view.is_some() {
        format!(" — 🔒 LOCKED (+{} new)", app.new_packets_since_lock())
    } else {
        " — ▶ LIVE".to_string()
    };

    let title = if reception_count > group_count {
        format!(
            "📦 Raw packet log ({group_count} packets, {reception_count} receptions){lock_suffix}"
        )
    } else {
        format!("📦 Raw packet log ({group_count}){lock_suffix}")
    };

    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Length(9),
            Constraint::Length(16),
            Constraint::Length(10),
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Length(7),
            Constraint::Length(4),
            Constraint::Min(20),
        ],
    )
    .header(header)
    .row_highlight_style(
        Style::default()
            .bg(Color::Rgb(0x2a, 0x2e, 0x3a))
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("➤ ")
    .block(block(title));

    frame.render_stateful_widget(table, area, &mut app.packet_table_state);
}

/// Rect centered in `area`, sized to `percent_x` × `percent_y` of it.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn section_title(text: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(
        text.into(),
        Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
    ))
}

/// One line of the per-reception breakdown: the fields that genuinely
/// differ between repeaters hearing the same packet (signal quality,
/// route, hop count, path) — as opposed to the fields common to the whole
/// group (payload type/version/content), shown once above.
fn reception_line(m: &PacketLogEntry) -> Line<'static> {
    let (route, hops, path) = match &m.header {
        Some(h) => (
            h.route_type.clone(),
            h.hops.to_string(),
            if h.path_hex.is_empty() {
                "flood".to_string()
            } else {
                h.path_hex.clone()
            },
        ),
        None => ("?".to_string(), "-".to_string(), "-".to_string()),
    };
    let snr_rssi = format!("{:.1}dB/{}dBm", m.snr, m.rssi);

    Line::from(vec![
        Span::styled(
            format!("   {:<9}", format_time_short(m.at_unix)),
            Style::default().fg(MUTED),
        ),
        Span::raw(format!("{snr_rssi:<16}")),
        Span::styled(format!("{route:<16}"), Style::default().fg(MUTED)),
        Span::raw(format!("{:<5}", format!("{hops}h"))),
        Span::styled(path, Style::default().fg(MUTED)),
    ])
}

fn reception_header_line() -> Line<'static> {
    Line::from(Span::styled(
        format!(
            "   {:<9}{:<16}{:<16}{:<5}{}",
            "Time", "SNR/RSSI", "Route", "Hop", "Path"
        ),
        Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
    ))
}

/// Popup with the full detail of a packet group, laid out differently
/// depending on its payload type since e.g. an advert carries
/// identity/position data that a plain data packet doesn't. Fields common
/// to every reception (payload type/version/content) are shown once; the
/// per-reception breakdown below covers what genuinely differs between the
/// repeaters that relayed it (signal quality, route, hop count, path).
fn draw_packet_detail_popup(frame: &mut Frame, group: &PacketGroup) {
    let area = centered_rect(76, 80, frame.area());
    frame.render_widget(Clear, area);

    let latest = group.latest();

    let mut lines = vec![field_line(
        "🕒 Last heard",
        format_time_full(latest.at_unix),
    )];
    if group.count() > 1 {
        let first = group.members.last().expect("group is never empty");
        lines.push(field_line(
            "🔁 Receptions",
            format!(
                "{} repeaters/paths, first heard {}",
                group.count(),
                format_time_full(first.at_unix)
            ),
        ));
    }

    match &latest.header {
        Some(h) => {
            lines.push(field_line("📦 Payload type", h.payload_type.clone()));
            lines.push(field_line(
                "🔢 Payload version",
                h.payload_version.to_string(),
            ));
            if let (Some(src), Some(dest)) = (&h.src_hash_hex, &h.dest_hash_hex) {
                lines.push(field_line("📤 Source", src.clone()));
                lines.push(field_line("🎯 Destination", dest.clone()));
            }

            match &h.advertisement {
                Some(adv) => {
                    lines.push(Line::from(""));
                    lines.push(section_title("— Advertised identity —"));
                    lines.push(field_line(
                        "🏷️  Name",
                        adv.name.clone().unwrap_or_else(|| "(unnamed)".to_string()),
                    ));
                    lines.push(field_line("🔖 Advertiser type", adv.adv_type_name.clone()));
                    lines.push(field_line("🔑 Public key", adv.public_key_hex.clone()));
                    lines.push(field_line(
                        "🌍 Position",
                        format_coords(adv.lat.unwrap_or(0.0), adv.lon.unwrap_or(0.0)),
                    ));
                }
                None => {
                    lines.push(Line::from(""));
                    lines.push(field_line(
                        "📨 Payload",
                        format!(
                            "{} bytes: {}",
                            latest.payload_len,
                            truncate_hex(&latest.payload_hex)
                        ),
                    ));
                }
            }
        }
        None => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Header could not be decoded.",
                Style::default().fg(YELLOW),
            )));
            lines.push(field_line(
                "📨 Raw payload",
                format!(
                    "{} bytes: {}",
                    latest.payload_len,
                    truncate_hex(&latest.payload_hex)
                ),
            ));
        }
    }

    lines.push(Line::from(""));
    lines.push(section_title(format!("— Receptions ({}) —", group.count())));
    lines.push(reception_header_line());
    const MAX_SHOWN_RECEPTIONS: usize = 12;
    for member in group.members.iter().take(MAX_SHOWN_RECEPTIONS) {
        lines.push(reception_line(member));
    }
    if group.count() > MAX_SHOWN_RECEPTIONS {
        lines.push(Line::from(Span::styled(
            format!("   … {} more", group.count() - MAX_SHOWN_RECEPTIONS),
            Style::default().fg(MUTED),
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "[Enter/Esc] Close",
        Style::default().fg(MUTED),
    )));

    let popup = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(block("🔍 Packet detail"));
    frame.render_widget(popup, area);
}

/// Content of the F1 help popup: a short header describing the page, plus
/// its keyboard shortcuts. Kept per-page since Dashboard and Packet log
/// don't share the same controls.
fn help_content(
    page: Page,
) -> (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
) {
    match page {
        Page::Dashboard => (
            "Dashboard",
            "Local node status, mesh contacts, and the live event log.",
            &[
                ("F1", "Toggle this help"),
                ("F2", "Dashboard page (current)"),
                ("F3", "Packet log page"),
                ("↑ / ↓", "Select a contact"),
                ("r", "Refresh snapshot"),
                ("m", "Toggle managed repeater"),
                ("d", "Delete contact (press twice to confirm)"),
                ("q / Esc", "Quit"),
            ],
        ),
        Page::PacketLog => (
            "Packet log",
            "Raw RF packets captured by the daemon (meshcore-rs LogData events).",
            &[
                ("F1", "Toggle this help"),
                ("F2", "Dashboard page"),
                ("F3", "Packet log page (current)"),
                ("↑ / ↓", "Select a packet (repeated relays are grouped)"),
                ("l", "Toggle scroll lock"),
                ("Enter", "Open / close packet detail"),
                ("Esc", "Close popup"),
                ("q", "Quit"),
            ],
        ),
    }
}

fn draw_help_popup(frame: &mut Frame, page: Page) {
    let area = centered_rect(60, 60, frame.area());
    frame.render_widget(Clear, area);

    let (page_name, header, shortcuts) = help_content(page);

    let mut lines = vec![
        Line::from(Span::styled(header, Style::default().fg(Color::White))),
        Line::from(""),
        Line::from(Span::styled(
            "Keyboard shortcuts",
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        )),
    ];
    for (key, desc) in shortcuts {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {key:<10}"),
                Style::default().fg(YELLOW).add_modifier(Modifier::BOLD),
            ),
            Span::styled(*desc, Style::default().fg(Color::White)),
        ]));
    }

    let popup = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(block(format!("❓ Help — {page_name}")));
    frame.render_widget(popup, area);
}

fn truncate_hex(hex: &str) -> String {
    const MAX: usize = 96;
    if hex.len() > MAX {
        format!("{}…", &hex[..MAX])
    } else {
        hex.to_string()
    }
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    if let Some((_, name)) = &app.pending_delete {
        let warning = Line::from(vec![Span::styled(
            format!(
                "  ⚠️  Press [d] again to permanently remove \"{name}\" from the node, any other key cancels"
            ),
            Style::default().fg(RED).add_modifier(Modifier::BOLD),
        )]);
        frame.render_widget(Paragraph::new(warning), area);
        return;
    }

    if let Some(status) = &app.last_status {
        let line = Line::from(vec![Span::styled(
            format!("  {status}"),
            Style::default().fg(CYAN),
        )]);
        frame.render_widget(Paragraph::new(line), area);
        return;
    }

    let daemon_dot = if app.daemon_connected {
        ("🟢", "daemon connected", GREEN)
    } else {
        ("🔴", "daemon unreachable", RED)
    };
    let mesh_dot = if app.snapshot.mesh_connected {
        ("🟢", "mesh connected", GREEN)
    } else {
        ("🟠", "mesh disconnected", YELLOW)
    };

    let line = Line::from(vec![
        Span::raw(format!(" {} ", daemon_dot.0)),
        Span::styled(daemon_dot.1, Style::default().fg(daemon_dot.2)),
        Span::raw("   "),
        Span::raw(format!("{} ", mesh_dot.0)),
        Span::styled(mesh_dot.1, Style::default().fg(mesh_dot.2)),
        Span::raw(format!("   ⏳ {}s", app.snapshot.uptime_secs)),
    ]);

    frame.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use fez_mesh_controller_core::mesh::PacketHeaderInfo;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// Three receptions: two relays of the same text message (identical
    /// payload, a few seconds apart) and one unrelated Ack.
    fn sample_entries() -> Vec<PacketLogEntry> {
        vec![
            PacketLogEntry {
                id: 3,
                at_unix: 20,
                snr: 5.5,
                rssi: -80,
                header: Some(PacketHeaderInfo {
                    route_type: "Flood".to_string(),
                    payload_type: "TextMsg".to_string(),
                    payload_version: 0,
                    hops: 2,
                    path_hash_size: 1,
                    path_hex: "aabb".to_string(),
                    transport_code_hex: None,
                    dest_hash_hex: Some("de".to_string()),
                    src_hash_hex: Some("ad".to_string()),
                    advertisement: None,
                }),
                payload_hex: "deadbeef".to_string(),
                payload_len: 4,
            },
            PacketLogEntry {
                id: 2,
                at_unix: 10,
                snr: 3.0,
                rssi: -95,
                header: Some(PacketHeaderInfo {
                    route_type: "Flood".to_string(),
                    payload_type: "TextMsg".to_string(),
                    payload_version: 0,
                    hops: 1,
                    path_hash_size: 1,
                    path_hex: "cc".to_string(),
                    transport_code_hex: None,
                    dest_hash_hex: Some("de".to_string()),
                    src_hash_hex: Some("ad".to_string()),
                    advertisement: None,
                }),
                payload_hex: "deadbeef".to_string(),
                payload_len: 4,
            },
            PacketLogEntry {
                id: 1,
                at_unix: 0,
                snr: 1.0,
                rssi: -100,
                header: Some(PacketHeaderInfo {
                    route_type: "Direct".to_string(),
                    payload_type: "Ack".to_string(),
                    payload_version: 0,
                    hops: 0,
                    path_hash_size: 1,
                    path_hex: String::new(),
                    transport_code_hex: None,
                    dest_hash_hex: None,
                    src_hash_hex: None,
                    advertisement: None,
                }),
                payload_hex: "01020304".to_string(),
                payload_len: 4,
            },
        ]
    }

    fn render(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn packet_log_page_groups_repeated_relays_in_the_table() {
        let mut app = App::new();
        app.page = Page::PacketLog;
        app.set_packet_log(sample_entries());
        app.packet_table_state.select(Some(0));

        let text = render(&mut app, 120, 20);

        // Two distinct packets (the grouped text message + the ack), three
        // raw receptions total.
        assert!(text.contains("2 packets, 3 receptions"));
        assert!(text.contains("×2"));
        assert!(text.contains("TextMsg"));
        assert!(text.contains("Ack"));
    }

    #[test]
    fn packet_log_table_shows_source_and_destination_hash_columns() {
        let mut app = App::new();
        app.page = Page::PacketLog;
        app.set_packet_log(sample_entries());
        app.packet_table_state.select(Some(0));

        let text = render(&mut app, 120, 20);

        assert!(text.contains("Src"));
        assert!(text.contains("Dst"));
        // The grouped TextMsg row shows its dest/src hashes...
        assert!(text.contains("de"));
        assert!(text.contains("ad"));
        // ...while the Ack row (no per-node addressing) shows a dash.
        assert!(text.contains("-"));
    }

    #[test]
    fn packet_detail_popup_shows_source_and_destination_when_present() {
        let mut app = App::new();
        app.page = Page::PacketLog;
        app.set_packet_log(sample_entries());
        app.packet_table_state.select(Some(0)); // the grouped TextMsg (newest)
        app.open_packet_detail();

        let text = render(&mut app, 140, 40);

        assert!(text.contains("Source"));
        assert!(text.contains("Destination"));
    }

    #[test]
    fn packet_detail_popup_shows_common_fields_once_and_per_reception_breakdown() {
        let mut app = App::new();
        app.page = Page::PacketLog;
        app.set_packet_log(sample_entries());
        app.packet_table_state.select(Some(0)); // the grouped TextMsg (newest)
        app.open_packet_detail();

        let text = render(&mut app, 140, 40);

        assert!(text.contains("Receptions (2)"));
        assert!(text.contains("Payload type"));
        // Per-reception rows: the two relays have different hop counts.
        assert!(text.contains("1h"));
        assert!(text.contains("2h"));
    }

    #[test]
    fn packet_detail_popup_stays_on_the_selected_packet_after_new_packets_arrive() {
        let mut app = App::new();
        app.page = Page::PacketLog;
        app.set_packet_log(sample_entries());
        app.packet_table_state.select(Some(0)); // the grouped TextMsg (newest)
        app.open_packet_detail();

        // A brand new packet arrives and is inserted at the front of the
        // live list, shifting every existing entry's index — before the
        // fix, this made the popup (which re-resolved the selection by
        // index on every frame) silently swap to showing this new packet
        // instead of staying on the one the user opened.
        app.push_packet(PacketLogEntry {
            id: 4,
            at_unix: 30,
            snr: 2.0,
            rssi: -90,
            header: Some(PacketHeaderInfo {
                route_type: "Direct".to_string(),
                payload_type: "Battery".to_string(),
                payload_version: 0,
                hops: 0,
                path_hash_size: 1,
                path_hex: String::new(),
                transport_code_hex: None,
                dest_hash_hex: None,
                src_hash_hex: None,
                advertisement: None,
            }),
            payload_hex: "ff".to_string(),
            payload_len: 1,
        });

        let text = render(&mut app, 140, 40);

        assert!(text.contains("Receptions (2)"));
        assert!(text.contains("1h"));
        assert!(text.contains("2h"));
    }

    #[test]
    fn packet_log_page_renders_without_grouping_when_singleton() {
        let mut app = App::new();
        app.page = Page::PacketLog;
        app.set_packet_log(vec![sample_entries().pop().unwrap()]);

        let text = render(&mut app, 120, 20);

        assert!(text.contains("Raw packet log (1)"));
        // A singleton group shows the "·" placeholder in the count
        // column, not a "×N" badge (the "×" that does appear is just the
        // column header).
        assert!(text.contains('·'));
        assert!(!text.contains("×1"));
        assert!(!text.contains("×2"));
    }
}
