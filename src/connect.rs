use crate::bridge;
use crate::noise;
use crate::protocol::{
    read_frame, write_frame, DeviceInfo, Message, Role, HELLO_TIMEOUT, PROTOCOL_VERSION,
    SESSION_ESTABLISH_TIMEOUT,
};
use crate::server;
use anyhow::{anyhow, Result};
use tokio::time::{timeout, Duration};

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

/// Client-side deadlines for connection establishment. TCP and Noise
/// handshakes are bounded inside `noise::client_connect`; these cover the
/// control frames that follow. Long-running data transfer is not bounded.
#[derive(Debug, Clone, Copy)]
pub struct ConnectTiming {
    /// Hello write/response and ListRequest write.
    pub hello_timeout: Duration,
    /// DeviceList, SessionAttach response, and Ready/Failed response.
    pub response_timeout: Duration,
}

impl Default for ConnectTiming {
    fn default() -> Self {
        Self {
            hello_timeout: HELLO_TIMEOUT,
            response_timeout: SESSION_ESTABLISH_TIMEOUT,
        }
    }
}

pub async fn list_devices(config: ListConfig) -> Result<Vec<DeviceInfo>> {
    list_devices_with_timing(config, ConnectTiming::default()).await
}

/// List devices with tunable deadlines for tests.
pub async fn list_devices_with_timing(
    config: ListConfig,
    timing: ConnectTiming,
) -> Result<Vec<DeviceInfo>> {
    let mut stream = noise::client_connect(&config.server, &config.server_key).await?;
    timeout(
        timing.hello_timeout,
        write_frame(
            &mut stream,
            &Message::Hello {
                version: PROTOCOL_VERSION,
                role: Role::Controller,
                token: config.token,
                device_id: None,
                features: Vec::new(),
            },
        ),
    )
    .await
    .map_err(|_| anyhow!("timed out writing hello to relay"))??;
    match timeout(timing.hello_timeout, read_frame(&mut stream))
        .await
        .map_err(|_| anyhow!("timed out waiting for relay hello response"))?
    {
        Ok(Message::HelloOk { .. }) => {}
        Ok(other) => return Err(anyhow!("relay did not accept hello: {other:?}")),
        Err(error) => return Err(error),
    }
    timeout(
        timing.hello_timeout,
        write_frame(&mut stream, &Message::ListRequest),
    )
    .await
    .map_err(|_| anyhow!("timed out writing list request"))??;

    match timeout(timing.response_timeout, read_frame(&mut stream))
        .await
        .map_err(|_| anyhow!("timed out waiting for relay device list"))?
    {
        Ok(Message::DeviceList { devices }) => Ok(devices),
        Ok(other) => Err(anyhow!("unexpected relay device-list response: {other:?}")),
        Err(error) => Err(error),
    }
}

pub async fn run(config: Config) -> Result<()> {
    run_with_timing(config, ConnectTiming::default()).await
}

/// Connect stdin/stdout to a device with tunable deadlines for tests.
pub async fn run_with_timing(config: Config, timing: ConnectTiming) -> Result<()> {
    let mut stream = noise::client_connect(&config.server, &config.server_key).await?;
    timeout(
        timing.hello_timeout,
        write_frame(
            &mut stream,
            &Message::Hello {
                version: PROTOCOL_VERSION,
                role: Role::Controller,
                token: config.token.clone(),
                device_id: None,
                features: Vec::new(),
            },
        ),
    )
    .await
    .map_err(|_| anyhow!("timed out writing hello to relay"))??;
    match timeout(timing.hello_timeout, read_frame(&mut stream))
        .await
        .map_err(|_| anyhow!("timed out waiting for relay hello response"))?
    {
        Ok(Message::HelloOk { .. }) => {}
        Ok(other) => return Err(anyhow!("relay did not accept hello: {other:?}")),
        Err(error) => return Err(error),
    }

    let session_id = server::session_id();
    timeout(
        timing.hello_timeout,
        write_frame(
            &mut stream,
            &Message::OpenRequest {
                target: config.target,
                session_id: session_id.clone(),
            },
        ),
    )
    .await
    .map_err(|_| anyhow!("timed out writing open request"))??;

    match timeout(timing.response_timeout, read_frame(&mut stream))
        .await
        .map_err(|_| anyhow!("timed out waiting for relay session response"))?
    {
        Ok(Message::Ready { session_id: id }) if id == session_id => {
            bridge::stdin_stdout(stream).await
        }
        Ok(Message::Failed {
            session_id: id,
            reason,
        }) if id == session_id => Err(anyhow!("relay refused SSH session: {reason}")),
        Ok(other) => Err(anyhow!("unexpected relay response: {other:?}")),
        Err(error) => Err(error),
    }
}
