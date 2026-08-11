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

    if app.page == Page::PacketLog && app.packet_detail_open {
        if let Some(packet) = app.selected_packet() {
            draw_packet_detail_popup(frame, packet);
        }
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
        MeshEventKind::RfLog { snr, rssi } => (
            "📡",
            format!("RF packet: SNR {snr:.1} dB, RSSI {rssi} dBm"),
            MUTED,
        ),
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

fn packet_row(entry: &PacketLogEntry) -> Row<'static> {
    let time = Local
        .timestamp_opt(entry.at_unix, 0)
        .single()
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "--:--:--".to_string());

    let (route, ptype, hops, color) = match &entry.header {
        Some(h) => (
            h.route_type.clone(),
            h.payload_type.clone(),
            h.hops.to_string(),
            payload_type_color(&h.payload_type),
        ),
        None => ("?".to_string(), "?".to_string(), "-".to_string(), MUTED),
    };

    Row::new(vec![
        Cell::from(time),
        Cell::from(format!("{:.1}/{}", entry.snr, entry.rssi)),
        Cell::from(route),
        Cell::from(Span::styled(ptype, Style::default().fg(color))),
        Cell::from(hops),
        Cell::from(packet_summary(entry)),
    ])
}

fn draw_packet_log_page(frame: &mut Frame, app: &mut App, area: Rect) {
    let packets = app.visible_packets();
    let count = packets.len();
    let rows: Vec<Row> = packets.iter().map(packet_row).collect();

    let header = Row::new(vec![
        Cell::from("Time"),
        Cell::from("SNR/RSSI"),
        Cell::from("Route"),
        Cell::from("Type"),
        Cell::from("Hops"),
        Cell::from("Summary"),
    ])
    .style(Style::default().fg(CYAN).add_modifier(Modifier::BOLD));

    let lock_suffix = if app.locked_view.is_some() {
        format!(" — 🔒 LOCKED (+{} new)", app.new_packets_since_lock())
    } else {
        " — ▶ LIVE".to_string()
    };

    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Length(9),
            Constraint::Length(16),
            Constraint::Length(10),
            Constraint::Length(5),
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
    .block(block(format!("📦 Raw packet log ({count}){lock_suffix}")));

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

/// Popup with the full detail of a packet, laid out differently depending
/// on its payload type since e.g. an advert carries identity/position data
/// that a plain data packet doesn't.
fn draw_packet_detail_popup(frame: &mut Frame, entry: &PacketLogEntry) {
    let area = centered_rect(70, 70, frame.area());
    frame.render_widget(Clear, area);

    let time = Local
        .timestamp_opt(entry.at_unix, 0)
        .single()
        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let mut lines = vec![
        field_line("🆔 Packet #", entry.id.to_string()),
        field_line("🕒 Time", time),
        field_line(
            "📶 SNR / RSSI",
            format!("{:.1} dB / {} dBm", entry.snr, entry.rssi),
        ),
    ];

    match &entry.header {
        Some(h) => {
            lines.push(field_line("🧭 Route type", h.route_type.clone()));
            lines.push(field_line("📦 Payload type", h.payload_type.clone()));
            lines.push(field_line(
                "🔢 Payload version",
                h.payload_version.to_string(),
            ));
            lines.push(field_line("🦘 Hops", h.hops.to_string()));
            if h.path_hash_size > 0 || !h.path_hex.is_empty() {
                lines.push(field_line(
                    "🗺️  Path",
                    if h.path_hex.is_empty() {
                        "flood".to_string()
                    } else {
                        format!("{} (hash size {})", h.path_hex, h.path_hash_size)
                    },
                ));
            }
            if let Some(tc) = &h.transport_code_hex {
                lines.push(field_line("🚚 Transport code", tc.clone()));
            }

            match &h.advertisement {
                Some(adv) => {
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        "— Advertised identity —",
                        Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
                    )));
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
                            entry.payload_len,
                            truncate_hex(&entry.payload_hex)
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
                    entry.payload_len,
                    truncate_hex(&entry.payload_hex)
                ),
            ));
        }
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

fn truncate_hex(hex: &str) -> String {
    const MAX: usize = 96;
    if hex.len() > MAX {
        format!("{}…", &hex[..MAX])
    } else {
        hex.to_string()
    }
}

fn footer_key_hints(app: &App) -> &'static str {
    match app.page {
        Page::Dashboard => {
            "   [q] Quit  [F2/F3] Page  [r] Refresh  [↑/↓] Select  [m] Toggle managed  [d] Delete contact"
        }
        Page::PacketLog => {
            "   [q] Quit  [F2/F3] Page  [↑/↓] Select  [l] Scroll lock  [Enter] Details"
        }
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
        Span::styled(footer_key_hints(app), Style::default().fg(MUTED)),
    ]);

    frame.render_widget(Paragraph::new(line), area);
}
