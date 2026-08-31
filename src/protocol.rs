use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const MAX_FRAME_SIZE: u32 = 64 * 1024;
pub const PROTOCOL_VERSION: u8 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Agent,
    Controller,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceInfo {
    pub device_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Hello {
        version: u8,
        role: Role,
        token: String,
        device_id: Option<String>,
    },
    HelloOk,
    ListRequest,
    DeviceList {
        devices: Vec<DeviceInfo>,
    },
    OpenRequest {
        target: String,
        session_id: String,
    },
    Open {
        session_id: String,
    },
    Ready {
        session_id: String,
    },
    Failed {
        session_id: String,
        reason: String,
    },
}

pub async fn read_frame<R>(reader: &mut R) -> Result<Message>
where
    R: AsyncRead + Unpin,
{
    let length = reader
        .read_u32()
        .await
        .context("reading control frame length")?;
    if length == 0 || length > MAX_FRAME_SIZE {
        return Err(anyhow!("invalid control frame length: {length}"));
    }

    let mut payload = vec![0_u8; length as usize];
    reader
        .read_exact(&mut payload)
        .await
        .context("reading control frame payload")?;
    serde_json::from_slice(&payload).context("decoding control frame")
}

pub async fn write_frame<W>(writer: &mut W, message: &Message) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let payload = serde_json::to_vec(message).context("encoding control frame")?;
    if payload.is_empty() || payload.len() > MAX_FRAME_SIZE as usize {
        return Err(anyhow!("control frame is too large"));
    }

    writer
        .write_u32(payload.len() as u32)
        .await
        .context("writing control frame length")?;
    writer
        .write_all(&payload)
        .await
        .context("writing control frame payload")?;
    writer.flush().await.context("flushing control frame")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn frame_round_trip() {
        let (mut left, mut right) = duplex(1024);
        let message = Message::Hello {
            version: PROTOCOL_VERSION,
            role: Role::Controller,
            token: "secret".to_owned(),
            device_id: None,
        };
        let expected = message.clone();

        let writer = tokio::spawn(async move {
            write_frame(&mut left, &message).await.unwrap();
        });
        let decoded = read_frame(&mut right).await.unwrap();
        writer.await.unwrap();

        assert_eq!(decoded, expected);
    }
}
