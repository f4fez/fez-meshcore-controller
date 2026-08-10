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

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use fez_mesh_controller_core::ipc::{ClientMessage, ServerMessage, PROTOCOL_VERSION};
use futures::{SinkExt, StreamExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};
use tracing::{debug, info, warn};

use crate::state::AppState;

/// Starts the IPC server: listens on the Unix socket and serves one client
/// per accepted connection until the daemon shuts down.
pub async fn run(socket_path: &Path, state: Arc<AppState>) -> Result<()> {
    if socket_path.exists() {
        std::fs::remove_file(socket_path)
            .with_context(|| format!("removing existing socket {}", socket_path.display()))?;
    }
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }

    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("binding socket {}", socket_path.display()))?;
    info!(socket = %socket_path.display(), "IPC server listening");

    loop {
        let (stream, _addr) = listener.accept().await.context("accepting IPC client")?;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_client(stream, state).await {
                debug!(error = %err, "IPC client connection closed");
            }
        });
    }
}

async fn handle_client(stream: UnixStream, state: Arc<AppState>) -> Result<()> {
    let (read_half, write_half) = stream.into_split();
    let mut reader = FramedRead::new(read_half, LinesCodec::new());
    let mut writer = FramedWrite::new(write_half, LinesCodec::new());
    let mut events_rx = state.events_tx.subscribe();

    send(
        &mut writer,
        &ServerMessage::Hello {
            version: PROTOCOL_VERSION,
        },
    )
    .await?;
    send(
        &mut writer,
        &ServerMessage::Snapshot(current_snapshot(&state).await),
    )
    .await?;

    loop {
        tokio::select! {
            event = events_rx.recv() => {
                match event {
                    Ok(event) => send(&mut writer, &ServerMessage::Event(event)).await?,
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(skipped, "IPC client too slow, events dropped");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            line = reader.next() => {
                match line {
                    Some(Ok(text)) => {
                        if let Ok(ClientMessage::RequestSnapshot) = serde_json::from_str(&text) {
                            send(&mut writer, &ServerMessage::Snapshot(current_snapshot(&state).await)).await?;
                        }
                    }
                    Some(Err(err)) => {
                        debug!(error = %err, "IPC client read error");
                        break;
                    }
                    None => break,
                }
            }
        }
    }

    Ok(())
}

async fn current_snapshot(state: &Arc<AppState>) -> fez_mesh_controller_core::ipc::Snapshot {
    let mut snap = state.snapshot.read().await.clone();
    snap.uptime_secs = state.uptime_secs();
    snap
}

async fn send<W>(writer: &mut FramedWrite<W, LinesCodec>, msg: &ServerMessage) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let text = serde_json::to_string(msg).context("serializing IPC message")?;
    writer.send(text).await.context("sending IPC message")?;
    Ok(())
}
