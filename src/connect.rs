use crate::bridge;
use crate::noise;
use crate::protocol::{read_frame, write_frame, DeviceInfo, Message, Role, PROTOCOL_VERSION};
use crate::server;
use anyhow::{anyhow, Result};

#[derive(Debug, Clone)]
pub struct Config {
    pub server: String,
    pub server_key: [u8; crate::identity::STATIC_KEY_SIZE],
    pub token: String,
    pub target: String,
}

#[derive(Debug, Clone)]
pub struct ListConfig {
    pub server: String,
    pub server_key: [u8; crate::identity::STATIC_KEY_SIZE],
    pub token: String,
}

pub async fn list_devices(config: ListConfig) -> Result<Vec<DeviceInfo>> {
    let mut stream = noise::client_connect(&config.server, &config.server_key).await?;
    write_frame(
        &mut stream,
        &Message::Hello {
            version: PROTOCOL_VERSION,
            role: Role::Controller,
            token: config.token,
            device_id: None,
        },
    )
    .await?;
    expect_hello_ok(&mut stream).await?;
    write_frame(&mut stream, &Message::ListRequest).await?;

    match read_frame(&mut stream).await? {
        Message::DeviceList { devices } => Ok(devices),
        other => Err(anyhow!("unexpected relay device-list response: {other:?}")),
    }
}

pub async fn run(config: Config) -> Result<()> {
    let mut stream = noise::client_connect(&config.server, &config.server_key).await?;
    write_frame(
        &mut stream,
        &Message::Hello {
            version: PROTOCOL_VERSION,
            role: Role::Controller,
            token: config.token.clone(),
            device_id: None,
        },
    )
    .await?;
    expect_hello_ok(&mut stream).await?;

    let session_id = server::session_id();
    write_frame(
        &mut stream,
        &Message::OpenRequest {
            target: config.target,
            session_id: session_id.clone(),
        },
    )
    .await?;

    match read_frame(&mut stream).await? {
        Message::Ready { session_id: id } if id == session_id => bridge::stdin_stdout(stream).await,
        Message::Failed {
            session_id: id,
            reason,
        } if id == session_id => Err(anyhow!("relay refused SSH session: {reason}")),
        other => Err(anyhow!("unexpected relay response: {other:?}")),
    }
}

async fn expect_hello_ok<S>(stream: &mut S) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    match read_frame(stream).await? {
        Message::HelloOk => Ok(()),
        other => Err(anyhow!("relay did not accept hello: {other:?}")),
    }
}
