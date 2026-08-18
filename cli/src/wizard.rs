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

use std::path::PathBuf;
use std::time::Duration;

use console::{style, Key, Term};
use dialoguer::theme::{ColorfulTheme, Theme};
use dialoguer::{Confirm, Input, Password, Select};
use fez_mesh_controller_core::{
    region, Config, ConnectionConfig, DaemonConfig, MqttAuthMethod, MqttBrokerConfig,
    MqttTransportProtocol, RegionConfig,
};
use indicatif::{ProgressBar, ProgressStyle};
use names::Generator;

use crate::theme::{self, accent, muted, primary};

const MANUAL_ENTRY: &str = "✏️  Enter manually...";

/// Generates a fun, memorable default name (e.g. "rusty-nail") to suggest
/// as the controller's label.
fn random_node_label() -> String {
    Generator::default()
        .next()
        .unwrap_or_else(|| "fez-mesh-controller".to_string())
}

/// Runs the interactive setup wizard. Returns `Ok(None)` (not an error) if
/// the user declines to save at the final confirmation — callers should
/// treat that as a normal, deliberate cancellation, not a failure.
pub async fn run(existing: Option<&Config>) -> anyhow::Result<Option<Config>> {
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

    // Working state, seeded from `existing` (or defaults) before
    // branching below, so both flows mutate the same variables and the
    // `Config` assembly further down doesn't need to know which flow ran.
    let mut node_label = existing
        .map(|c| c.node_label.clone())
        .unwrap_or_else(random_node_label);
    let mut refresh_interval_secs = existing
        .map(|c| c.daemon.refresh_interval_secs)
        .unwrap_or(5);
    let mut regions = existing.map(|c| c.regions.clone()).unwrap_or_default();
    let mut connection = existing.map(|c| c.connection.clone());
    let mut mqtt_brokers = existing.map(|c| c.mqtt_brokers.clone()).unwrap_or_default();
    let mut observer_node_managed_config = existing
        .map(|c| c.daemon.observer_node_managed_config)
        .unwrap_or(true);

    if existing.is_none() {
        // First run: chain the three sections, numbered.
        run_general_section(
            &dtheme,
            Some((1, 3)),
            &mut node_label,
            &mut refresh_interval_secs,
            &mut regions,
        )?;
        connection = Some(
            run_connection_section(
                &dtheme,
                Some((2, 3)),
                connection.as_ref(),
                &mut observer_node_managed_config,
            )
            .await?,
        );
        run_mqtt_section(&dtheme, Some((3, 3)), &mut mqtt_brokers)?;
    } else {
        // Re-run: let the user pick which section to revisit, as many
        // times as they like, until they're done.
        loop {
            let items = ["🔧 General", "📡 Connection", "📶 MQTT", "✅ Terminate"];
            let choice = Select::with_theme(&dtheme)
                .with_prompt("What would you like to configure?")
                .items(&items)
                .default(0)
                .interact()?;

            match choice {
                0 => run_general_section(
                    &dtheme,
                    None,
                    &mut node_label,
                    &mut refresh_interval_secs,
                    &mut regions,
                )?,
                1 => {
                    connection = Some(
                        run_connection_section(
                            &dtheme,
                            None,
                            connection.as_ref(),
                            &mut observer_node_managed_config,
                        )
                        .await?,
                    )
                }
                2 => run_mqtt_section(&dtheme, None, &mut mqtt_brokers)?,
                _ => break,
            }
        }
    }

    let connection = connection.expect("set by the first-run chain or the re-run menu above");

    let socket_path = existing
        .map(|c| c.daemon.socket_path.clone())
        .unwrap_or_else(fez_mesh_controller_core::config::default_socket_path);
    let log_level = existing
        .map(|c| c.daemon.log_level.clone())
        .unwrap_or_else(|| "info".to_string());
    let log_dir = existing
        .map(|c| c.daemon.log_dir.clone())
        .unwrap_or_else(fez_mesh_controller_core::config::default_log_dir);
    let db_path = existing
        .map(|c| c.daemon.db_path.clone())
        .unwrap_or_else(fez_mesh_controller_core::config::default_db_path);
    let managed_repeaters = existing
        .map(|c| c.managed_repeaters.clone())
        .unwrap_or_default();
    let hashtag_channels = existing
        .map(|c| c.hashtag_channels.clone())
        .unwrap_or_default();

    let config = Config {
        node_label,
        connection,
        daemon: DaemonConfig {
            socket_path,
            refresh_interval_secs,
            log_level,
            log_dir,
            packet_log_capacity: fez_mesh_controller_core::config::default_packet_log_capacity(),
            db_path,
            observer_node_managed_config,
        },
        managed_repeaters,
        regions,
        hashtag_channels,
        mqtt_brokers,
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
    println!(
        "   🔒 Observer lock: {}",
        accent().apply_to(if config.daemon.observer_node_managed_config {
            "Yes"
        } else {
            "No"
        })
    );
    if config.regions.is_empty() {
        println!("   🧩 Regions      : {}", muted().apply_to("(none)"));
    } else {
        println!(
            "   🧩 Regions      : {}",
            accent().apply_to(format!("{} configured", config.regions.len()))
        );
        for (depth, region) in region::flatten_region_tree(&config.regions) {
            println!("        {}{}", "  ".repeat(depth), region.name);
        }
    }
    if config.mqtt_brokers.is_empty() {
        println!("   📡 MQTT brokers : {}", muted().apply_to("(none)"));
    } else {
        println!(
            "   📡 MQTT brokers : {}",
            accent().apply_to(format!("{} configured", config.mqtt_brokers.len()))
        );
        for broker in &config.mqtt_brokers {
            println!("        {} ({}:{})", broker.name, broker.host, broker.port);
        }
    }
    println!();

    let confirmed = Confirm::with_theme(&dtheme)
        .with_prompt("💾 Save this configuration?")
        .default(true)
        .interact()?;

    if !confirmed {
        return Ok(None);
    }

    Ok(Some(config))
}

/// Prints a section's header: `[step/total] Title` when chained during
/// first-run setup, or just `Title` when reached via the re-run menu
/// (`step: None`) — see [`run`].
fn print_section_header(step: Option<(usize, usize)>, emoji: &str, title: &str) {
    match step {
        Some((n, total)) => theme::section(&format!("[{n}/{total}] {title}"), emoji),
        None => theme::section(title, emoji),
    }
}

/// The "General" section: controller name, refresh interval, and the
/// cluster region hierarchy — grouped together as the settings that are
/// neither connection- nor MQTT-specific. See [`run`].
fn run_general_section(
    dtheme: &ColorfulTheme,
    step: Option<(usize, usize)>,
    node_label: &mut String,
    refresh_interval_secs: &mut u64,
    regions: &mut Vec<RegionConfig>,
) -> anyhow::Result<()> {
    print_section_header(step, "🔧", "General");

    *node_label = Input::with_theme(dtheme)
        .with_prompt("🏷️  Name of this controller")
        .default(node_label.clone())
        .interact_text()?;

    println!();
    *refresh_interval_secs = Input::with_theme(dtheme)
        .with_prompt("⏱️  Refresh interval (seconds)")
        .default(*refresh_interval_secs)
        .interact_text()?;

    *regions = ask_regions(regions, dtheme)?;

    Ok(())
}

/// The "Connection" section — see [`run`].
async fn run_connection_section(
    dtheme: &ColorfulTheme,
    step: Option<(usize, usize)>,
    existing: Option<&ConnectionConfig>,
    observer_node_managed_config: &mut bool,
) -> anyhow::Result<ConnectionConfig> {
    print_section_header(step, "📡", "Connection");
    let connection = ask_connection(dtheme, existing).await?;

    println!();
    println!(
        "  {}",
        muted().apply_to(
            "When enabled, the daemon locks this node to an observer-only state on every"
        )
    );
    println!(
        "  {}",
        muted()
            .apply_to("connect: disables its contact auto-add, clears all channels, and keeps its")
    );
    println!(
        "  {}",
        muted().apply_to("contact list in sync with the managed repeaters below.")
    );
    *observer_node_managed_config = Confirm::with_theme(dtheme)
        .with_prompt("🔒 Enforce observer-only node configuration?")
        .default(*observer_node_managed_config)
        .interact()?;

    Ok(connection)
}

/// The "MQTT" section — see [`run`].
fn run_mqtt_section(
    dtheme: &ColorfulTheme,
    step: Option<(usize, usize)>,
    mqtt_brokers: &mut Vec<MqttBrokerConfig>,
) -> anyhow::Result<()> {
    print_section_header(step, "📶", "MQTT");
    *mqtt_brokers = ask_mqtt_brokers(mqtt_brokers, dtheme)?;
    Ok(())
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

/// A region name at a given indentation depth while interactively editing
/// the hierarchy in [`edit_region_tree`]. Order + depth together imply
/// the parent (the nearest preceding entry one level shallower) — see
/// [`regions_from_outline`].
type RegionOutline = Vec<(String, usize)>;

/// Prompts for the cluster's region hierarchy via a single interactive
/// tree editor (add/rename/delete/reorder/reparent all within one
/// widget) — see [`edit_region_tree`].
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
        muted().apply_to("MeshCore node firmware's own region concept. Arrange the tree below;")
    );
    println!(
        "  {}",
        muted().apply_to("Enter to confirm, Esc to cancel any changes made here.")
    );
    println!();

    edit_region_tree(existing, dtheme, &Term::stderr())
}

