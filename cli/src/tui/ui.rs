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
use fez_mesh_controller_core::channel;
use fez_mesh_controller_core::ipc::{MeshEvent, MqttBrokerStatus, MqttBrokerStatusDto};
use fez_mesh_controller_core::mesh::{
    ContactDto, ControlPayloadInfo, MeshEventKind, PacketHeaderInfo, PacketLogEntry,
};
use fez_mesh_controller_core::region;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table, Wrap,
};
use ratatui::Frame;

use crate::format::{format_coords, format_last_seen, strip_flag_emoji};
use crate::tui::app::{App, Page};
use crate::tui::packet_group::{path_hop_hashes, PacketGroup};

const CYAN: Color = Color::Rgb(0x4d, 0xd0, 0xe1);
const MAGENTA: Color = Color::Rgb(0xe0, 0x67, 0xf2);
const GREEN: Color = Color::Rgb(0x66, 0xe3, 0x8a);
const RED: Color = Color::Rgb(0xf2, 0x5c, 0x5c);
const YELLOW: Color = Color::Rgb(0xf2, 0xc9, 0x4c);
const MUTED: Color = Color::Rgb(0x7a, 0x84, 0x94);
/// Packet log row background: this packet's source or destination is a
/// managed repeater.
const HL_ENDPOINT_MANAGED_BG: Color = Color::Rgb(0x8a, 0x7a, 0x2a);
/// Packet log row background: this packet was relayed by a managed
/// repeater (but doesn't itself address one) — darker/brown so it reads as
/// a lower-priority signal than [`HL_ENDPOINT_MANAGED_BG`].
const HL_RELAYED_BY_MANAGED_BG: Color = Color::Rgb(0x4a, 0x38, 0x12);

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
            draw_packet_detail_popup(frame, group, &app.region_keys, &app.channel_keys);
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

    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(cols[0]);

    draw_cluster_block(frame, app, left[0]);
    draw_self_info(frame, app, left[1]);

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(cols[1]);

    draw_repeaters(frame, app, right[0]);
    draw_events(frame, app, right[1]);
}

fn draw_self_info(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = match &app.snapshot.self_info {
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

    if !app.snapshot.mqtt_brokers.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "MQTT brokers:",
            Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
        )));
        lines.extend(app.snapshot.mqtt_brokers.iter().map(mqtt_broker_line));
    }

    frame.render_widget(
        Paragraph::new(lines).block(block("🛰️  Observer node")),
        area,
    );
}

/// One line for a configured MQTT broker in the Observer node block: its
/// name plus a colored icon/label for its live connection status (see
/// [`fez_mesh_controller_core::ipc::MqttBrokerStatus`]).
fn mqtt_broker_line(broker: &MqttBrokerStatusDto) -> Line<'static> {
    let (icon, text, color) = match &broker.status {
        MqttBrokerStatus::Connecting => ("🟡", "Connecting".to_string(), YELLOW),
        MqttBrokerStatus::Connected => ("🟢", "Connected".to_string(), GREEN),
        MqttBrokerStatus::Disconnected => ("🔴", "Disconnected".to_string(), MUTED),
        MqttBrokerStatus::Error { reason } => ("⚠️", format!("Error: {reason}"), RED),
    };
    Line::from(vec![
        Span::raw(format!("  {icon} {}: ", broker.name)),
        Span::styled(text, Style::default().fg(color)),
    ])
}

/// The cluster's configured region hierarchy (see
/// `fez_mesh_controller_core::config::RegionConfig`) — for now, just
/// renders the tree, indented by depth. Local to this controller's own
/// config, unrelated to the connected node's own settings.
fn draw_cluster_block(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines = vec![Line::from(Span::styled(
        "🗺️ Regions:",
        Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
    ))];

    if app.snapshot.regions.is_empty() {
        lines.push(Line::from(Span::styled(
            "No regions configured",
            Style::default().fg(MUTED),
        )));
    } else {
        lines.extend(
            region::flatten_region_tree(&app.snapshot.regions)
                .into_iter()
                .map(|(depth, region)| {
                    Line::from(Span::raw(format!(
                        "{}{}",
                        "  ".repeat(depth + 1),
                        region.name
                    )))
                }),
        );
    }

    frame.render_widget(Paragraph::new(lines).block(block("🧩 Cluster")), area);
}

fn field_line(label: &str, value: impl Into<String>) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<16}"), Style::default().fg(MUTED)),
        Span::styled(value.into(), Style::default().fg(Color::White)),
    ])
}

