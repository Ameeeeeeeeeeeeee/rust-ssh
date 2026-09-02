use crate::bootstrap;
use crate::bridge;
use crate::noise;
use crate::protocol::{read_frame, write_frame, Message, Role, PROTOCOL_VERSION};
use anyhow::{anyhow, Result};
use std::net::SocketAddr;
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio::time::{sleep, Duration};
use tracing::{info, warn};

const INITIAL_RECONNECT_DELAY: Duration = Duration::from_secs(1);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub enum Status {
    Connecting,
    Connected,
    Retrying(String),
    Stopped,
    Failed(String),
}

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
    if let Some(pairing_device_id) = bootstrap.device_id.as_deref() {
        if pairing_device_id != device_id {
            return Err(anyhow!("device ID does not match the device pairing code"));
        }
    }
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

pub async fn run_until_stopped(config: Config, stop: oneshot::Receiver<()>) -> Result<()> {
    run_until_stopped_inner(config, stop, None).await
}

pub async fn run_until_stopped_with_status(
    config: Config,
    stop: oneshot::Receiver<()>,
    status: std::sync::mpsc::Sender<Status>,
) -> Result<()> {
    run_until_stopped_inner(config, stop, Some(status)).await
}

async fn run_until_stopped_inner(
    config: Config,
    mut stop: oneshot::Receiver<()>,
    status: Option<std::sync::mpsc::Sender<Status>>,
) -> Result<()> {
    if let Err(error) = validate_target(&config.target) {
        report_status(&status, Status::Failed(error.to_string()));
        return Err(error);
    }
    // Network and relay failures are retryable indefinitely while the stop
    // channel remains open. Backoff prevents a disconnected client from
    // hammering the server, while an established connection retries quickly.
    let mut delay = INITIAL_RECONNECT_DELAY;
    loop {
        report_status(&status, Status::Connecting);
        let mut connected = false;
        let run_result = tokio::select! {
            result = run_once(&config, status.as_ref(), &mut connected) => Some(result),
            _ = &mut stop => {
                report_status(&status, Status::Stopped);
                return Ok(());
            },
        };
        match run_result.expect("agent run result must be present") {
            Ok(()) => {
                warn!("agent session ended; reconnecting");
                report_status(&status, Status::Retrying("连接已断开，正在重试".to_owned()));
            }
            Err(error) => {
                warn!(%error, "agent connection failed; reconnecting");
                report_status(&status, Status::Retrying(error.to_string()));
            }
        }

        let retry_delay = if connected {
            INITIAL_RECONNECT_DELAY
        } else {
            delay
        };
        tokio::select! {
            _ = sleep(retry_delay) => {}
            _ = &mut stop => {
                report_status(&status, Status::Stopped);
                return Ok(());
            },
        }
        delay = next_reconnect_delay(delay, connected);
    }
}

fn next_reconnect_delay(current: Duration, was_connected: bool) -> Duration {
    if was_connected {
        INITIAL_RECONNECT_DELAY
    } else {
        std::cmp::min(current.saturating_mul(2), MAX_RECONNECT_DELAY)
    }
}

fn report_status(status: &Option<std::sync::mpsc::Sender<Status>>, update: Status) {
    if let Some(sender) = status {
        let _ = sender.send(update);
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

async fn run_once(
    config: &Config,
    status: Option<&std::sync::mpsc::Sender<Status>>,
    connected: &mut bool,
) -> Result<()> {
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
    *connected = true;
    if let Some(sender) = status {
        let _ = sender.send(Status::Connected);
    }
    info!(device = %config.device_id, target = %config.target, "agent connected to relay");

    let mut sessions = JoinSet::new();
    loop {
        tokio::select! {
            message = read_frame(&mut stream) => {
                match message? {
                    Message::Open { session_id } => {
                        let session_config = config.clone();
                        sessions.spawn(async move {
                            if let Err(error) = run_session(session_config, session_id).await {
                                warn!(%error, "SSH session ended with an error");
                            }
                        });
                    }
                    other => return Err(anyhow!("unexpected relay message: {other:?}")),
                }
            }
            result = sessions.join_next(), if !sessions.is_empty() => {
                if let Some(Err(error)) = result {
                    warn!(%error, "SSH session task failed");
                }
            }
        }
    }
}

async fn run_session(config: Config, session_id: String) -> Result<()> {
    let mut stream = noise::client_connect(&config.server, &config.server_key).await?;
    write_frame(
        &mut stream,
        &Message::Hello {
            version: PROTOCOL_VERSION,
            role: Role::AgentSession,
            token: config.token,
            device_id: Some(config.device_id.clone()),
        },
    )
    .await?;
    expect_hello_ok(&mut stream).await?;
    write_frame(
        &mut stream,
        &Message::SessionAttach {
            session_id: session_id.clone(),
        },
    )
    .await?;

    match read_frame(&mut stream).await? {
        Message::SessionAccepted { session_id: id } if id == session_id => {}
        Message::Failed {
            session_id: id,
            reason,
        } if id == session_id => return Err(anyhow!("relay refused SSH session: {reason}")),
        other => return Err(anyhow!("unexpected relay session response: {other:?}")),
    }

    let local = match TcpStream::connect(&config.target).await {
        Ok(local) => local,
        Err(error) => {
            let reason = format!("cannot connect to local SSH target: {error}");
            write_frame(
                &mut stream,
                &Message::Failed {
                    session_id,
                    reason: reason.clone(),
                },
            )
            .await?;
            return Err(anyhow!(reason));
        }
    };

    let session_id_for_log = session_id.clone();
    write_frame(&mut stream, &Message::Ready { session_id }).await?;
    info!(session = %session_id_for_log, "SSH session accepted");
    bridge::bidirectional(stream, local).await
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

#[cfg(test)]
mod tests {
    use super::{next_reconnect_delay, INITIAL_RECONNECT_DELAY, MAX_RECONNECT_DELAY};
    use std::time::Duration;

    #[test]
    fn reconnect_backoff_is_bounded_but_never_stops() {
        let mut delay = INITIAL_RECONNECT_DELAY;
        let mut delays = Vec::new();
        for _ in 0..8 {
            delays.push(delay);
            delay = next_reconnect_delay(delay, false);
        }

        assert_eq!(
            delays,
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(16),
                Duration::from_secs(30),
                Duration::from_secs(30),
                Duration::from_secs(30),
            ]
        );
        assert_eq!(delay, MAX_RECONNECT_DELAY);
    }

    #[test]
    fn established_connection_reconnects_with_initial_delay() {
        assert_eq!(
            next_reconnect_delay(Duration::from_secs(30), true),
            INITIAL_RECONNECT_DELAY
        );
    }
}