/// Flattens `regions` (parent-by-name) into the ordered `(name, depth)`
/// outline [`edit_region_tree`] edits, so re-running the wizard starts
/// from whatever hierarchy was already configured.
fn outline_from_regions(regions: &[RegionConfig]) -> RegionOutline {
    region::flatten_region_tree(regions)
        .into_iter()
        .map(|(depth, r)| (r.name.clone(), depth))
        .collect()
}

/// Converts an edited `(name, depth)` outline back into `RegionConfig`s:
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

/// Removes the entry at `index`, promoting its entire subtree (direct
/// children *and* deeper descendants) up one depth level first — every
/// following entry with depth greater than the removed one's, until an
/// entry at depth ≤ its own is reached, has its depth decremented by 1.
/// Decrementing only direct children and leaving grandchildren alone
/// would silently double-nest them relative to their now-shifted parent.
fn delete_outline_entry(outline: &mut RegionOutline, index: usize) {
    let depth = outline[index].1;
    let mut end = index + 1;
    while end < outline.len() && outline[end].1 > depth {
        outline[end].1 -= 1;
        end += 1;
    }
    outline.remove(index);
}

/// Interactive single-widget tree editor for the cluster's region
/// hierarchy — a custom `dialoguer`-style component. `dialoguer` has no
/// built-in tree/outline widget, and this stays away from
/// `ratatui`/full-screen alternate-screen rendering (matching the rest of
/// this wizard): it renders inline in the normal scrollback via
/// `console::Term`, the same underlying mechanism `dialoguer`'s own
/// `Select`/`Sort` prompts use internally (`term.read_key()` for
/// single-keypress input, `term.clear_last_lines()` + redraw for
/// updates), reusing the public `Theme` trait's
/// `format_select_prompt`/`format_select_prompt_item`/
/// `format_input_prompt` so the look matches the rest of the wizard
/// exactly (dialoguer's own renderer that backs those calls internally,
/// `theme::render::TermThemeRenderer`, is crate-private — not reusable
/// from here).
///
/// Keys: `↑`/`↓` move the cursor, `←`/`→` (or `h`/`l`) indent/unindent
/// (reparent), `j`/`k` reorder (swap the selected entry with the
/// next/previous one), `a` add a sibling, `r` rename, `d`/Delete/
/// Backspace delete (promoting the deleted entry's subtree, see
/// [`delete_outline_entry`]), Enter confirm, Esc cancel (discarding every
/// edit made this session).
fn edit_region_tree(
    initial: &[RegionConfig],
    theme: &dyn Theme,
    term: &Term,
) -> anyhow::Result<Vec<RegionConfig>> {
    let mut outline = outline_from_regions(initial);
    let mut selected: usize = 0;
    let mut previous_line_count = 0;

    term.hide_cursor()?;

    let result = (|| -> anyhow::Result<Vec<RegionConfig>> {
        loop {
            previous_line_count =
                render_region_tree(term, theme, &outline, selected, previous_line_count, None)?;

            match term.read_key()? {
                Key::ArrowUp => selected = selected.saturating_sub(1),
                Key::ArrowDown => {
                    if selected + 1 < outline.len() {
                        selected += 1;
                    }
                }
                Key::ArrowRight | Key::Char('l') if !outline.is_empty() => {
                    outline[selected].1 = clamp_indent(&outline, selected, 1);
                }
                Key::ArrowLeft | Key::Char('h') if !outline.is_empty() => {
                    outline[selected].1 = clamp_indent(&outline, selected, -1);
                }
                Key::Char('k') if selected > 0 => {
                    outline.swap(selected, selected - 1);
                    selected -= 1;
                }
                Key::Char('j') if selected + 1 < outline.len() => {
                    outline.swap(selected, selected + 1);
                    selected += 1;
                }
                Key::Char('a') => {
                    // A blank placeholder, right after the selected entry
                    // (or the first root if the tree is empty), edited in
                    // place at its correct position/indentation — see
                    // `edit_outline_entry_inline`. Removed again below if
                    // left empty (Esc, or confirmed with no text).
                    let depth = outline.get(selected).map(|(_, d)| *d).unwrap_or(0);
                    let insert_at = if outline.is_empty() { 0 } else { selected + 1 };
                    outline.insert(insert_at, (String::new(), depth));
                    selected = insert_at;

                    let (name, lines) = edit_outline_entry_inline(
                        term,
                        theme,
                        &outline,
                        selected,
                        previous_line_count,
                    )?;
                    previous_line_count = lines;

                    match name.map(|n| n.trim().to_string()) {
                        Some(name) if !name.is_empty() => outline[selected].0 = name,
                        _ => {
                            outline.remove(selected);
                            selected = selected.saturating_sub(1);
                        }
                    }
                }
                Key::Char('r') if !outline.is_empty() => {
                    let original = outline[selected].0.clone();
                    let (name, lines) = edit_outline_entry_inline(
                        term,
                        theme,
                        &outline,
                        selected,
                        previous_line_count,
                    )?;
                    previous_line_count = lines;

                    outline[selected].0 = match name.map(|n| n.trim().to_string()) {
                        Some(name) if !name.is_empty() => name,
                        _ => original,
                    };
                }
                Key::Char('d') | Key::Backspace | Key::Del if !outline.is_empty() => {
                    delete_outline_entry(&mut outline, selected);
                    if selected >= outline.len() {
                        selected = outline.len().saturating_sub(1);
                    }
                }
                Key::Enter => return Ok(regions_from_outline(&outline)),
                Key::Escape => return Ok(initial.to_vec()),
                _ => {}
            }
        }
    })();

    term.clear_last_lines(previous_line_count)?;
    term.show_cursor()?;

    result
}

