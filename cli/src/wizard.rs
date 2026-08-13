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

//! Interactive first-run setup wizard, launched automatically when no
//! configuration file is found (or explicitly via `fez-mesh-controller
//! setup`).

use std::time::Duration;

use console::style;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, ExecutableCommand};
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, Select};
use fez_mesh_controller_core::{region, Config, ConnectionConfig, DaemonConfig, RegionConfig};
use indicatif::{ProgressBar, ProgressStyle};
use names::Generator;
use ratatui::backend::CrosstermBackend;
use ratatui::style::{Color, Modifier as RatatuiModifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Terminal;

use crate::theme::{self, accent, muted, primary};

const MANUAL_ENTRY: &str = "✏️  Enter manually...";

/// Generates a fun, memorable default name (e.g. "rusty-nail") to suggest
/// as the controller's label.
fn random_node_label() -> String {
    Generator::default()
        .next()
        .unwrap_or_else(|| "fez-mesh-controller".to_string())
}

pub async fn run(existing: Option<&Config>) -> anyhow::Result<Config> {
    theme::section("Setup wizard", "🧙");
    println!(
        "  {}",
        muted().apply_to("Answer the few questions below; you can re-run this wizard")
    );
    println!(
        "  {}",
        muted().apply_to("at any time with `fez-mesh-controller setup`.")
    );

    let dtheme = ColorfulTheme::default();

    let node_label: String = Input::with_theme(&dtheme)
        .with_prompt("🏷️  Name of this controller")
        .default(
            existing
                .map(|c| c.node_label.clone())
                .unwrap_or_else(random_node_label),
        )
        .interact_text()?;

    let connection = ask_connection(&dtheme, existing.map(|c| &c.connection)).await?;

    let socket_path = existing
        .map(|c| c.daemon.socket_path.clone())
        .unwrap_or_else(fez_mesh_controller_core::config::default_socket_path);

    println!();
    let refresh_interval_secs: u64 = Input::with_theme(&dtheme)
        .with_prompt("⏱️  Refresh interval (seconds)")
        .default(
            existing
                .map(|c| c.daemon.refresh_interval_secs)
                .unwrap_or(5),
        )
        .interact_text()?;

    let log_level = existing
        .map(|c| c.daemon.log_level.clone())
        .unwrap_or_else(|| "info".to_string());
    let log_dir = existing
        .map(|c| c.daemon.log_dir.clone())
        .unwrap_or_else(fez_mesh_controller_core::config::default_log_dir);

    let managed_repeaters = existing
        .map(|c| c.managed_repeaters.clone())
        .unwrap_or_default();

    let existing_regions = existing.map(|c| c.regions.as_slice()).unwrap_or(&[]);
    let regions = ask_regions(existing_regions, &dtheme)?;

    let config = Config {
        node_label,
        connection,
        daemon: DaemonConfig {
            socket_path,
            refresh_interval_secs,
            log_level,
            log_dir,
            packet_log_capacity: fez_mesh_controller_core::config::default_packet_log_capacity(),
        },
        managed_repeaters,
        regions,
    };

    println!();
    println!("{} {}", style("📝").bold(), primary().apply_to("Summary"));
    println!(
        "   🏷️  Name         : {}",
        accent().apply_to(&config.node_label)
    );
    println!(
        "   📡 Connection   : {}",
        accent().apply_to(config.connection.to_string())
    );
    println!(
        "   🔌 IPC socket   : {}",
        accent().apply_to(config.daemon.socket_path.display())
    );
    println!(
        "   ⏱️  Refresh      : {}",
        accent().apply_to(format!("{}s", config.daemon.refresh_interval_secs))
    );
    println!();

    let confirmed = Confirm::with_theme(&dtheme)
        .with_prompt("💾 Save this configuration?")
        .default(true)
        .interact()?;

    if !confirmed {
        anyhow::bail!("configuration cancelled by user");
    }

    Ok(config)
}

async fn ask_connection(
    dtheme: &ColorfulTheme,
    existing: Option<&ConnectionConfig>,
) -> anyhow::Result<ConnectionConfig> {
    let options = ["📻 Serial (USB / UART)", "🌐 TCP", "📶 Bluetooth (BLE)"];
    let default_idx = match existing {
        Some(ConnectionConfig::Serial { .. }) => 0,
        Some(ConnectionConfig::Tcp { .. }) => 1,
        Some(ConnectionConfig::Ble { .. }) => 2,
        None => 0,
    };

    let choice = Select::with_theme(dtheme)
        .with_prompt("📡 Connection type to the MeshCore node")
        .items(&options)
        .default(default_idx)
        .interact()?;

    match choice {
        0 => ask_serial(dtheme, existing),
        1 => ask_tcp(dtheme, existing),
        _ => ask_ble(dtheme, existing).await,
    }
}

fn ask_serial(
    dtheme: &ColorfulTheme,
    existing: Option<&ConnectionConfig>,
) -> anyhow::Result<ConnectionConfig> {
    let existing_port = match existing {
        Some(ConnectionConfig::Serial { port, .. }) => Some(port.clone()),
        _ => None,
    };

    let available: Vec<String> = serialport::available_ports()
        .map(|ports| ports.into_iter().map(|p| p.port_name).collect())
        .unwrap_or_default();

    let port = if available.is_empty() {
        Input::with_theme(dtheme)
            .with_prompt("🔌 Serial port (e.g. /dev/ttyUSB0, /dev/tty.usbserial-XXXX)")
            .default(existing_port.unwrap_or_else(|| "/dev/ttyUSB0".to_string()))
            .interact_text()?
    } else {
        let mut items: Vec<String> = available.clone();
        items.push(MANUAL_ENTRY.to_string());
        let default_idx = existing_port
            .as_ref()
            .and_then(|p| available.iter().position(|a| a == p))
            .unwrap_or(0);

        let idx = Select::with_theme(dtheme)
            .with_prompt("🔌 Detected serial port")
            .items(&items)
            .default(default_idx)
            .interact()?;

        if idx == items.len() - 1 {
            Input::with_theme(dtheme)
                .with_prompt("🔌 Serial port")
                .default("/dev/ttyUSB0".to_string())
                .interact_text()?
        } else {
            items[idx].clone()
        }
    };

    let baud_rate: u32 = Input::with_theme(dtheme)
        .with_prompt("⚡ Baud rate")
        .default(match existing {
            Some(ConnectionConfig::Serial { baud_rate, .. }) => *baud_rate,
            _ => 115_200,
        })
        .interact_text()?;

    Ok(ConnectionConfig::Serial { port, baud_rate })
}

fn ask_tcp(
    dtheme: &ColorfulTheme,
    existing: Option<&ConnectionConfig>,
) -> anyhow::Result<ConnectionConfig> {
    let host: String = Input::with_theme(dtheme)
        .with_prompt("🌐 Node address (host)")
        .default(match existing {
            Some(ConnectionConfig::Tcp { host, .. }) => host.clone(),
            _ => "192.168.1.50".to_string(),
        })
        .interact_text()?;

    let port: u16 = Input::with_theme(dtheme)
        .with_prompt("🔢 TCP port")
        .default(match existing {
            Some(ConnectionConfig::Tcp { port, .. }) => *port,
            _ => 5000,
        })
        .interact_text()?;

    Ok(ConnectionConfig::Tcp { host, port })
}

async fn ask_ble(
    dtheme: &ColorfulTheme,
    existing: Option<&ConnectionConfig>,
) -> anyhow::Result<ConnectionConfig> {
    let scan = Confirm::with_theme(dtheme)
        .with_prompt("🔍 Scan for nearby MeshCore devices (5s)?")
        .default(true)
        .interact()?;

    let discovered = if scan {
        let spinner = ProgressBar::new_spinner();
        spinner.set_style(ProgressStyle::with_template("{spinner:.cyan} {msg}").unwrap());
        spinner.set_message("Scanning for BLE devices...");
        spinner.enable_steady_tick(Duration::from_millis(100));

        let result = meshcore_rs::MeshCore::ble_discover(Duration::from_secs(5)).await;

        spinner.finish_and_clear();
        match result {
            Ok(devices) if !devices.is_empty() => devices,
            Ok(_) => {
                theme::info_line("no devices found, make sure Bluetooth is enabled");
                Vec::new()
            }
            Err(err) => {
                theme::error_line(&format!("BLE scan failed: {err}"));
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    let name = if discovered.is_empty() {
        Input::with_theme(dtheme)
            .with_prompt("📶 BLE device name")
            .default(match existing {
                Some(ConnectionConfig::Ble { name }) => name.clone(),
                _ => String::new(),
            })
            .interact_text()?
    } else {
        let mut items = discovered.clone();
        items.push(MANUAL_ENTRY.to_string());
        let idx = Select::with_theme(dtheme)
            .with_prompt("📶 Detected device")
            .items(&items)
            .default(0)
            .interact()?;

        if idx == items.len() - 1 {
            Input::with_theme(dtheme)
                .with_prompt("📶 BLE device name")
                .interact_text()?
        } else {
            items[idx].clone()
        }
    };

    Ok(ConnectionConfig::Ble { name })
}

/// A region name at a given indentation depth while interactively
/// arranging the hierarchy. Order + depth together determine the parent
/// (the nearest preceding entry one level shallower) — see
/// `regions_from_outline`.
type RegionOutline = Vec<(String, usize)>;

/// Prompts for the cluster's region names, then — if there are at least
/// two — a small embedded screen to arrange them into a hierarchy
/// (indent/unindent with the arrow keys), mirroring the MeshCore node
/// firmware's own region tree. `dialoguer` has no tree/indent widget
/// (only `Sort`, for flat reordering), so arranging needs a custom screen;
/// entering names stays a normal `Input` loop, consistent with the rest of
/// the wizard.
fn ask_regions(
    existing: &[RegionConfig],
    dtheme: &ColorfulTheme,
) -> anyhow::Result<Vec<RegionConfig>> {
    println!();
    println!(
        "{} {}",
        style("🧩").bold(),
        primary().apply_to("Cluster regions")
    );
    println!(
        "  {}",
        muted().apply_to("Regions this controller's cluster is organized into, mirroring the")
    );
    println!(
        "  {}",
        muted().apply_to("MeshCore node firmware's own region concept. Leave empty to skip.")
    );

    let mut outline = outline_from_regions(existing);

    loop {
        let name: String = Input::with_theme(dtheme)
            .with_prompt("🧩 Add a region name (leave empty to continue)")
            .allow_empty(true)
            .interact_text()?;
        let name = name.trim();
        if name.is_empty() {
            break;
        }
        outline.push((name.to_string(), 0));
    }

    if outline.len() >= 2 {
        outline = arrange_region_hierarchy(outline)?;
    }

    Ok(regions_from_outline(&outline))
}

/// Flattens existing `RegionConfig`s (parent-by-name) into the ordered
/// `(name, depth)` outline the arrange screen edits, so re-running the
/// wizard preserves whatever hierarchy was already configured.
fn outline_from_regions(regions: &[RegionConfig]) -> RegionOutline {
    region::flatten_region_tree(regions)
        .into_iter()
        .map(|(depth, r)| (r.name.clone(), depth))
        .collect()
}

/// Converts the arranged `(name, depth)` outline back into `RegionConfig`s:
/// each entry's parent is the name of the nearest *preceding* entry one
/// depth shallower (`None` for a depth-0 root).
fn regions_from_outline(outline: &RegionOutline) -> Vec<RegionConfig> {
    outline
        .iter()
        .enumerate()
        .map(|(i, (name, depth))| {
            let parent = if *depth == 0 {
                None
            } else {
                outline[..i]
                    .iter()
                    .rev()
                    .find(|(_, d)| *d == depth - 1)
                    .map(|(n, _)| n.clone())
            };
            RegionConfig {
                name: name.clone(),
                parent,
            }
        })
        .collect()
}

/// The new depth for indenting (`delta > 0`) or unindenting (`delta < 0`)
/// the entry at `index`. Indenting is capped at one level deeper than the
/// *immediately preceding* entry (can't skip a level — the same rule
/// outliners like Workflowy/org-mode use), and the very first entry can
/// never be indented (nothing precedes it to nest under). Unindenting is
/// floored at 0.
fn clamp_indent(outline: &RegionOutline, index: usize, delta: i32) -> usize {
    let current = outline[index].1;
    if delta > 0 {
        let max_depth = if index == 0 {
            0
        } else {
            outline[index - 1].1 + 1
        };
        (current + 1).min(max_depth)
    } else {
        current.saturating_sub(1)
    }
}

/// Runs the embedded raw-mode screen for arranging `outline` into a
/// hierarchy. Returns the arranged outline on `Enter`, or the original
/// (pre-arrangement) outline unchanged on `Esc`. Enters/exits raw mode +
/// the alternate screen the same way `cli/src/tui/mod.rs`'s
/// `setup_terminal`/`restore_terminal` do; not shared with that module
/// since this is simple enough not to be worth coupling the two.
fn arrange_region_hierarchy(outline: RegionOutline) -> anyhow::Result<RegionOutline> {
    let original = outline.clone();
    let mut outline = outline;
    let mut selected: usize = 0;

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

    let result = (|| -> anyhow::Result<RegionOutline> {
        loop {
            terminal.draw(|frame| draw_arrange_screen(frame, &outline, selected))?;

            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }

            match key.code {
                KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    if selected > 0 {
                        outline.swap(selected, selected - 1);
                        selected -= 1;
                    }
                }
                KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    if selected + 1 < outline.len() {
                        outline.swap(selected, selected + 1);
                        selected += 1;
                    }
                }
                KeyCode::Up => selected = selected.saturating_sub(1),
                KeyCode::Down => {
                    if selected + 1 < outline.len() {
                        selected += 1;
                    }
                }
                KeyCode::Right => outline[selected].1 = clamp_indent(&outline, selected, 1),
                KeyCode::Left => outline[selected].1 = clamp_indent(&outline, selected, -1),
                KeyCode::Enter => return Ok(outline.clone()),
                KeyCode::Esc => return Ok(original.clone()),
                _ => {}
            }
        }
    })();

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn draw_arrange_screen(frame: &mut ratatui::Frame, outline: &RegionOutline, selected: usize) {
    let mut lines = vec![
        Line::from(Span::styled(
            "Arrange the region hierarchy",
            Style::default().add_modifier(RatatuiModifier::BOLD),
        )),
        Line::from(""),
    ];

    for (i, (name, depth)) in outline.iter().enumerate() {
        let cursor = if i == selected { "➤ " } else { "  " };
        let text = format!("{cursor}{}{name}", "  ".repeat(*depth));
        let style = if i == selected {
            Style::default()
                .add_modifier(RatatuiModifier::BOLD)
                .bg(Color::Rgb(0x2a, 0x2e, 0x3a))
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(text, style)));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "↑/↓ select   Shift+↑/↓ move   →/← indent/unindent   Enter confirm   Esc cancel",
        Style::default().fg(Color::DarkGray),
    )));

    let block = Block::default().borders(Borders::ALL).title(" 🧩 Regions ");
    frame.render_widget(Paragraph::new(lines).block(block), frame.area());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outline(pairs: &[(&str, usize)]) -> RegionOutline {
        pairs.iter().map(|(n, d)| (n.to_string(), *d)).collect()
    }

    // --- outline_from_regions / regions_from_outline (round-trip) --------

    #[test]
    fn regions_from_outline_resolves_parent_by_nearest_preceding_shallower_entry() {
        let o = outline(&[("World", 0), ("Europe", 1), ("France", 2), ("Asia", 1)]);

        let regions = regions_from_outline(&o);

        assert_eq!(regions[0].parent, None);
        assert_eq!(regions[1].parent.as_deref(), Some("World"));
        assert_eq!(regions[2].parent.as_deref(), Some("Europe"));
        assert_eq!(regions[3].parent.as_deref(), Some("World"));
    }

    #[test]
    fn outline_and_regions_round_trip() {
        let regions = vec![
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
            RegionConfig {
                name: "Asia".to_string(),
                parent: Some("World".to_string()),
            },
        ];

        let o = outline_from_regions(&regions);
        let round_tripped = regions_from_outline(&o);

        assert_eq!(round_tripped, regions);
    }

    #[test]
    fn outline_and_regions_round_trip_with_multiple_roots() {
        let regions = vec![
            RegionConfig {
                name: "World".to_string(),
                parent: None,
            },
            RegionConfig {
                name: "Moon".to_string(),
                parent: None,
            },
        ];

        let o = outline_from_regions(&regions);
        let round_tripped = regions_from_outline(&o);

        assert_eq!(round_tripped, regions);
    }

    // --- clamp_indent -------------------------------------------------------

    #[test]
    fn clamp_indent_cannot_indent_the_first_entry() {
        let o = outline(&[("World", 0)]);
        assert_eq!(clamp_indent(&o, 0, 1), 0);
    }

    #[test]
    fn clamp_indent_cannot_skip_a_level() {
        // "Asia" (depth 0) follows "France" (depth 2) — indenting "Asia"
        // can only reach depth 1 (France's depth + 1), not jump to 3.
        let o = outline(&[("World", 0), ("Europe", 1), ("France", 2), ("Asia", 0)]);
        assert_eq!(clamp_indent(&o, 3, 1), 1);
    }

    #[test]
    fn clamp_indent_unindent_floors_at_zero() {
        let o = outline(&[("World", 0)]);
        assert_eq!(clamp_indent(&o, 0, -1), 0);
    }

    #[test]
    fn clamp_indent_normal_indent_and_unindent() {
        let o = outline(&[("World", 0), ("Europe", 0)]);
        assert_eq!(clamp_indent(&o, 1, 1), 1); // indent under World
        let o2 = outline(&[("World", 0), ("Europe", 1)]);
        assert_eq!(clamp_indent(&o2, 1, -1), 0); // unindent back to root
    }
}