fn draw_repeaters(frame: &mut Frame, app: &mut App, area: Rect) {
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
        let name = strip_flag_emoji(&c.name);

        let name_cell = if c.managed {
            Cell::from(Line::from(vec![
                Span::raw("🛰️ "),
                Span::styled(
                    name,
                    Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
                ),
            ]))
        } else {
            Cell::from(name)
        };

        let (status_text, status_color) = match (c.registered, c.managed) {
            (_, true) => ("🛰️ Managed", GREEN),
            (true, false) => ("📟 Known", MUTED),
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
    .block(block(format!("👥 Repeaters ({})", contacts.len())));

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
        MeshEventKind::Advertisement { name, .. } => (
            "📶",
            format!("Advertisement received from {}", strip_flag_emoji(name)),
            CYAN,
        ),
        MeshEventKind::NewContact { name } => (
            "🆕",
            format!("New contact: {}", strip_flag_emoji(name)),
            MAGENTA,
        ),
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
            format!(
                "Declared managed repeater to the node: {}",
                strip_flag_emoji(name)
            ),
            GREEN,
        ),
        MeshEventKind::ObserverNodeConfigEnforced { detail } => (
            "🔒",
            format!("Observer node config enforced: {detail}"),
            GREEN,
        ),
        MeshEventKind::RepeaterHeard { name, prefix_hex } => (
            "🔍",
            format!(
                "Repeater heard: {} [{}] — not yet registered",
                strip_flag_emoji(name),
                &prefix_hex[..prefix_hex.len().min(8)]
            ),
            YELLOW,
        ),
        MeshEventKind::ContactRemoved { name, prefix_hex } => (
            "🗑️",
            format!(
                "Contact removed: {} [{}]",
                strip_flag_emoji(name),
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

/// Plain-language explanation of what a payload type is used for, shown in
/// the packet detail popup — the type name alone (`TextMsg`, `Path`, ...)
/// isn't self-explanatory to someone not already familiar with the MeshCore
/// protocol. Mirrors `meshcore_rs::packets::PayloadType`'s variants.
fn payload_type_description(payload_type: &str) -> &'static str {
    match payload_type {
        "Req" => {
            "A directed request to a specific node (e.g. a remote status \
             query), addressed by its destination hash and routed back to \
             the sender via its source hash."
        }
        "Response" => "A reply to a Req or an anonymous request (AnonReq).",
        "TextMsg" => {
            "A direct, end-to-end encrypted text message between two \
             specific nodes."
        }
        "Ack" => "Acknowledgement confirming a message reached its destination.",
        "Advert" => {
            "A node broadcasting its identity (public key, and optionally \
             name/position) to the whole mesh, so other nodes can discover \
             and route to it."
        }
        "GroupText" => {
            "A text message sent to a channel/group, addressed by a shared \
             channel hash rather than a specific node — anyone with the \
             channel's secret can read it."
        }
        "GroupData" => {
            "Structured (non-text) data sent to a channel/group, addressed \
             the same way as GroupText."
        }
        "AnonReq" => {
            "A request from a sender with no prior contact relationship \
             (e.g. an anonymous remote query), carrying the sender's full \
             public key instead of a source hash."
        }
        "Path" => {
            "The route back to a node, returned in response to a path \
             discovery so future packets can be sent to it directly \
             instead of flooding."
        }
        "Trace" => {
            "A diagnostic packet used to trace the hops (and their SNR) a \
             transmission takes across the mesh."
        }
        "Multipart" => "One fragment of a message too large to fit in a single packet.",
        "Control" => {
            "A low-level control packet (e.g. node discovery) used by the \
             mesh protocol itself, not by applications."
        }
        "RawCustom" => {
            "Custom application payload with its own encryption/format, \
             sent via SEND_RAW_DATA — not decoded by any of the standard \
             message types."
        }
        _ => "Unrecognized payload type; its purpose can't be determined.",
    }
}

/// One-line, human-readable summary of a packet's content, tailored to its
/// payload type since different types carry very different data.
fn packet_summary(entry: &PacketLogEntry, channel_keys: &[(String, [u8; 32], u8)]) -> String {
    let Some(header) = &entry.header else {
        return format!("undecodable header ({} byte payload)", entry.payload_len);
    };

    if let Some(adv) = &header.advertisement {
        let name = adv.name.as_deref().map(strip_flag_emoji);
        let name = name
            .as_deref()
            .filter(|n| !n.is_empty())
            .unwrap_or("(unnamed)");
        let pos = format_coords(adv.lat.unwrap_or(0.0), adv.lon.unwrap_or(0.0));
        return format!("{} \"{name}\" @ {pos}", adv.adv_type_name);
    }

    if let Some(decoded) = channel::decode_group_text(entry, channel_keys) {
        return format!("{}: {}", decoded.channel_name, decoded.text);
    }

    if let Some(key) = &header.anon_req_sender_public_key_hex {
        let short: String = key.chars().take(12).collect();
        return format!("AnonReq from {short}…");
    }

    if let Some(control) = &header.control {
        return control_summary(control);
    }

    match header.payload_type.as_str() {
        "Ack" => "acknowledgement".to_string(),
        "TextMsg" | "GroupText" => format!("{} bytes of (encrypted) text", entry.payload_len),
        _ => format!("{} bytes payload", entry.payload_len),
    }
}

/// One-line summary of a decoded `Control` payload (see
/// [`ControlPayloadInfo`]), for [`packet_summary`].
fn control_summary(control: &ControlPayloadInfo) -> String {
    match control {
        ControlPayloadInfo::DiscoverReq { type_filter, .. } => {
            format!("Discover request (type filter 0x{type_filter:02x})")
        }
        ControlPayloadInfo::DiscoverResp {
            node_type_name,
            snr,
            ..
        } => format!("Discover response: {node_type_name} @ {snr:.1} dB"),
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

/// `src_hash_hex` as displayed in the packet log: the hex hash, or a dash
/// for payload types that don't carry per-node addressing (see
/// `PacketHeaderInfo::dest_hash_hex`).
fn hash_or_dash(hash_hex: &Option<String>) -> String {
    hash_hex.clone().unwrap_or_else(|| "-".to_string())
}

/// The packet log table's Dst cell: a real node's destination hash (plain
/// text, as-is), a channel hash for `GroupText`/`GroupData` (wrapped in
/// braces and styled muted/italic so it can't be mistaken for a node hash
/// at a glance — see `PacketHeaderInfo::channel_hash_hex`), or a dash.
fn destination_cell(header: Option<&PacketHeaderInfo>) -> Cell<'static> {
    let Some(h) = header else {
        return Cell::from("-");
    };
    if let Some(dest) = &h.dest_hash_hex {
        return Cell::from(dest.clone());
    }
    if let Some(channel) = &h.channel_hash_hex {
        return Cell::from(Span::styled(
            format!("{{{channel}}}"),
            Style::default().fg(MUTED).add_modifier(Modifier::ITALIC),
        ));
    }
    Cell::from("-")
}

/// Row background for a packet group, flagging its involvement with a
/// managed repeater: being addressed to/from one takes priority over
/// merely being relayed by one, since it's the more specific signal (a
/// packet can be both — e.g. a managed repeater relaying a message to
/// another managed repeater — in which case the endpoint color wins).
fn packet_row_highlight(group: &PacketGroup, contacts: &[ContactDto]) -> Option<Color> {
    if group.endpoint_is_managed_repeater(contacts) {
        Some(HL_ENDPOINT_MANAGED_BG)
    } else if group.relayed_by_managed_repeater(contacts) {
        Some(HL_RELAYED_BY_MANAGED_BG)
    } else {
        None
    }
}

fn packet_group_row(
    group: &PacketGroup,
    contacts: &[ContactDto],
    channel_keys: &[(String, [u8; 32], u8)],
) -> Row<'static> {
    let latest = group.latest();
    let time = format_time_short(latest.at_unix);

    let (route, ptype, color, src) = match &latest.header {
        Some(h) => (
            h.route_type.clone(),
            h.payload_type.clone(),
            payload_type_color(&h.payload_type),
            hash_or_dash(&h.src_hash_hex),
        ),
        None => ("?".to_string(), "?".to_string(), MUTED, "-".to_string()),
    };
    let dest_cell = destination_cell(latest.header.as_ref());

    let count_cell = if group.count() > 1 {
        Cell::from(Span::styled(
            format!("×{}", group.count()),
            Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
        ))
    } else {
        Cell::from(Span::styled("·", Style::default().fg(MUTED)))
    };

    let mut row = Row::new(vec![
        Cell::from(time),
        Cell::from(format!("{:.1}/{}", latest.snr, latest.rssi)),
        Cell::from(route),
        Cell::from(Span::styled(ptype, Style::default().fg(color))),
        Cell::from(src),
        dest_cell,
        Cell::from(hops_range(group)),
        count_cell,
        Cell::from(packet_summary(latest, channel_keys)),
    ]);
    if let Some(bg) = packet_row_highlight(group, contacts) {
        row = row.style(Style::default().bg(bg));
    }
    row
}

fn draw_packet_log_page(frame: &mut Frame, app: &mut App, area: Rect) {
    let groups = app.packet_groups();
    let group_count = groups.len();
    let reception_count: usize = groups.iter().map(PacketGroup::count).sum();
    let contacts = &app.snapshot.contacts;
    let channel_keys = &app.channel_keys;
    let rows: Vec<Row> = groups
        .iter()
        .map(|g| packet_group_row(g, contacts, channel_keys))
        .collect();

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

/// Human-readable rendering of a packet's path: its individual repeater
/// hop hashes (see [`path_hop_hashes`]), separated by an arrow so they
/// read as a sequence of hops rather than one illegible run-together hex
/// blob — `"flood"` for an empty path, or the raw hex as a fallback if it
/// can't be split into whole hops (malformed/undecodable path).
fn format_path(path_hex: &str, path_hash_size: u8) -> String {
    if path_hex.is_empty() {
        return "flood".to_string();
    }
    let hops = path_hop_hashes(path_hex, path_hash_size);
    if hops.is_empty() {
        return path_hex.to_string();
    }
    hops.join(" → ")
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
            format_path(&h.path_hex, h.path_hash_size),
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

/// The packet detail popup's transport code line: the header's *first*
/// transport code half only (the second is reserved, never shown — see
/// `PacketHeaderInfo::transport_code_hex`). `None` when the packet has no
/// transport code at all (a route type that doesn't carry one).
///
/// A code of `{0, 0}` (*both* halves — the reservation only ever nudges a
/// real computed code away from an all-zero *first* half, so checking just
/// that half isn't enough to tell a genuine `{0, x}` apart from `{0, 0}`)
/// isn't a normal region-scoped code at all: it's the firmware's explicit
/// "Share" marker — `isShare()` in `examples/simple_repeater/MyMesh.cpp`
/// checks exactly this, with the literal comment `{ 0, 0 } means 'send
/// this nowhere'` (also `BaseChatMesh::shareContactZeroHop`). It's how a
/// node deliberately opts a packet out of flood/repeat propagation — e.g.
/// resharing a contact's advert to immediate neighbors only — not a
/// leftover/unset value. Shown as a distinct "Share (not repeated)" label
/// rather than attempting a region match, since it's not a real code.
///
/// Otherwise, green when it matches a configured region's precomputed
/// transport code for this exact packet, plain white if not.
fn transport_code_line(
    entry: &PacketLogEntry,
    region_keys: &[(String, [u8; 16])],
) -> Option<Line<'static>> {
    let code_hex = entry.header.as_ref()?.transport_code_hex.as_deref()?;
    if code_hex.len() < 8 {
        return None;
    }
    let first_half = &code_hex[..4];

    let value = if code_hex == "00000000" {
        Span::styled("Share (not repeated)", Style::default().fg(MUTED))
    } else {
        let color = if region::matching_region_name(entry, region_keys).is_some() {
            GREEN
        } else {
            Color::White
        };
        Span::styled(first_half.to_string(), Style::default().fg(color))
    };

    Some(Line::from(vec![
        Span::styled(
            format!("{:<16}", "🧭 Transport code"),
            Style::default().fg(MUTED),
        ),
        value,
    ]))
}

/// Popup with the full detail of a packet group, laid out differently
/// depending on its payload type since e.g. an advert carries
/// identity/position data that a plain data packet doesn't. Fields common
/// to every reception (payload type/version/content) are shown once; the
/// per-reception breakdown below covers what genuinely differs between the
/// repeaters that relayed it (signal quality, route, hop count, path).
fn draw_packet_detail_popup(
    frame: &mut Frame,
    group: &PacketGroup,
    region_keys: &[(String, [u8; 16])],
    channel_keys: &[(String, [u8; 32], u8)],
) {
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
            if let Some(transport_line) = transport_code_line(latest, region_keys) {
                lines.push(transport_line);
            }
            if let Some(dest) = &h.dest_hash_hex {
                lines.push(field_line("🎯 Destination", dest.clone()));
            } else if let Some(channel) = &h.channel_hash_hex {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{:<16}", "🎯 Destination"),
                        Style::default().fg(MUTED),
                    ),
                    Span::styled(
                        format!("{{{channel}}} (channel)"),
                        Style::default().fg(MUTED).add_modifier(Modifier::ITALIC),
                    ),
                ]));
            }
            if let Some(src) = &h.src_hash_hex {
                lines.push(field_line("📤 Source", src.clone()));
            }

            lines.push(Line::from(""));
            lines.push(section_title("— About this packet type —"));
            lines.push(Line::from(Span::styled(
                payload_type_description(&h.payload_type),
                Style::default().fg(Color::White),
            )));

            if let Some(adv) = &h.advertisement {
                lines.push(Line::from(""));
                lines.push(section_title("— Advertised identity —"));
                let name = adv.name.as_deref().map(strip_flag_emoji);
                lines.push(field_line(
                    "🏷️  Name",
                    name.filter(|n| !n.is_empty())
                        .unwrap_or_else(|| "(unnamed)".to_string()),
                ));
                lines.push(field_line("🔖 Advertiser type", adv.adv_type_name.clone()));
                lines.push(field_line("🔑 Public key", adv.public_key_hex.clone()));
                lines.push(field_line(
                    "🌍 Position",
                    format_coords(adv.lat.unwrap_or(0.0), adv.lon.unwrap_or(0.0)),
                ));
            } else if let Some(decoded) = channel::decode_group_text(latest, channel_keys) {
                lines.push(Line::from(""));
                lines.push(section_title("— Payload —"));
                lines.push(field_line("📡 Channel", decoded.channel_name));
                lines.push(field_line("💬 Message", decoded.text));
            } else if let Some(control) = &h.control {
                lines.push(Line::from(""));
                lines.push(section_title("— Payload —"));
                match control {
                    ControlPayloadInfo::DiscoverReq {
                        prefix_only,
                        type_filter,
                        tag_hex,
                        since_unix,
                    } => {
                        lines.push(field_line("🔍 Sub-type", "Discover request"));
                        lines.push(field_line(
                            "🏷️  Type filter",
                            format!("0x{type_filter:02x}"),
                        ));
                        lines.push(field_line("🔖 Tag", tag_hex.clone()));
                        lines.push(field_line(
                            "🕒 Since",
                            since_unix
                                .map(|t| format_time_full(t as i64))
                                .unwrap_or_else(|| "any".to_string()),
                        ));
                        lines.push(field_line(
                            "🔒 Prefix only",
                            if *prefix_only { "yes" } else { "no" },
                        ));
                    }
                    ControlPayloadInfo::DiscoverResp {
                        node_type_name,
                        snr,
                        tag_hex,
                        pubkey_hex,
                    } => {
                        lines.push(field_line("🔍 Sub-type", "Discover response"));
                        lines.push(field_line("🔖 Node type", node_type_name.clone()));
                        lines.push(field_line("📶 SNR", format!("{snr:.1} dB")));
                        lines.push(field_line("🔖 Tag", tag_hex.clone()));
                        lines.push(field_line("🔑 Public key", pubkey_hex.clone()));
                    }
                }
            } else {
                lines.push(Line::from(""));
                lines.push(section_title("— Payload —"));
                if let Some(sender_key) = &h.anon_req_sender_public_key_hex {
                    lines.push(field_line("🔑 Sender key", sender_key.clone()));
                }
                lines.push(field_line(
                    "📏 Size",
                    format!("{} bytes", latest.payload_len),
                ));
                lines.push(field_line("📨 Raw data", truncate_hex(&latest.payload_hex)));
            }
        }
        None => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Header could not be decoded.",
                Style::default().fg(YELLOW),
            )));
            lines.push(Line::from(""));
            lines.push(section_title("— Payload —"));
            lines.push(field_line(
                "📏 Size",
                format!("{} bytes", latest.payload_len),
            ));
            lines.push(field_line("📨 Raw data", truncate_hex(&latest.payload_hex)));
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
            "Observer node status, cluster regions, repeaters, and the live event log.",
            &[
                ("F1", "Toggle this help"),
                ("F2", "Dashboard page (current)"),
                ("F3", "Packet log page"),
                ("↑ / ↓", "Select a repeater"),
                ("r", "Refresh snapshot"),
                ("m", "Toggle managed repeater"),
                ("d", "Delete repeater (press twice to confirm)"),
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
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

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

    fn contact_of_type(public_key_prefix_hex: &str, contact_type: u8) -> ContactDto {
        ContactDto {
            contact_type,
            ..contact(public_key_prefix_hex, false)
        }
    }

    fn single_member_group(header: PacketHeaderInfo) -> PacketGroup {
        PacketGroup {
            members: vec![PacketLogEntry {
                id: 1,
                at_unix: 0,
                snr: 1.0,
                rssi: -90,
                header: Some(header),
                payload_hex: "aabb".to_string(),
                payload_len: 2,
            }],
        }
    }

    /// A real `GroupText` packet addressed to the well-known "Public"
    /// channel: `channel_hash(0x11) || mac || ciphertext`, cross-checked
    /// independently via the `openssl` CLI (AES-128-ECB) and Python's
    /// hmac/hashlib (see `core::channel::tests`) — decrypts to
    /// "Hello, mesh!".
    fn public_group_text_entry() -> PacketLogEntry {
        PacketLogEntry {
            id: 1,
            at_unix: 0,
            snr: 1.0,
            rssi: -90,
            header: Some(PacketHeaderInfo {
                route_type: "Flood".to_string(),
                route_type_raw: 1,
                payload_type: "GroupText".to_string(),
                payload_type_raw: 5,
                payload_version: 0,
                hops: 1,
                path_hash_size: 1,
                path_hex: String::new(),
                transport_code_hex: None,
                dest_hash_hex: None,
                src_hash_hex: None,
                channel_hash_hex: Some("11".to_string()),
                anon_req_sender_public_key_hex: None,
                control: None,
                advertisement: None,
            }),
            payload_hex: "11ece6f1d59f9d82b89139e8f514be20fe914ab3d762d29a9a22438cb69d8789f99ce3"
                .to_string(),
            payload_len: 34,
        }
    }

    // --- packet_row_highlight --------------------------------------------

    #[test]
    fn packet_row_highlight_prioritizes_endpoint_over_relay() {
        let group = single_member_group(PacketHeaderInfo {
            route_type: "Flood".to_string(),
            route_type_raw: 1,
            payload_type: "TextMsg".to_string(),
            payload_type_raw: 2,
            payload_version: 0,
            hops: 1,
            path_hash_size: 1,
            path_hex: "aa".to_string(),
            transport_code_hex: None,
            dest_hash_hex: Some("de".to_string()),
            src_hash_hex: None,
            channel_hash_hex: None,
            anon_req_sender_public_key_hex: None,
            control: None,
            advertisement: None,
        });
        // One contact matches the destination, another matches the relay
        // hop — the endpoint color must win.
        let contacts = vec![contact("deadbeefcafe", true), contact("aabbccddeeff", true)];

        assert_eq!(
            packet_row_highlight(&group, &contacts),
            Some(HL_ENDPOINT_MANAGED_BG)
        );
    }

    #[test]
    fn packet_row_highlight_relay_only() {
        let group = single_member_group(PacketHeaderInfo {
            route_type: "Flood".to_string(),
            route_type_raw: 1,
            payload_type: "TextMsg".to_string(),
            payload_type_raw: 2,
            payload_version: 0,
            hops: 1,
            path_hash_size: 1,
            path_hex: "aa".to_string(),
            transport_code_hex: None,
            dest_hash_hex: None,
            src_hash_hex: None,
            channel_hash_hex: None,
            anon_req_sender_public_key_hex: None,
            control: None,
            advertisement: None,
        });
        let contacts = vec![contact("aabbccddeeff", true)];

        assert_eq!(
            packet_row_highlight(&group, &contacts),
            Some(HL_RELAYED_BY_MANAGED_BG)
        );
    }

    #[test]
    fn packet_row_highlight_none_when_no_managed_repeater_involved() {
        let group = single_member_group(PacketHeaderInfo {
            route_type: "Flood".to_string(),
            route_type_raw: 1,
            payload_type: "TextMsg".to_string(),
            payload_type_raw: 2,
            payload_version: 0,
            hops: 1,
            path_hash_size: 1,
            path_hex: "aa".to_string(),
            transport_code_hex: None,
            dest_hash_hex: Some("de".to_string()),
            src_hash_hex: None,
            channel_hash_hex: None,
            anon_req_sender_public_key_hex: None,
            control: None,
            advertisement: None,
        });
        let contacts = vec![contact("112233445566", true)];

        assert_eq!(packet_row_highlight(&group, &contacts), None);
    }

    // --- format_path --------------------------------------------------------

    #[test]
    fn format_path_shows_flood_for_an_empty_path() {
        assert_eq!(format_path("", 1), "flood");
    }

    #[test]
    fn format_path_separates_hops_with_an_arrow() {
        assert_eq!(format_path("aabbcc", 1), "aa → bb → cc");
        assert_eq!(format_path("11223344", 2), "1122 → 3344");
    }

    #[test]
    fn format_path_falls_back_to_the_raw_hex_when_it_cannot_be_split_into_hops() {
        // Not evenly divisible by the hash size (2 bytes = 4 hex chars).
        assert_eq!(format_path("aabbcc", 2), "aabbcc");
    }

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
                    route_type_raw: 1,
                    payload_type: "TextMsg".to_string(),
                    payload_type_raw: 2,
                    payload_version: 0,
                    hops: 2,
                    path_hash_size: 1,
                    path_hex: "aabb".to_string(),
                    transport_code_hex: None,
                    dest_hash_hex: Some("de".to_string()),
                    src_hash_hex: Some("ad".to_string()),
                    channel_hash_hex: None,
                    anon_req_sender_public_key_hex: None,
                    control: None,
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
                    route_type_raw: 1,
                    payload_type: "TextMsg".to_string(),
                    payload_type_raw: 2,
                    payload_version: 0,
                    hops: 1,
                    path_hash_size: 1,
                    path_hex: "cc".to_string(),
                    transport_code_hex: None,
                    dest_hash_hex: Some("de".to_string()),
                    src_hash_hex: Some("ad".to_string()),
                    channel_hash_hex: None,
                    anon_req_sender_public_key_hex: None,
                    control: None,
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
                    route_type_raw: 2,
                    payload_type: "Ack".to_string(),
                    payload_type_raw: 3,
                    payload_version: 0,
                    hops: 0,
                    path_hash_size: 1,
                    path_hex: String::new(),
                    transport_code_hex: None,
                    dest_hash_hex: None,
                    src_hash_hex: None,
                    channel_hash_hex: None,
                    anon_req_sender_public_key_hex: None,
                    control: None,
                    advertisement: None,
                }),
                payload_hex: "01020304".to_string(),
                payload_len: 4,
            },
        ]
    }

    fn render(app: &mut App, width: u16, height: u16) -> String {
        let buffer = render_buffer(app, width, height);
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render_buffer(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// The foreground color of the cell where `text` starts in the
    /// rendered buffer (scanning left-to-right, top-to-bottom), or `None`
    /// if it isn't found. Used to assert on styling that plain
    /// string-contains checks can't see.
    fn fg_of_text(buffer: &ratatui::buffer::Buffer, text: &str) -> Option<Color> {
        let chars: Vec<String> = text.chars().map(|c| c.to_string()).collect();
        for y in 0..buffer.area.height {
            for x in 0..=buffer.area.width.saturating_sub(chars.len() as u16) {
                let matches = chars
                    .iter()
                    .enumerate()
                    .all(|(i, ch)| buffer[(x + i as u16, y)].symbol() == ch);
                if matches {
                    return Some(buffer[(x, y)].fg);
                }
            }
        }
        None
    }

    /// The row where `text` starts in the rendered buffer (scanning
    /// left-to-right, top-to-bottom), or `None` if it isn't found. Used to
    /// assert on the relative vertical ordering of two blocks.
    fn text_row(buffer: &ratatui::buffer::Buffer, text: &str) -> Option<u16> {
        let chars: Vec<String> = text.chars().map(|c| c.to_string()).collect();
        for y in 0..buffer.area.height {
            for x in 0..=buffer.area.width.saturating_sub(chars.len() as u16) {
                let matches = chars
                    .iter()
                    .enumerate()
                    .all(|(i, ch)| buffer[(x + i as u16, y)].symbol() == ch);
                if matches {
                    return Some(y);
                }
            }
        }
        None
    }

    // --- Event log -------------------------------------------------------

    #[test]
    fn event_log_strips_flag_emoji_from_remotely_supplied_names() {
        // Flag emoji (paired Regional Indicator Symbols) trigger a
        // ratatui rendering bug (ratatui/ratatui#75) that shifts
        // everything after them on the line — see `strip_flag_emoji`.
        // Node names are remotely supplied and can't be controlled, so
        // they're sanitized before display.
        let mut app = App::new();
        app.push_event(MeshEvent {
            at_unix: 0,
            kind: MeshEventKind::Advertisement {
                name: "🇨🇵30 Depot".to_string(),
                prefix_hex: "aabbccddeeff".to_string(),
                lat: 0.0,
                lon: 0.0,
            },
        });
        app.push_event(MeshEvent {
            at_unix: 0,
            kind: MeshEventKind::NewContact {
                name: "🇫🇷Repeater".to_string(),
            },
        });

        let text = render(&mut app, 140, 20);

        assert!(text.contains("Advertisement received from 30 Depot"));
        assert!(text.contains("New contact: Repeater"));
        // No leftover regional-indicator codepoints anywhere on screen.
        assert!(!text
            .chars()
            .any(|c| ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)));
    }

    // --- Dashboard layout ----------------------------------------------------

    #[test]
    fn dashboard_renames_local_node_to_observer_node() {
        let mut app = App::new();

        let text = render(&mut app, 120, 30);

        assert!(text.contains("Observer node"));
        assert!(!text.contains("Local node"));
    }

    #[test]
    fn observer_node_block_hides_mqtt_section_when_no_brokers_configured() {
        let mut app = App::new();

        let text = render(&mut app, 120, 30);

        assert!(!text.contains("MQTT brokers"));
    }

    #[test]
    fn observer_node_block_shows_configured_mqtt_brokers_and_status() {
        use fez_mesh_controller_core::ipc::{MqttBrokerStatus, MqttBrokerStatusDto, Snapshot};

        let mut app = App::new();
        app.apply_snapshot(Snapshot {
            mqtt_brokers: vec![
                MqttBrokerStatusDto {
                    name: "Home Assistant".to_string(),
                    status: MqttBrokerStatus::Connected,
                },
                MqttBrokerStatusDto {
                    name: "Backup".to_string(),
                    status: MqttBrokerStatus::Error {
                        reason: "connection refused".to_string(),
                    },
                },
            ],
            ..Default::default()
        });

        let text = render(&mut app, 120, 30);

        assert!(text.contains("MQTT brokers"));
        assert!(text.contains("Home Assistant"));
        assert!(text.contains("Connected"));
        assert!(text.contains("Backup"));
        assert!(text.contains("Error: connection refused"));
    }

    #[test]
    fn dashboard_places_the_cluster_block_above_the_observer_node_block() {
        let mut app = App::new();

        let buffer = render_buffer(&mut app, 120, 30);
        let cluster_y = text_row(&buffer, "Cluster").expect("Cluster block should render");
        let observer_y =
            text_row(&buffer, "Observer node").expect("Observer node block should render");

        assert!(
            cluster_y < observer_y,
            "expected Cluster (y={cluster_y}) above Observer node (y={observer_y})"
        );
    }

    // --- Cluster block -----------------------------------------------------

    #[test]
    fn draw_cluster_block_shows_empty_state_when_no_regions_configured() {
        let mut app = App::new();

        let text = render(&mut app, 120, 30);

        assert!(text.contains("Cluster"));
        assert!(text.contains("No regions configured"));
    }

    #[test]
    fn draw_cluster_block_shows_a_label_above_the_region_list() {
        use fez_mesh_controller_core::ipc::Snapshot;
        use fez_mesh_controller_core::RegionConfig;

        let mut app = App::new();
        app.apply_snapshot(Snapshot {
            regions: vec![RegionConfig {
                name: "World".to_string(),
                parent: None,
            }],
            ..Default::default()
        });

        let buffer = render_buffer(&mut app, 120, 30);
        let label_y = text_row(&buffer, "Regions:").expect("label should render");
        let world_y = text_row(&buffer, "World").expect("region should render");

        assert!(label_y < world_y);
    }

    #[test]
    fn draw_cluster_block_renders_the_hierarchy_indented_by_depth() {
        use fez_mesh_controller_core::ipc::Snapshot;
        use fez_mesh_controller_core::RegionConfig;

        let mut app = App::new();
        app.apply_snapshot(Snapshot {
            regions: vec![
                RegionConfig {
                    name: "World".to_string(),
                    parent: None,
                },
                RegionConfig {
                    name: "Europe".to_string(),
                    parent: Some("World".to_string()),
                },
                RegionConfig {
                    name: "France".to_string(),
                    parent: Some("Europe".to_string()),
                },
            ],
            ..Default::default()
        });

        let text = render(&mut app, 120, 30);

        // Every entry is shifted 2 extra characters right of the "Regions:"
        // label, on top of its own depth-based indentation.
        assert!(text.contains("  World"));
        assert!(text.contains("    Europe"));
        assert!(text.contains("      France"));
    }

    // --- Repeaters panel -----------------------------------------------------

    #[test]
    fn draw_repeaters_panel_is_titled_repeaters() {
        let mut app = App::new();

        let text = render(&mut app, 120, 30);

        assert!(text.contains("Repeaters ("));
        assert!(!text.contains("Contacts ("));
    }

    #[test]
    fn draw_repeaters_panel_strips_flag_emoji_from_the_name_column() {
        let mut app = App::new();
        app.snapshot.contacts = vec![ContactDto {
            name: "🇨🇵30 Depot".to_string(),
            ..contact("aabbccddeeff", false)
        }];

        let text = render(&mut app, 140, 20);

        assert!(text.contains("30 Depot"));
        assert!(!text
            .chars()
            .any(|c| ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)));
    }

    #[test]
    fn draw_repeaters_panel_only_shows_repeaters_and_room_servers() {
        let mut app = App::new();
        app.snapshot.contacts = vec![
            contact_of_type("111111111111", 1), // Chat
            contact_of_type("222222222222", 2), // Repeater
            contact_of_type("333333333333", 3), // Room
            contact_of_type("444444444444", 4), // Sensor
        ];

        let text = render(&mut app, 120, 30);

        assert!(text.contains("Repeaters (2)"));
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
    fn packet_log_table_highlights_a_row_addressed_to_a_managed_repeater() {
        let mut app = App::new();
        app.page = Page::PacketLog;
        app.set_packet_log(sample_entries());
        app.packet_table_state.select(None); // avoid the selection style masking the bg
        app.snapshot.contacts.push(contact("deadbeefcafe", true));

        let backend = TestBackend::new(120, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();

        // The TextMsg row (dest hash "de" matches the managed repeater's
        // prefix) is on the first data row of the table, just past the
        // left border/highlight-symbol gutter.
        let cell = &buffer[(3, 5)];
        assert_eq!(cell.bg, HL_ENDPOINT_MANAGED_BG);
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
    fn packet_log_table_shows_decoded_text_for_a_public_channel_message() {
        let mut app = App::new();
        app.page = Page::PacketLog;
        app.set_packet_log(vec![public_group_text_entry()]);
        app.packet_table_state.select(Some(0));

        let text = render(&mut app, 140, 20);

        assert!(text.contains("Public: Hello, mesh!"));
    }

    #[test]
    fn packet_log_table_shows_decoded_text_for_a_configured_hashtag_channel() {
        use fez_mesh_controller_core::ipc::Snapshot;

        let mut app = App::new();
        app.page = Page::PacketLog;
        app.apply_snapshot(Snapshot {
            hashtag_channels: vec!["#mytest".to_string()],
            ..Default::default()
        });
        // `channel_hash(0x61) || mac || ciphertext` for channel "#mytest",
        // cross-checked independently via `openssl`/Python (see
        // `core::channel::tests`) — decrypts to "Topic chat".
        app.set_packet_log(vec![PacketLogEntry {
            id: 1,
            at_unix: 0,
            snr: 1.0,
            rssi: -90,
            header: Some(PacketHeaderInfo {
                route_type: "Flood".to_string(),
                route_type_raw: 1,
                payload_type: "GroupText".to_string(),
                payload_type_raw: 5,
                payload_version: 0,
                hops: 1,
                path_hash_size: 1,
                path_hex: String::new(),
                transport_code_hex: None,
                dest_hash_hex: None,
                src_hash_hex: None,
                channel_hash_hex: Some("61".to_string()),
                anon_req_sender_public_key_hex: None,
                control: None,
                advertisement: None,
            }),
            payload_hex: "6197dfa79aead70a65177d5a0b785700bae78c".to_string(),
            payload_len: 19,
        }]);
        app.packet_table_state.select(Some(0));

        let text = render(&mut app, 140, 20);

        assert!(text.contains("#mytest: Topic chat"));
    }

    #[test]
    fn packet_log_table_does_not_decode_a_hashtag_channel_that_is_not_configured() {
        let mut app = App::new(); // no hashtag_channels configured
        app.page = Page::PacketLog;
        app.set_packet_log(vec![PacketLogEntry {
            id: 1,
            at_unix: 0,
            snr: 1.0,
            rssi: -90,
            header: Some(PacketHeaderInfo {
                route_type: "Flood".to_string(),
                route_type_raw: 1,
                payload_type: "GroupText".to_string(),
                payload_type_raw: 5,
                payload_version: 0,
                hops: 1,
                path_hash_size: 1,
                path_hex: String::new(),
                transport_code_hex: None,
                dest_hash_hex: None,
                src_hash_hex: None,
                channel_hash_hex: Some("61".to_string()),
                anon_req_sender_public_key_hex: None,
                control: None,
                advertisement: None,
            }),
            payload_hex: "6197dfa79aead70a65177d5a0b785700bae78c".to_string(),
            payload_len: 19,
        }]);
        app.packet_table_state.select(Some(0));

        let text = render(&mut app, 140, 20);

        assert!(!text.contains("Topic chat"));
        assert!(text.contains("bytes of (encrypted) text"));
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
    fn packet_detail_popup_shows_destination_without_requiring_source() {
        // AnonReq-shaped: a destination hash but no source hash (the
        // requester's full public key stands in for it instead).
        let mut app = App::new();
        app.page = Page::PacketLog;
        app.set_packet_log(vec![PacketLogEntry {
            id: 1,
            at_unix: 0,
            snr: 1.0,
            rssi: -90,
            header: Some(PacketHeaderInfo {
                route_type: "Flood".to_string(),
                route_type_raw: 1,
                payload_type: "AnonReq".to_string(),
                payload_type_raw: 7,
                payload_version: 0,
                hops: 1,
                path_hash_size: 1,
                path_hex: String::new(),
                transport_code_hex: None,
                dest_hash_hex: Some("de".to_string()),
                src_hash_hex: None,
                channel_hash_hex: None,
                anon_req_sender_public_key_hex: None,
                control: None,
                advertisement: None,
            }),
            payload_hex: "deadbeef".to_string(),
            payload_len: 4,
        }]);
        app.packet_table_state.select(Some(0));
        app.open_packet_detail();

        let text = render(&mut app, 140, 40);

        assert!(text.contains("Destination"));
        assert!(text.contains("de"));
        assert!(!text.contains("Source"));
    }

    fn anon_req_entry() -> PacketLogEntry {
        PacketLogEntry {
            id: 1,
            at_unix: 0,
            snr: 1.0,
            rssi: -90,
            header: Some(PacketHeaderInfo {
                route_type: "Flood".to_string(),
                route_type_raw: 1,
                payload_type: "AnonReq".to_string(),
                payload_type_raw: 7,
                payload_version: 0,
                hops: 1,
                path_hash_size: 1,
                path_hex: String::new(),
                transport_code_hex: None,
                dest_hash_hex: Some("de".to_string()),
                src_hash_hex: None,
                channel_hash_hex: None,
                anon_req_sender_public_key_hex: Some("ab".repeat(32)),
                control: None,
                advertisement: None,
            }),
            payload_hex: "deadbeef".to_string(),
            payload_len: 4,
        }
    }

    #[test]
    fn packet_log_table_shows_the_anon_req_senders_public_key_prefix() {
        let mut app = App::new();
        app.page = Page::PacketLog;
        app.set_packet_log(vec![anon_req_entry()]);
        app.packet_table_state.select(Some(0));

        let text = render(&mut app, 140, 20);

        assert!(text.contains("AnonReq from abababababab"));
    }

    #[test]
    fn packet_detail_popup_shows_the_anon_req_senders_full_public_key() {
        let mut app = App::new();
        app.page = Page::PacketLog;
        app.set_packet_log(vec![anon_req_entry()]);
        app.packet_table_state.select(Some(0));
        app.open_packet_detail();

        let text = render(&mut app, 140, 40);

        assert!(text.contains("Sender key"));
        assert!(text.contains(&"ab".repeat(32)));
    }

    fn control_entry(control: ControlPayloadInfo) -> PacketLogEntry {
        PacketLogEntry {
            id: 1,
            at_unix: 0,
            snr: 1.0,
            rssi: -90,
            header: Some(PacketHeaderInfo {
                route_type: "Direct".to_string(),
                route_type_raw: 2,
                payload_type: "Control".to_string(),
                payload_type_raw: 11,
                payload_version: 0,
                hops: 0,
                path_hash_size: 1,
                path_hex: String::new(),
                transport_code_hex: None,
                dest_hash_hex: None,
                src_hash_hex: None,
                channel_hash_hex: None,
                anon_req_sender_public_key_hex: None,
                control: Some(control),
                advertisement: None,
            }),
            payload_hex: "80000000000000".to_string(),
            payload_len: 7,
        }
    }

    #[test]
    fn packet_log_table_shows_a_discover_req_summary() {
        let mut app = App::new();
        app.page = Page::PacketLog;
        app.set_packet_log(vec![control_entry(ControlPayloadInfo::DiscoverReq {
            prefix_only: false,
            type_filter: 0x04,
            tag_hex: "11223344".to_string(),
            since_unix: Some(0),
        })]);
        app.packet_table_state.select(Some(0));

        let text = render(&mut app, 140, 20);

        assert!(text.contains("Discover request (type filter 0x04)"));
    }

    #[test]
    fn packet_log_table_shows_a_discover_resp_summary() {
        let mut app = App::new();
        app.page = Page::PacketLog;
        app.set_packet_log(vec![control_entry(ControlPayloadInfo::DiscoverResp {
            node_type_name: "Repeater".to_string(),
            snr: 5.0,
            tag_hex: "11223344".to_string(),
            pubkey_hex: "cd".repeat(32),
        })]);
        app.packet_table_state.select(Some(0));

        let text = render(&mut app, 140, 20);

        assert!(text.contains("Discover response: Repeater @ 5.0 dB"));
    }

    #[test]
    fn packet_detail_popup_shows_discover_req_fields() {
        let mut app = App::new();
        app.page = Page::PacketLog;
        app.set_packet_log(vec![control_entry(ControlPayloadInfo::DiscoverReq {
            prefix_only: true,
            type_filter: 0x04,
            tag_hex: "11223344".to_string(),
            since_unix: None,
        })]);
        app.packet_table_state.select(Some(0));
        app.open_packet_detail();

        let text = render(&mut app, 140, 40);

        assert!(text.contains("Discover request"));
        assert!(text.contains("Type filter"));
        assert!(text.contains("0x04"));
        assert!(text.contains("Tag"));
        assert!(text.contains("11223344"));
        assert!(text.contains("Since"));
        assert!(text.contains("any"));
        assert!(text.contains("Prefix only"));
        assert!(text.contains("yes"));
    }

    #[test]
    fn packet_detail_popup_shows_discover_resp_fields() {
        let mut app = App::new();
        app.page = Page::PacketLog;
        app.set_packet_log(vec![control_entry(ControlPayloadInfo::DiscoverResp {
            node_type_name: "Repeater".to_string(),
            snr: 5.0,
            tag_hex: "11223344".to_string(),
            pubkey_hex: "cd".repeat(32),
        })]);
        app.packet_table_state.select(Some(0));
        app.open_packet_detail();

        let text = render(&mut app, 140, 40);

        assert!(text.contains("Discover response"));
        assert!(text.contains("Node type"));
        assert!(text.contains("Repeater"));
        assert!(text.contains("SNR"));
        assert!(text.contains("5.0 dB"));
        assert!(text.contains("Public key"));
        assert!(text.contains(&"cd".repeat(32)));
    }

    #[test]
    fn packet_log_table_shows_a_channel_hash_in_braces_for_group_messages() {
        let mut app = App::new();
        app.page = Page::PacketLog;
        app.set_packet_log(vec![PacketLogEntry {
            id: 1,
            at_unix: 0,
            snr: 1.0,
            rssi: -90,
            header: Some(PacketHeaderInfo {
                route_type: "Flood".to_string(),
                route_type_raw: 1,
                payload_type: "GroupText".to_string(),
                payload_type_raw: 5,
                payload_version: 0,
                hops: 1,
                path_hash_size: 1,
                path_hex: String::new(),
                transport_code_hex: None,
                dest_hash_hex: None,
                src_hash_hex: None,
                channel_hash_hex: Some("ab".to_string()),
                anon_req_sender_public_key_hex: None,
                control: None,
                advertisement: None,
            }),
            payload_hex: "abcd".to_string(),
            payload_len: 2,
        }]);
        app.packet_table_state.select(Some(0));

        let text = render(&mut app, 120, 20);

        assert!(text.contains("{ab}"));
    }

    #[test]
    fn packet_detail_popup_shows_the_channel_hash_for_group_messages() {
        let mut app = App::new();
        app.page = Page::PacketLog;
        app.set_packet_log(vec![PacketLogEntry {
            id: 1,
            at_unix: 0,
            snr: 1.0,
            rssi: -90,
            header: Some(PacketHeaderInfo {
                route_type: "Flood".to_string(),
                route_type_raw: 1,
                payload_type: "GroupText".to_string(),
                payload_type_raw: 5,
                payload_version: 0,
                hops: 1,
                path_hash_size: 1,
                path_hex: String::new(),
                transport_code_hex: None,
                dest_hash_hex: None,
                src_hash_hex: None,
                channel_hash_hex: Some("ab".to_string()),
                anon_req_sender_public_key_hex: None,
                control: None,
                advertisement: None,
            }),
            payload_hex: "abcd".to_string(),
            payload_len: 2,
        }]);
        app.packet_table_state.select(Some(0));
        app.open_packet_detail();

        let text = render(&mut app, 140, 40);

        assert!(text.contains("Destination"));
        assert!(text.contains("{ab}"));
        assert!(text.contains("channel"));
    }

    #[test]
    fn packet_detail_popup_shows_the_decoded_channel_and_message_in_the_payload_section() {
        let mut app = App::new();
        app.page = Page::PacketLog;
        app.set_packet_log(vec![public_group_text_entry()]);
        app.packet_table_state.select(Some(0));
        app.open_packet_detail();

        let text = render(&mut app, 140, 40);

        assert!(text.contains("Payload"));
        assert!(text.contains("Channel"));
        assert!(text.contains("Public"));
        assert!(text.contains("Message"));
        assert!(text.contains("Hello, mesh!"));
        // Decoded, so no raw-hex fallback fields alongside it.
        assert!(!text.contains("Raw data"));
    }

    #[test]
    fn packet_detail_popup_shows_size_and_raw_data_for_an_undecoded_payload() {
        let mut app = App::new();
        app.page = Page::PacketLog;
        app.set_packet_log(vec![PacketLogEntry {
            id: 1,
            at_unix: 0,
            snr: 1.0,
            rssi: -90,
            header: Some(PacketHeaderInfo {
                route_type: "Flood".to_string(),
                route_type_raw: 1,
                payload_type: "Ack".to_string(),
                payload_type_raw: 4,
                payload_version: 0,
                hops: 1,
                path_hash_size: 1,
                path_hex: String::new(),
                transport_code_hex: None,
                dest_hash_hex: None,
                src_hash_hex: None,
                channel_hash_hex: None,
                anon_req_sender_public_key_hex: None,
                control: None,
                advertisement: None,
            }),
            payload_hex: "deadbeef".to_string(),
            payload_len: 4,
        }]);
        app.packet_table_state.select(Some(0));
        app.open_packet_detail();

        let text = render(&mut app, 140, 40);

        assert!(text.contains("— Payload —"));
        assert!(text.contains("Size"));
        assert!(text.contains("4 bytes"));
        assert!(text.contains("Raw data"));
        assert!(text.contains("deadbeef"));
    }

    // --- Transport code -----------------------------------------------------

    /// The same verified vector used in `core::meshcore_crypto`'s and
    /// `core::region`'s tests: region "TestRegion", payload_type 2
    /// ("TextMsg"), payload "deadbeef" -> transport code "a518".
    fn entry_with_transport_code(transport_code_hex: Option<&str>) -> PacketLogEntry {
        PacketLogEntry {
            id: 1,
            at_unix: 0,
            snr: 1.0,
            rssi: -90,
            header: Some(PacketHeaderInfo {
                route_type: "TransportFlood".to_string(),
                route_type_raw: 0,
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
                anon_req_sender_public_key_hex: None,
                control: None,
                advertisement: None,
            }),
            payload_hex: "deadbeef".to_string(),
            payload_len: 4,
        }
    }

    fn app_with_regions_and_entry(region_names: &[&str], entry: PacketLogEntry) -> App {
        use fez_mesh_controller_core::ipc::Snapshot;
        use fez_mesh_controller_core::RegionConfig;

        let mut app = App::new();
        app.page = Page::PacketLog;
        app.apply_snapshot(Snapshot {
            regions: region_names
                .iter()
                .map(|name| RegionConfig {
                    name: name.to_string(),
                    parent: None,
                })
                .collect(),
            ..Default::default()
        });
        app.set_packet_log(vec![entry]);
        app.packet_table_state.select(Some(0));
        app.open_packet_detail();
        app
    }

    #[test]
    fn transport_code_renders_green_when_it_matches_a_configured_region() {
        let mut app = app_with_regions_and_entry(
            &["TestRegion"],
            entry_with_transport_code(Some("a5180000")),
        );

        let buffer = render_buffer(&mut app, 140, 40);

        assert_eq!(fg_of_text(&buffer, "a518"), Some(GREEN));
    }

    #[test]
    fn transport_code_renders_plain_when_it_matches_no_configured_region() {
        let mut app = app_with_regions_and_entry(
            &["SomeOtherRegion"],
            entry_with_transport_code(Some("a5180000")),
        );

        let buffer = render_buffer(&mut app, 140, 40);

        assert_eq!(fg_of_text(&buffer, "a518"), Some(Color::White));
    }

    #[test]
    fn transport_code_all_zero_shows_the_share_label_and_is_never_green() {
        // { 0, 0 } is the firmware's explicit "Share" marker (isShare() in
        // examples/simple_repeater/MyMesh.cpp: "{ 0, 0 } means 'send this
        // nowhere'") -- not a leftover/unset value -- so it must never be
        // treated as a normal region-scoped code, even if a configured
        // region's name happened to collide.
        let mut app = app_with_regions_and_entry(
            &["TestRegion"],
            entry_with_transport_code(Some("00000000")),
        );

        let text = render(&mut app, 140, 40);
        let buffer = render_buffer(&mut app, 140, 40);

        assert!(text.contains("Transport code"));
        assert!(text.contains("Share"));
        assert!(text.contains("not repeated"));
        assert_ne!(fg_of_text(&buffer, "Share"), Some(GREEN));
    }

    #[test]
    fn transport_code_with_only_the_first_half_zero_is_not_treated_as_share() {
        // The "Share" marker requires *both* halves to be zero. A first
        // half of 0000 with a nonzero second half isn't a legitimate
        // firmware value (calcTransportCode reserves 0000 away), but the
        // check must still require both halves, not just the first --
        // otherwise a malformed/non-conformant packet with only the first
        // half zero would be mislabeled "Share".
        let mut app = app_with_regions_and_entry(
            &["TestRegion"],
            entry_with_transport_code(Some("00000001")),
        );

        let text = render(&mut app, 140, 40);

        assert!(text.contains("Transport code"));
        assert!(!text.contains("Share"));
    }

    #[test]
    fn no_transport_code_line_when_the_packet_has_none() {
        let mut app = app_with_regions_and_entry(&["TestRegion"], entry_with_transport_code(None));

        let text = render(&mut app, 140, 40);

        assert!(!text.contains("Transport code"));
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
        // The 2-hop reception's path ("aabb", path_hash_size 1) is shown as
        // its individual hop hashes separated by an arrow, not run together.
        assert!(text.contains("aa → bb"));
        assert!(!text.contains("aabb"));
    }

    #[test]
    fn packet_detail_popup_explains_the_payload_type_below_the_summary_block() {
        let mut app = App::new();
        app.page = Page::PacketLog;
        app.set_packet_log(sample_entries());
        app.packet_table_state.select(Some(0)); // the grouped TextMsg (newest)
        app.open_packet_detail();

        let text = render(&mut app, 140, 40);

        assert!(text.contains("About this packet type"));
        assert!(text.contains("end-to-end encrypted text message"));

        // Placed after the summary block (Payload version) and before the
        // per-reception breakdown.
        let version_pos = text.find("Payload version").unwrap();
        let about_pos = text.find("About this packet type").unwrap();
        let receptions_pos = text.find("Receptions (2)").unwrap();
        assert!(version_pos < about_pos);
        assert!(about_pos < receptions_pos);
    }

    #[test]
    fn payload_type_description_has_an_entry_for_every_known_payload_type() {
        for payload_type in [
            "Req",
            "Response",
            "TextMsg",
            "Ack",
            "Advert",
            "GroupText",
            "GroupData",
            "AnonReq",
            "Path",
            "Trace",
            "Multipart",
            "Control",
            "RawCustom",
        ] {
            assert_ne!(
                payload_type_description(payload_type),
                payload_type_description("some-unknown-type"),
                "{payload_type} should have its own description"
            );
        }
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
                route_type_raw: 2,
                payload_type: "Battery".to_string(),
                payload_type_raw: 255,
                payload_version: 0,
                hops: 0,
                path_hash_size: 1,
                path_hex: String::new(),
                transport_code_hex: None,
                dest_hash_hex: None,
                src_hash_hex: None,
                channel_hash_hex: None,
                anon_req_sender_public_key_hex: None,
                control: None,
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