/// Redraws the whole tree-editor widget in place: clears
/// `previous_line_count` lines from the last frame, prints the
/// prompt/header line, one line per outline entry (indented; the
/// selected one styled via `theme.format_select_prompt_item`, matching
/// `Select`/`Sort`'s own active-item look), then a keybinding legend.
/// Returns the number of lines just printed, to pass back in as
/// `previous_line_count` next frame.
///
/// `editing`, when set, is `(index, in-progress buffer)` for
/// [`edit_outline_entry_inline`]: that entry's row renders the raw buffer
/// (with a trailing cursor mark) instead of `outline[index].0`, still at
/// its correct indentation, and the footer swaps to the text-edit legend.
fn render_region_tree(
    term: &Term,
    theme: &dyn Theme,
    outline: &RegionOutline,
    selected: usize,
    previous_line_count: usize,
    editing: Option<(usize, &str)>,
) -> anyhow::Result<usize> {
    term.clear_last_lines(previous_line_count)?;

    let mut lines = Vec::new();

    let mut prompt_line = String::new();
    theme.format_select_prompt(&mut prompt_line, "Region hierarchy")?;
    lines.push(prompt_line);

    if outline.is_empty() {
        lines.push("  (no regions yet — press 'a' to add one)".to_string());
    } else {
        for (i, (name, depth)) in outline.iter().enumerate() {
            let indent = "  ".repeat(*depth);
            let text = match editing {
                Some((edit_idx, buf)) if edit_idx == i => format!("{indent}{buf}▏"),
                _ => format!("{indent}{name}"),
            };
            let is_active = editing.map_or(i == selected, |(edit_idx, _)| edit_idx == i);
            let mut item_line = String::new();
            theme.format_select_prompt_item(&mut item_line, &text, is_active)?;
            lines.push(item_line);
        }
    }

    lines.push(String::new());
    let legend = if editing.is_some() {
        "Type the region name — Enter confirm   Esc cancel"
    } else {
        "↑/↓ move   ←/→ nest   j/k reorder   a add   r rename   d delete   Enter done   Esc cancel"
    };
    lines.push(style(legend).dim().to_string());

    for line in &lines {
        term.write_line(line)?;
    }

    Ok(lines.len())
}

