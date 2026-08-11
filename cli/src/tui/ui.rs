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
use fez_mesh_controller_core::mesh::MeshEventKind;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Cell, List, ListItem, Paragraph, Row, Table};
use ratatui::Frame;

use crate::format::{format_coords, format_last_seen};
use crate::tui::app::App;

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

    draw_title(frame, root[0]);
    draw_body(frame, app, root[1]);
    draw_footer(frame, app, root[2]);
}

fn draw_title(frame: &mut Frame, area: Rect) {
    let title = Paragraph::new(Line::from(vec![
        Span::raw("📡 "),
        Span::styled(
            "fez-mesh-controller",
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  —  🕸️  "),
        Span::styled("MeshCore Network Monitor", Style::default().fg(MUTED)),
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
        Span::styled(
            "   [q] Quit  [r] Refresh  [↑/↓] Select  [m] Toggle managed  [d] Delete contact",
            Style::default().fg(MUTED),
        ),
    ]);

    frame.render_widget(Paragraph::new(line), area);
}
