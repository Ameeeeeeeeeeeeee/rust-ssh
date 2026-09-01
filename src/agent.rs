use crate::bootstrap;
use crate::bridge;
use crate::noise;
use crate::protocol::{read_frame, write_frame, Message, Role, PROTOCOL_VERSION};
use anyhow::{anyhow, Result};
use std::net::SocketAddr;
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::time::{sleep, Duration};
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct Config {
    pub server: String,
    pub server_key: [u8; crate::identity::STATIC_KEY_SIZE],
    pub token: String,
    pub device_id: String,
    pub target: String,
}

pub async fn run_with_bootstrap(
    bootstrap: bootstrap::Config,
    device_id: String,
    target: String,
) -> Result<()> {
    run(Config {
        server: bootstrap.server,
        server_key: bootstrap.server_key,
        token: bootstrap.token,
        device_id,
        target,
    })
    .await
}

pub async fn run(config: Config) -> Result<()> {
    let (_stop_tx, stop_rx) = oneshot::channel();
    run_until_stopped(config, stop_rx).await
}

pub async fn run_until_stopped(config: Config, mut stop: oneshot::Receiver<()>) -> Result<()> {
    validate_target(&config.target)?;
    let mut delay = Duration::from_secs(1);
    loop {
        let run_result = tokio::select! {
            result = run_once(&config) => Some(result),
            _ = &mut stop => return Ok(()),
        };
        match run_result.expect("agent run result must be present") {
            Ok(()) => {
                warn!("agent session ended; reconnecting");
            }
            Err(error) => {
                warn!(%error, "agent connection failed; reconnecting");
            }
        }
        tokio::select! {
            _ = sleep(delay) => {}
            _ = &mut stop => return Ok(()),
        }
        delay = std::cmp::min(delay.saturating_mul(2), Duration::from_secs(30));
    }
}

fn validate_target(target: &str) -> Result<()> {
    let address: SocketAddr = target
        .parse()
        .map_err(|_| anyhow!("SSH target must be a loopback IP address with port"))?;
    if !address.ip().is_loopback() {
        return Err(anyhow!(
            "SSH target must stay on loopback (127.0.0.1 or ::1)"
        ));
    }
    Ok(())
}

async fn run_once(config: &Config) -> Result<()> {
    let mut stream = noise::client_connect(&config.server, &config.server_key).await?;
    write_frame(
        &mut stream,
        &Message::Hello {
            version: PROTOCOL_VERSION,
            role: Role::Agent,
            token: config.token.clone(),
            device_id: Some(config.device_id.clone()),
        },
    )
    .await?;
    expect_hello_ok(&mut stream).await?;
    info!(device = %config.device_id, target = %config.target, "agent connected to relay");

    loop {
        match read_frame(&mut stream).await? {
            Message::Open { session_id } => {
                let local = match TcpStream::connect(&config.target).await {
                    Ok(local) => local,
                    Err(error) => {
                        write_frame(
                            &mut stream,
                            &Message::Failed {
                                session_id,
                                reason: format!("cannot connect to local SSH target: {error}"),
                            },
                        )
                        .await?;
                        continue;
                    }
                };
                let session_id_for_log = session_id.clone();
                write_frame(&mut stream, &Message::Ready { session_id }).await?;
                info!(session = %session_id_for_log, "SSH session accepted");
                bridge::bidirectional(stream, local).await?;
                return Ok(());
            }
            other => return Err(anyhow!("unexpected relay message: {other:?}")),
        }
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