/// Edits the outline entry at `index` **in place, live, as part of the
/// tree's own rendering** (correct row, correct indentation) — used by
/// [`edit_region_tree`]'s `a`/`r` keys instead of a separate prompt line
/// below the tree. `Char` appends, `Backspace` removes the last
/// character, `Enter` commits (returns the typed text — possibly empty
/// or unchanged, callers decide what that means), `Esc` cancels (`None`).
/// Read-only on `outline`: only reads `outline[index].0` for the initial
/// buffer and never writes to it — the caller applies the result.
fn edit_outline_entry_inline(
    term: &Term,
    theme: &dyn Theme,
    outline: &RegionOutline,
    index: usize,
    mut previous_line_count: usize,
) -> anyhow::Result<(Option<String>, usize)> {
    let mut buf = outline[index].0.clone();

    loop {
        previous_line_count = render_region_tree(
            term,
            theme,
            outline,
            index,
            previous_line_count,
            Some((index, buf.as_str())),
        )?;

        match term.read_key()? {
            Key::Char(c) if !c.is_control() => buf.push(c),
            Key::Backspace => {
                buf.pop();
            }
            Key::Enter => return Ok((Some(buf), previous_line_count)),
            Key::Escape => return Ok((None, previous_line_count)),
            _ => {}
        }
    }
}

