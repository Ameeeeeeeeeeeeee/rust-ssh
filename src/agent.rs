use crate::bootstrap;
use crate::bridge;
use crate::noise;
use crate::protocol::{
    read_frame, write_frame, HeartbeatTiming, Message, Role, HEARTBEAT_FEATURE, HELLO_TIMEOUT,
    PROTOCOL_VERSION, SESSION_ESTABLISH_TIMEOUT,
};
use anyhow::{anyhow, bail, Result};
use std::net::SocketAddr;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use tokio::time::{sleep, sleep_until, timeout, Duration, Instant};
use tracing::{info, warn};

const INITIAL_RECONNECT_DELAY: Duration = Duration::from_secs(1);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);
/// Control frames travel from the reader task to the control loop through a
/// small bounded channel; backpressure also paces the relay.
const CONTROL_FRAME_CHANNEL_CAPACITY: usize = 16;

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
    stop: oneshot::Receiver<()>,
    status: Option<std::sync::mpsc::Sender<Status>>,
) -> Result<()> {
    run_until_stopped_with_timing(config, stop, status, HeartbeatTiming::default()).await
}

/// Supervisor with tunable heartbeat timing for tests.
pub async fn run_until_stopped_with_timing(
    config: Config,
    mut stop: oneshot::Receiver<()>,
    status: Option<std::sync::mpsc::Sender<Status>>,
    timing: HeartbeatTiming,
) -> Result<()> {
    if let Err(error) = validate_target(&config.target) {
        report_status(&status, Status::Failed(error.to_string()));
        return Err(error);
    }
    // Established SSH sessions live in the supervisor, not inside a single
    // control connection: a dead control channel reconnects without tearing
    // down sessions that already carry data. Dropping the JoinSet on stop
    // aborts every session, so a user stop still terminates all of them.
    let mut sessions = JoinSet::new();
    // Network and relay failures are retryable indefinitely while the stop
    // channel remains open. Backoff prevents a disconnected client from
    // hammering the server, while an established connection retries quickly.
    let mut delay = INITIAL_RECONNECT_DELAY;
    loop {
        report_status(&status, Status::Connecting);
        let mut connected = false;
        let run_result = tokio::select! {
            result = run_control(&config, status.as_ref(), &mut connected, &mut sessions, timing) => Some(result),
            _ = &mut stop => {
                report_status(&status, Status::Stopped);
                sessions.shutdown().await;
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
                sessions.shutdown().await;
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

/// Connect the control channel, negotiate heartbeat support, and run it until
/// it fails. Existing sessions in `sessions` are left untouched on failure.
async fn run_control(
    config: &Config,
    status: Option<&std::sync::mpsc::Sender<Status>>,
    connected: &mut bool,
    sessions: &mut JoinSet<()>,
    timing: HeartbeatTiming,
) -> Result<()> {
    let mut stream = noise::client_connect(&config.server, &config.server_key).await?;
    timeout(
        timing.write_timeout,
        write_frame(
            &mut stream,
            &Message::Hello {
                version: PROTOCOL_VERSION,
                role: Role::Agent,
                token: config.token.clone(),
                device_id: Some(config.device_id.clone()),
                features: vec![HEARTBEAT_FEATURE.to_owned()],
            },
        ),
    )
    .await
    .map_err(|_| anyhow!("timed out writing hello to relay"))??;
    let heartbeat = match timeout(HELLO_TIMEOUT, read_frame(&mut stream))
        .await
        .map_err(|_| anyhow!("timed out waiting for relay hello response"))?
    {
        Ok(Message::HelloOk { features }) => features.iter().any(|f| f == HEARTBEAT_FEATURE),
        Ok(other) => bail!("relay did not accept hello: {other:?}"),
        Err(error) => return Err(error),
    };
    *connected = true;
    if let Some(sender) = status {
        let _ = sender.send(Status::Connected);
    }
    info!(
        device = %config.device_id,
        target = %config.target,
        heartbeat,
        "agent connected to relay"
    );
    run_control_stream(stream, config, sessions, timing, heartbeat).await
}

async fn run_control_stream<S>(
    stream: S,
    config: &Config,
    sessions: &mut JoinSet<()>,
    timing: HeartbeatTiming,
    heartbeat: bool,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(stream);
    // A dedicated reader task owns the frame decoder. `read_frame` is not
    // cancellation-safe: cancelling it mid-frame from a select! loses the
    // already-consumed bytes and desyncs the stream. Reading whole frames
    // here and handing them over a channel keeps session completions and
    // heartbeat ticks from corrupting the control stream.
    let (frame_tx, mut frame_rx) = mpsc::channel(CONTROL_FRAME_CHANNEL_CAPACITY);
    let mut reader_task = tokio::spawn(async move {
        loop {
            let message = read_frame(&mut reader).await?;
            if frame_tx.send(message).await.is_err() {
                return Err(anyhow!("agent control reader consumer dropped"));
            }
        }
    });

    // Send the first ping immediately so a dead control channel is detected
    // as early as possible instead of one interval later.
    let mut last_ping = Instant::now() - timing.ping_interval;
    let mut last_frame = Instant::now();
    let mut reader_consumed = false;
    let result: Result<()> = loop {
        tokio::select! {
            frame = frame_rx.recv() => match frame {
                Some(Message::Open { session_id }) => {
                    last_frame = Instant::now();
                    let session_config = config.clone();
                    sessions.spawn(async move {
                        if let Err(error) = run_session(session_config, session_id).await {
                            warn!(%error, "SSH session ended with an error");
                        }
                    });
                }
                Some(Message::Pong) => last_frame = Instant::now(),
                Some(other) => break Err(anyhow!("unexpected relay message: {other:?}")),
                None => break Err(anyhow!("agent control reader ended")),
            },
            result = sessions.join_next(), if !sessions.is_empty() => {
                if let Some(Err(error)) = result {
                    warn!(%error, "SSH session task failed");
                }
            }
            result = &mut reader_task, if !reader_consumed => {
                // Polling a JoinHandle to completion consumes its result. Do
                // not await the same handle again during cleanup: Tokio
                // treats that as a second poll and panics.
                reader_consumed = true;
                break match result {
                    Ok(Ok(())) => Err(anyhow!("agent control reader ended")),
                    Ok(Err(error)) => Err(error),
                    Err(error) => Err(anyhow!("agent control reader task failed: {error}")),
                };
            }
            _ = sleep_until(last_ping + timing.ping_interval), if heartbeat => {
                match timeout(timing.write_timeout, write_frame(&mut writer, &Message::Ping)).await {
                    Ok(Ok(())) => last_ping = Instant::now(),
                    Ok(Err(error)) => break Err(error.context("writing heartbeat ping")),
                    Err(_) => break Err(anyhow!(
                        "timed out writing heartbeat ping; control channel is dead"
                    )),
                }
            }
            _ = sleep_until(last_frame + timing.dead_after), if heartbeat => {
                break Err(anyhow!(
                    "no heartbeat response from relay within {:?}",
                    timing.dead_after
                ));
            }
        }
    };
    if !reader_consumed {
        reader_task.abort();
        let _ = reader_task.await;
    }
    result
}

async fn run_session(config: Config, session_id: String) -> Result<()> {
    let mut stream = noise::client_connect(&config.server, &config.server_key).await?;
    timeout(
        SESSION_ESTABLISH_TIMEOUT,
        write_frame(
            &mut stream,
            &Message::Hello {
                version: PROTOCOL_VERSION,
                role: Role::AgentSession,
                token: config.token,
                device_id: Some(config.device_id.clone()),
                features: Vec::new(),
            },
        ),
    )
    .await
    .map_err(|_| anyhow!("timed out writing session hello to relay"))??;
    match timeout(HELLO_TIMEOUT, read_frame(&mut stream))
        .await
        .map_err(|_| anyhow!("timed out waiting for relay hello response"))?
    {
        Ok(Message::HelloOk { .. }) => {}
        Ok(other) => bail!("relay did not accept hello: {other:?}"),
        Err(error) => return Err(error),
    }
    timeout(
        SESSION_ESTABLISH_TIMEOUT,
        write_frame(
            &mut stream,
            &Message::SessionAttach {
                session_id: session_id.clone(),
            },
        ),
    )
    .await
    .map_err(|_| anyhow!("timed out writing session attach"))??;

    match timeout(SESSION_ESTABLISH_TIMEOUT, read_frame(&mut stream))
        .await
        .map_err(|_| anyhow!("timed out waiting for relay session response"))?
    {
        Ok(Message::SessionAccepted { session_id: id }) if id == session_id => {}
        Ok(Message::Failed {
            session_id: id,
            reason,
        }) if id == session_id => return Err(anyhow!("relay refused SSH session: {reason}")),
        Ok(other) => return Err(anyhow!("unexpected relay session response: {other:?}")),
        Err(error) => return Err(error),
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
    timeout(
        SESSION_ESTABLISH_TIMEOUT,
        write_frame(&mut stream, &Message::Ready { session_id }),
    )
    .await
    .map_err(|_| anyhow!("timed out writing session ready"))??;
    info!(session = %session_id_for_log, "SSH session accepted");
    bridge::bidirectional(stream, local).await
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
