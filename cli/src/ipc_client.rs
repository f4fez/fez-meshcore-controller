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

//! IPC connection (Unix socket) from the CLI to the daemon.

use std::path::Path;

use anyhow::{Context, Result};
use fez_mesh_controller_core::ipc::{ClientMessage, ServerMessage};
use futures::{SinkExt, StreamExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};

pub struct IpcConnection {
    reader: FramedRead<OwnedReadHalf, LinesCodec>,
    writer: FramedWrite<OwnedWriteHalf, LinesCodec>,
}

impl IpcConnection {
    pub async fn connect(path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(path)
            .await
            .with_context(|| format!("connecting to daemon via {}", path.display()))?;
        let (read_half, write_half) = stream.into_split();
        Ok(Self {
            reader: FramedRead::new(read_half, LinesCodec::new()),
            writer: FramedWrite::new(write_half, LinesCodec::new()),
        })
    }

    pub async fn recv(&mut self) -> Result<Option<ServerMessage>> {
        match self.reader.next().await {
            Some(Ok(line)) => Ok(Some(serde_json::from_str(&line)?)),
            Some(Err(err)) => Err(err.into()),
            None => Ok(None),
        }
    }

    pub async fn send(&mut self, msg: &ClientMessage) -> Result<()> {
        let text = serde_json::to_string(msg)?;
        self.writer.send(text).await?;
        Ok(())
    }
}