/// Interactively builds the list of MQTT brokers to forward received mesh
/// events to (`Config::mqtt_brokers`) — same topics/JSON format as the
/// community `meshcore-mqtt` bridge, see `daemon/src/mqtt.rs`. A
/// `Confirm`-driven "add another?" loop, since (unlike regions, a flat
/// list of names) each broker needs several fields.
fn ask_mqtt_brokers(
    existing: &[MqttBrokerConfig],
    dtheme: &ColorfulTheme,
) -> anyhow::Result<Vec<MqttBrokerConfig>> {
    println!(
        "  {}",
        muted().apply_to("Forward received mesh events to MQTT brokers, using the same topics")
    );
    println!(
        "  {}",
        muted().apply_to("and message format as the community meshcore-mqtt bridge. Passwords are")
    );
    println!(
        "  {}",
        muted().apply_to("stored in plaintext in config.toml, like the rest of this file. The")
    );
    println!(
        "  {}",
        muted().apply_to("MeshCore Auth Token option never stores or transmits the node's private")
    );
    println!(
        "  {}",
        muted().apply_to("key -- only its public key and short-lived, on-device-signed tokens.")
    );

    let mut brokers = existing.to_vec();

    loop {
        let prompt = if brokers.is_empty() {
            "➕ Add an MQTT broker?"
        } else {
            "➕ Add another MQTT broker?"
        };
        let add_one = Confirm::with_theme(dtheme)
            .with_prompt(prompt)
            .default(false)
            .interact()?;
        if !add_one {
            break;
        }

        let name: String = Input::with_theme(dtheme)
            .with_prompt("🏷️  Broker name (internal identification)")
            .interact_text()?;

        let host: String = Input::with_theme(dtheme)
            .with_prompt("🌐 Broker host")
            .interact_text()?;

        let port: u16 = Input::with_theme(dtheme)
            .with_prompt("🔢 Broker port")
            .default(fez_mesh_controller_core::config::default_mqtt_port())
            .interact_text()?;

        let protocol_options = ["📡 TCP", "🔌 WebSocket"];
        let protocol_choice = Select::with_theme(dtheme)
            .with_prompt("🔗 Connection protocol")
            .items(&protocol_options)
            .default(0)
            .interact()?;
        let (transport_protocol, websocket_path) = if protocol_choice == 1 {
            let path: String = Input::with_theme(dtheme)
                .with_prompt("🔀 WebSocket path (broker-specific, e.g. /mqtt, /ws)")
                .default(fez_mesh_controller_core::config::default_mqtt_websocket_path())
                .interact_text()?;
            (MqttTransportProtocol::Websocket, path)
        } else {
            (
                MqttTransportProtocol::Tcp,
                fez_mesh_controller_core::config::default_mqtt_websocket_path(),
            )
        };

        let auth_options = [
            "🔑 Username/Password",
            "✍️  MeshCore Auth Token (device-signed, e.g. LetsMesh, MeshMapper)",
            "🕶️  Anonymous",
        ];
        let auth_choice = Select::with_theme(dtheme)
            .with_prompt("🔐 Authentication")
            .items(&auth_options)
            .default(0)
            .interact()?;
        let (username, password, auth_method) = match auth_choice {
            0 => {
                let username = ask_optional_text(dtheme, "👤 Username (leave empty for none)")?;
                let password = if username.is_some() {
                    let password: String = Password::with_theme(dtheme)
                        .with_prompt("🔑 Password (leave empty for none)")
                        .allow_empty_password(true)
                        .interact()?;
                    (!password.is_empty()).then_some(password)
                } else {
                    None
                };
                (username, password, MqttAuthMethod::Passwd)
            }
            1 => (None, None, MqttAuthMethod::Device),
            _ => (None, None, MqttAuthMethod::None),
        };

        let topic_prefix: String = Input::with_theme(dtheme)
            .with_prompt("📂 Topic prefix")
            .default(fez_mesh_controller_core::config::default_mqtt_topic_prefix())
            .interact_text()?;

        let status_topic: String = Input::with_theme(dtheme)
            .with_prompt("📍 Status topic route ({prefix}/{public_key} placeholders supported)")
            .default(fez_mesh_controller_core::config::default_mqtt_status_topic())
            .interact_text()?;

        let status_refresh_interval_secs: u32 = Input::with_theme(dtheme)
            .with_prompt("💓 Status heartbeat interval, in seconds (0 to disable)")
            .default(fez_mesh_controller_core::config::default_mqtt_status_refresh_interval_secs())
            .interact_text()?;

        let enable_high_level_messages = Confirm::with_theme(dtheme)
            .with_prompt("📤 Publish the decoded-event and status topics to this broker?")
            .default(fez_mesh_controller_core::config::default_mqtt_enable_high_level_messages())
            .interact()?;

        let enable_packet_trafic_messages = Confirm::with_theme(dtheme)
            .with_prompt("📦 Publish packet capture")
            .default(fez_mesh_controller_core::config::default_mqtt_enable_packet_trafic_messages())
            .interact()?;

        let tls_enabled = Confirm::with_theme(dtheme)
            .with_prompt("🔒 Enable TLS?")
            .default(false)
            .interact()?;

        let (tls_ca_cert, tls_client_cert, tls_client_key) = if tls_enabled {
            let ca_cert = ask_optional_path(
                dtheme,
                "📄 CA certificate path (leave empty to use the system trust store)",
            )?;
            let client_cert = ask_optional_path(
                dtheme,
                "📄 Client certificate path (leave empty to skip mutual TLS)",
            )?;
            let client_key = if client_cert.is_some() {
                ask_optional_path(dtheme, "🔑 Client private key path")?
            } else {
                None
            };
            (ca_cert, client_cert, client_key)
        } else {
            (None, None, None)
        };

        brokers.push(MqttBrokerConfig {
            name,
            host,
            port,
            username,
            password,
            auth_method,
            jwt_ttl_secs: fez_mesh_controller_core::config::default_mqtt_jwt_ttl_secs(),
            jwt_audience: None,
            topic_prefix,
            tls_enabled,
            tls_ca_cert,
            tls_client_cert,
            tls_client_key,
            status_refresh_interval_secs,
            enable_high_level_messages,
            enable_packet_trafic_messages,
            packet_trafic_topic: fez_mesh_controller_core::config::default_mqtt_packet_trafic_topic(
            ),
            enable_raw_messages: false,
            raw_topic: fez_mesh_controller_core::config::default_mqtt_raw_topic(),
            status_topic,
            transport_protocol,
            websocket_path,
        });
    }

    Ok(brokers)
}

