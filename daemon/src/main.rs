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

mod command;
mod mesh_task;
mod server;
mod state;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use daemonize::Daemonize;
use fez_mesh_controller_core::Config;
use state::AppState;
use tracing::{error, info};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

/// fez-mesh-controller daemon: connects to the MeshCore node and serves its
/// state to CLI clients over a Unix socket.
#[derive(Parser)]
#[command(name = "fez-mesh-controller-daemon", version)]
struct Cli {
    /// Path to an alternate configuration file.
    #[arg(long, value_name = "CONFIG")]
    config: Option<PathBuf>,

    /// Run in the background, detached from the terminal. Logs are then
    /// written to daily-rotating files under the configured log directory
    /// instead of the console.
    #[arg(long)]
    daemon: bool,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let config_path = cli
        .config
        .clone()
        .unwrap_or_else(fez_mesh_controller_core::config::config_path);
    let config = Config::load_from(&config_path).map_err(|err| {
        anyhow::anyhow!(
            "failed to load configuration from {}: {err}\nrun `fez-mesh-controller` (the CLI) to run the setup wizard",
            config_path.display()
        )
    })?;

    if cli.daemon {
        run_in_background(&config.daemon.log_dir)?;
    }

    // The tracing worker guard must stay alive for the process lifetime: it
    // flushes buffered log lines on drop, so it is kept in this local
    // binding until `main` returns.
    let _guard = if cli.daemon {
        init_file_logging(&config.daemon.log_level, &config.daemon.log_dir)?
    } else {
        init_console_logging(&config.daemon.log_level)
    };

    info!(
        node = %config.node_label,
        connection = %config.connection,
        background = cli.daemon,
        "starting fez-mesh-controller-daemon"
    );

    tokio::runtime::Runtime::new()?.block_on(run(config, config_path))
}

async fn run(config: Config, config_path: PathBuf) -> anyhow::Result<()> {
    let (command_tx, command_rx) = tokio::sync::mpsc::channel(8);
    let refresh_interval = Duration::from_secs(config.daemon.refresh_interval_secs.max(1));
    let connection = config.connection.clone();
    let socket_path = config.daemon.socket_path.clone();

    let state = Arc::new(AppState::new(command_tx, config, config_path));

    let mesh_state = state.clone();
    let mesh_handle = tokio::spawn(mesh_task::run(
        connection,
        refresh_interval,
        command_rx,
        mesh_state,
    ));

    let server_state = state.clone();
    let server_socket_path = socket_path.clone();
    let server_handle =
        tokio::spawn(async move { server::run(&server_socket_path, server_state).await });

    tokio::select! {
        res = server_handle => {
            if let Err(err) = res.expect("IPC server task") {
                error!(error = %err, "IPC server stopped");
            }
        }
        _ = mesh_handle => {
            error!("mesh connection task stopped unexpectedly");
        }
        _ = tokio::signal::ctrl_c() => {
            info!("shutdown signal received, stopping daemon");
        }
    }

    let _ = std::fs::remove_file(&socket_path);
    Ok(())
}

/// Forks the process into the background and detaches it from the
/// controlling terminal. Must run before the Tokio runtime (and its worker
/// threads) is started, since forking a multi-threaded process is unsafe.
fn run_in_background(log_dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(log_dir).map_err(|err| {
        anyhow::anyhow!(
            "failed to create log directory {}: {err}",
            log_dir.display()
        )
    })?;
    let working_directory = std::env::current_dir()?;

    Daemonize::new()
        .working_directory(working_directory)
        .start()
        .map_err(|err| anyhow::anyhow!("failed to move to the background: {err}"))
}

fn init_console_logging(default_level: &str) -> Option<WorkerGuard> {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
    tracing_subscriber::fmt().with_env_filter(filter).init();
    None
}

fn init_file_logging(default_level: &str, log_dir: &Path) -> anyhow::Result<Option<WorkerGuard>> {
    let file_appender = tracing_appender::rolling::daily(log_dir, "fez-mesh-controller-daemon.log");
    let (writer, guard) = tracing_appender::non_blocking(file_appender);

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(writer)
        .init();

    Ok(Some(guard))
}
