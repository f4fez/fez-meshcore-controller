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
        None => tui::run(&config).await,
    }
}