fn ask_optional_text(dtheme: &ColorfulTheme, prompt: &str) -> anyhow::Result<Option<String>> {
    let value: String = Input::with_theme(dtheme)
        .with_prompt(prompt)
        .allow_empty(true)
        .interact_text()?;
    let value = value.trim();
    Ok((!value.is_empty()).then(|| value.to_string()))
}

fn ask_optional_path(dtheme: &ColorfulTheme, prompt: &str) -> anyhow::Result<Option<PathBuf>> {
    Ok(ask_optional_text(dtheme, prompt)?.map(PathBuf::from))
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

    // --- delete_outline_entry ------------------------------------------------

    #[test]
    fn delete_outline_entry_leaf_leaves_the_rest_untouched() {
        let mut o = outline(&[("World", 0), ("Europe", 1), ("France", 2), ("Asia", 1)]);

        delete_outline_entry(&mut o, 2); // France

        assert_eq!(o, outline(&[("World", 0), ("Europe", 1), ("Asia", 1)]));
    }

    #[test]
    fn delete_outline_entry_promotes_the_whole_subtree_one_level() {
        // World > Europe > France > Paris (Paris is a grandchild of Europe)
        let mut o = outline(&[("World", 0), ("Europe", 1), ("France", 2), ("Paris", 3)]);

        delete_outline_entry(&mut o, 1); // Europe

        // France (was Europe's direct child) is promoted to depth 1; Paris
        // (was France's child, a grandchild of Europe) is promoted to
        // depth 2 too, staying one level below France.
        assert_eq!(o, outline(&[("World", 0), ("France", 1), ("Paris", 2)]));
        assert_eq!(
            regions_from_outline(&o)[2].parent.as_deref(),
            Some("France")
        );
    }

    #[test]
    fn delete_outline_entry_deleting_a_root_promotes_children_to_top_level() {
        let mut o = outline(&[("World", 0), ("Europe", 1), ("Asia", 1)]);

        delete_outline_entry(&mut o, 0); // World

        assert_eq!(o, outline(&[("Europe", 0), ("Asia", 0)]));
    }
}
