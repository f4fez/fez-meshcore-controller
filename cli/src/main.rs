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

mod format;
mod ipc_client;
mod repeater;
mod status;
mod theme;
mod tui;
mod wizard;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use fez_mesh_controller_core::Config;

#[derive(Parser)]
#[command(
    name = "fez-mesh-controller",
    version,
    about = "Controller and monitor for a MeshCore network 🛰️"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Alternate path to the configuration file.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// (Re)runs the interactive setup wizard
    Setup,
    /// Prints a one-off snapshot of the network state, then exits
    Status,
    /// Manage repeaters known to the node (same actions as the TUI's
    /// contact list), then exits
    Repeater {
        #[command(subcommand)]
        action: RepeaterCommand,
    },
}

#[derive(Subcommand)]
enum RepeaterCommand {
    /// List known repeaters/contacts and their status
    List,
    /// Declares a new repeater directly from its full public key, without
    /// requiring it to have been heard on the mesh first
    Add {
        /// Display name for this repeater
        name: String,
        /// Full public key (64 hex characters)
        public_key_hex: String,
        /// Also mark it as managed
        #[arg(short, long)]
        manage: bool,
    },
    /// Mark a repeater as managed, registering it with the node first if
    /// needed (fails if it hasn't been heard on the mesh yet)
    Manage {
        /// Public key prefix (hex, see `repeater list`); may be
        /// abbreviated as long as it's unambiguous
        prefix: String,
        /// Display name to use (defaults to the contact's current name)
        #[arg(long)]
        name: Option<String>,
    },
    /// Unmark a repeater as managed (it stays registered with the node)
    Unmanage {
        /// Public key prefix (hex, see `repeater list`); may be
        /// abbreviated as long as it's unambiguous
        prefix: String,
    },
    /// Permanently remove a repeater from the node's contact list
    Remove {
        /// Public key prefix (hex, see `repeater list`); may be
        /// abbreviated as long as it's unambiguous
        prefix: String,
        /// Skip the confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    theme::print_banner();

    let config_path = cli
        .config
        .clone()
        .unwrap_or_else(fez_mesh_controller_core::config::config_path);

    let config = match &cli.command {
        Some(Command::Setup) => {
            let existing = Config::load_from(&config_path).ok();
            let config = wizard::run(existing.as_ref()).await?;
            config.save_to(&config_path)?;
            theme::success_line(&format!("configuration saved to {}", config_path.display()));
            config
        }
        _ if config_path.exists() => Config::load_from(&config_path)?,
        _ => {
            theme::info_line("no configuration found, launching the setup wizard 🧙");
            let config = wizard::run(None).await?;
            config.save_to(&config_path)?;
            theme::success_line(&format!("configuration saved to {}", config_path.display()));
            config
        }
    };

    match cli.command {
        Some(Command::Setup) => Ok(()),
        Some(Command::Status) => status::run(&config).await,
        Some(Command::Repeater { action }) => match action {
            RepeaterCommand::List => repeater::list(&config).await,
            RepeaterCommand::Add {
                name,
                public_key_hex,
                manage,
            } => repeater::add(&config, &name, &public_key_hex, manage).await,
            RepeaterCommand::Manage { prefix, name } => {
                repeater::manage(&config, &prefix, name).await
            }
            RepeaterCommand::Unmanage { prefix } => repeater::unmanage(&config, &prefix).await,
            RepeaterCommand::Remove { prefix, yes } => {
                repeater::remove(&config, &prefix, yes).await
            }
        },
        None => tui::run(&config).await,
    }
}
