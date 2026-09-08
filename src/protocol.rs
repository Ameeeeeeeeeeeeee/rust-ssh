use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const MAX_FRAME_SIZE: u32 = 64 * 1024;
pub const PROTOCOL_VERSION: u8 = 4;
/// Capability advertised in Hello/HelloOk by v0.5.6+ endpoints. Only the
/// agent's long-lived control connection uses heartbeats; old peers that do
/// not advertise the feature keep working without them.
pub const HEARTBEAT_FEATURE: &str = "heartbeat";
/// Absolute deadline for the first Hello frame after the Noise handshake and
/// for each endpoint's authentication response.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// Deadline for completing a new SSH session (attach, local connect, Ready).
/// Long-running data transfer is never bound by this.
pub const SESSION_ESTABLISH_TIMEOUT: Duration = Duration::from_secs(15);

/// Heartbeat policy for the agent control channel. Shared by the agent and
/// the server so both ends agree on the same liveness window; tests use
/// shorter values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeartbeatTiming {
    /// How often the agent sends a Ping on the control channel.
    pub ping_interval: Duration,
    /// No valid response within this window marks the control channel dead.
    pub dead_after: Duration,
    /// Heartbeat and control writes must finish within this budget so a
    /// blackholed TCP connection cannot block the writer forever.
    pub write_timeout: Duration,
}

impl Default for HeartbeatTiming {
    fn default() -> Self {
        Self {
            ping_interval: Duration::from_secs(15),
            dead_after: Duration::from_secs(45),
            write_timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Agent,
    AgentSession,
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
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        features: Vec<String>,
    },
    HelloOk {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        features: Vec<String>,
    },
    SessionAttach {
        session_id: String,
    },
    SessionAccepted {
        session_id: String,
    },
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
    /// Control-channel heartbeat; only used between v0.5.6+ agent and server
    /// after both advertise the "heartbeat" feature, never on data sessions.
    Ping,
    Pong,
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
            features: vec![HEARTBEAT_FEATURE.to_owned()],
        };
        let expected = message.clone();

        let writer = tokio::spawn(async move {
            write_frame(&mut left, &message).await.unwrap();
        });
        let decoded = read_frame(&mut right).await.unwrap();
        writer.await.unwrap();

        assert_eq!(decoded, expected);
    }

    #[test]
    fn heartbeat_messages_round_trip() {
        let encoded = serde_json::to_value(&Message::Ping).unwrap();
        assert_eq!(encoded, serde_json::json!({ "type": "ping" }));
        let decoded: Message = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, Message::Ping);
    }

    #[test]
    fn hello_without_features_still_parses_like_old_peers() {
        // v0.5.5 and earlier sent Hello/HelloOk without a features field.
        let hello: Message = serde_json::from_value(serde_json::json!({
            "type": "hello",
            "version": PROTOCOL_VERSION,
            "role": "agent",
            "token": "t".repeat(32),
            "device_id": "rssh-0123456789abcdef0123456789abcdef",
        }))
        .unwrap();
        assert_eq!(
            hello,
            Message::Hello {
                version: PROTOCOL_VERSION,
                role: Role::Agent,
                token: "t".repeat(32),
                device_id: Some("rssh-0123456789abcdef0123456789abcdef".to_owned()),
                features: Vec::new(),
            }
        );

        let hello_ok: Message =
            serde_json::from_value(serde_json::json!({ "type": "hello_ok" })).unwrap();
        assert_eq!(
            hello_ok,
            Message::HelloOk {
                features: Vec::new()
            }
        );
    }

    #[test]
    fn empty_features_are_omitted_from_the_wire() {
        // A v0.5.6 endpoint talking to an old peer must keep sending the old
        // plain hello_ok shape.
        let encoded = serde_json::to_value(&Message::HelloOk {
            features: Vec::new(),
        })
        .unwrap();
        assert_eq!(encoded, serde_json::json!({ "type": "hello_ok" }));
    }
}
