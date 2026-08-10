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

pub mod app;
mod ui;

use std::io::Stdout;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, ExecutableCommand};
use fez_mesh_controller_core::ipc::{ClientMessage, ServerMessage};
use fez_mesh_controller_core::Config;
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::ipc_client::IpcConnection;
use app::App;

type Term = Terminal<CrosstermBackend<Stdout>>;

enum UiEvent {
    DaemonConnected,
    DaemonDisconnected,
    Server(ServerMessage),
}

/// Launches the full-screen real-time dashboard.
pub async fn run(config: &Config) -> Result<()> {
    let mut terminal = setup_terminal()?;

    let (ui_tx, mut ui_rx) = mpsc::channel::<UiEvent>(256);
    let (cmd_tx, cmd_rx) = mpsc::channel::<ClientMessage>(8);
    tokio::spawn(ipc_task(config.daemon.socket_path.clone(), ui_tx, cmd_rx));

    let mut app = App::new();
    let mut term_events = EventStream::new();

    let result = run_loop(
        &mut terminal,
        &mut app,
        &mut ui_rx,
        &mut term_events,
        &cmd_tx,
    )
    .await;

    restore_terminal(&mut terminal)?;
    result
}

async fn run_loop(
    terminal: &mut Term,
    app: &mut App,
    ui_rx: &mut mpsc::Receiver<UiEvent>,
    term_events: &mut EventStream,
    cmd_tx: &mpsc::Sender<ClientMessage>,
) -> Result<()> {
    loop {
        terminal.draw(|f| ui::draw(f, app))?;

        tokio::select! {
            Some(event) = ui_rx.recv() => apply_ui_event(app, event),
            Some(Ok(event)) = term_events.next() => {
                if let Event::Key(key) = event {
                    if key.kind == KeyEventKind::Press {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
                            KeyCode::Char('r') => {
                                let _ = cmd_tx.send(ClientMessage::RequestSnapshot).await;
                            }
                            KeyCode::Down => app.select_next_contact(),
                            KeyCode::Up => app.select_prev_contact(),
                            _ => {}
                        }
                    }
                }
            }
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

fn apply_ui_event(app: &mut App, event: UiEvent) {
    match event {
        UiEvent::DaemonConnected => app.daemon_connected = true,
        UiEvent::DaemonDisconnected => app.daemon_connected = false,
        UiEvent::Server(ServerMessage::Snapshot(snapshot)) => app.snapshot = snapshot,
        UiEvent::Server(ServerMessage::Event(event)) => app.push_event(event),
        UiEvent::Server(ServerMessage::Error(message)) => app.last_status = Some(message),
        UiEvent::Server(ServerMessage::Hello { .. }) => {}
    }
}

/// Background task: maintains the IPC connection to the daemon (with
/// automatic reconnection) and bridges the socket with the UI channels.
async fn ipc_task(
    socket_path: PathBuf,
    ui_tx: mpsc::Sender<UiEvent>,
    mut cmd_rx: mpsc::Receiver<ClientMessage>,
) {
    loop {
        let mut conn = match IpcConnection::connect(&socket_path).await {
            Ok(conn) => conn,
            Err(_) => {
                let _ = ui_tx.send(UiEvent::DaemonDisconnected).await;
                tokio::time::sleep(Duration::from_secs(3)).await;
                continue;
            }
        };

        if ui_tx.send(UiEvent::DaemonConnected).await.is_err() {
            return;
        }

        loop {
            tokio::select! {
                msg = conn.recv() => {
                    match msg {
                        Ok(Some(msg)) => {
                            if ui_tx.send(UiEvent::Server(msg)).await.is_err() {
                                return;
                            }
                        }
                        _ => break,
                    }
                }
                Some(cmd) = cmd_rx.recv() => {
                    if conn.send(&cmd).await.is_err() {
                        break;
                    }
                }
            }
        }

        let _ = ui_tx.send(UiEvent::DaemonDisconnected).await;
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

fn setup_terminal() -> Result<Term> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal(terminal: &mut Term) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}
