use crate::bridge;
use crate::identity::ServerIdentity;
use crate::noise::{self, RelayStream};
use crate::protocol::{read_frame, write_frame, DeviceInfo, Message, Role, PROTOCOL_VERSION};
use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, Mutex, Semaphore};
use tokio::time::{timeout, Duration};
use tracing::{info, warn};

const MAX_CONNECTIONS: usize = 128;
const NOISE_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TOKEN_BYTES: usize = 4096;

#[derive(Debug, Clone)]
pub struct Config {
    pub listen: String,
    pub identity_key: PathBuf,
    pub token: String,
}

#[derive(Clone)]
struct State {
    token: Arc<str>,
    devices: Arc<Mutex<HashMap<String, Arc<Device>>>>,
    identity: Arc<ServerIdentity>,
    connections: Arc<Semaphore>,
}

struct Device {
    open_tx: mpsc::Sender<OpenRequest>,
    busy: AtomicBool,
}

struct OpenRequest {
    session_id: String,
    response_tx: oneshot::Sender<std::result::Result<RelayStream, String>>,
}

pub async fn run(config: Config) -> Result<()> {
    let identity = Arc::new(ServerIdentity::load(&config.identity_key)?);
    let listener = TcpListener::bind(&config.listen)
        .await
        .with_context(|| format!("binding relay listener {}", config.listen))?;
    let state = State {
        token: Arc::from(config.token),
        devices: Arc::new(Mutex::new(HashMap::new())),
        identity,
        connections: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
    };

    info!(
        listen = %config.listen,
        max_connections = MAX_CONNECTIONS,
        "rust-ssh relay is listening"
    );
    loop {
        let (tcp, peer) = listener.accept().await.context("accepting TCP client")?;
        let permit = match state.connections.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                warn!(%peer, "relay connection limit reached; dropping connection");
                continue;
            }
        };
        let identity = state.identity.clone();
        let state = state.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(error) = tcp.set_nodelay(true) {
                warn!(%peer, %error, "could not enable TCP_NODELAY");
            }
            match timeout(
                NOISE_HANDSHAKE_TIMEOUT,
                noise::server_handshake(tcp, &identity),
            )
            .await
            {
                Ok(Ok(stream)) => {
                    if let Err(error) = handle_connection(stream, state).await {
                        warn!(%peer, %error, "relay connection ended with error");
                    }
                }
                Ok(Err(error)) => warn!(%peer, %error, "Noise handshake failed"),
                Err(_) => warn!(%peer, "Noise handshake timed out"),
            }
        });
    }
}

async fn handle_connection(mut stream: RelayStream, state: State) -> Result<()> {
    let hello = read_frame(&mut stream).await?;
    let (version, role, device_id, token) = match hello {
        Message::Hello {
            version,
            role,
            device_id,
            token,
        } => (version, role, device_id, token),
        _ => return Err(anyhow!("first control message must be hello")),
    };

    if version != PROTOCOL_VERSION {
        return Err(anyhow!(
            "unsupported protocol version {version}; expected {PROTOCOL_VERSION}"
        ));
    }
    if token.len() > MAX_TOKEN_BYTES
        || !constant_time_equal(token.as_bytes(), state.token.as_bytes())
    {
        return Err(anyhow!("invalid relay token"));
    }
    write_frame(&mut stream, &Message::HelloOk).await?;

    match role {
        Role::Agent => {
            let device_id = device_id.ok_or_else(|| anyhow!("agent did not provide device id"))?;
            handle_agent(stream, state, device_id).await
        }
        Role::Controller => handle_controller(stream, state).await,
    }
}

async fn handle_agent(mut stream: RelayStream, state: State, device_id: String) -> Result<()> {
    if !valid_device_id(&device_id) {
        return Err(anyhow!("invalid device id"));
    }

    let (open_tx, mut open_rx) = mpsc::channel(1);
    let device = Arc::new(Device {
        open_tx,
        busy: AtomicBool::new(false),
    });
    {
        let mut devices = state.devices.lock().await;
        devices.insert(device_id.clone(), device.clone());
    }
    info!(device = %device_id, "agent registered");

    while let Some(request) = open_rx.recv().await {
        write_frame(
            &mut stream,
            &Message::Open {
                session_id: request.session_id.clone(),
            },
        )
        .await?;

        let response = read_frame(&mut stream).await?;
        match response {
            Message::Ready { session_id } if session_id == request.session_id => {
                let _ = request.response_tx.send(Ok(stream));
                unregister(&state, &device_id, &device).await;
                return Ok(());
            }
            Message::Failed { session_id, reason } if session_id == request.session_id => {
                device.busy.store(false, Ordering::Release);
                let _ = request.response_tx.send(Err(reason));
            }
            other => {
                device.busy.store(false, Ordering::Release);
                let reason = format!("unexpected agent response: {other:?}");
                let _ = request.response_tx.send(Err(reason));
            }
        }
    }

    unregister(&state, &device_id, &device).await;
    info!(device = %device_id, "agent disconnected");
    Ok(())
}

fn valid_device_id(device_id: &str) -> bool {
    !device_id.is_empty()
        && device_id.len() <= 128
        && device_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

async fn handle_controller(mut stream: RelayStream, state: State) -> Result<()> {
    let request = read_frame(&mut stream).await?;
    let request = match request {
        Message::ListRequest => {
            let mut devices: Vec<DeviceInfo> = {
                let devices = state.devices.lock().await;
                devices
                    .keys()
                    .cloned()
                    .map(|device_id| DeviceInfo { device_id })
                    .collect()
            };
            devices.sort_by(|left, right| left.device_id.cmp(&right.device_id));
            write_frame(&mut stream, &Message::DeviceList { devices }).await?;
            return Ok(());
        }
        Message::OpenRequest { target, session_id } => (target, session_id),
        _ => return Err(anyhow!("controller did not send a valid request")),
    };
    let (target, session_id) = request;
    let device = {
        let devices = state.devices.lock().await;
        devices.get(&target).cloned()
    };
    let Some(device) = device else {
        write_frame(
            &mut stream,
            &Message::Failed {
                session_id,
                reason: format!("device is offline: {target}"),
            },
        )
        .await?;
        return Ok(());
    };

    if device.busy.swap(true, Ordering::AcqRel) {
        write_frame(
            &mut stream,
            &Message::Failed {
                session_id,
                reason: "device already has an active SSH session".to_owned(),
            },
        )
        .await?;
        return Ok(());
    }

    let (response_tx, response_rx) = oneshot::channel();
    if device
        .open_tx
        .send(OpenRequest {
            session_id: session_id.clone(),
            response_tx,
        })
        .await
        .is_err()
    {
        device.busy.store(false, Ordering::Release);
        write_frame(
            &mut stream,
            &Message::Failed {
                session_id,
                reason: "device agent disconnected before session started".to_owned(),
            },
        )
        .await?;
        return Ok(());
    }

    let agent_stream = match timeout(Duration::from_secs(15), response_rx).await {
        Ok(Ok(Ok(agent_stream))) => agent_stream,
        Ok(Ok(Err(reason))) => {
            device.busy.store(false, Ordering::Release);
            write_frame(&mut stream, &Message::Failed { session_id, reason }).await?;
            return Ok(());
        }
        Ok(Err(_)) => {
            device.busy.store(false, Ordering::Release);
            write_frame(
                &mut stream,
                &Message::Failed {
                    session_id,
                    reason: "agent closed before accepting session".to_owned(),
                },
            )
            .await?;
            return Ok(());
        }
        Err(_) => {
            device.busy.store(false, Ordering::Release);
            write_frame(
                &mut stream,
                &Message::Failed {
                    session_id,
                    reason: "agent did not respond within 15 seconds".to_owned(),
                },
            )
            .await?;
            return Ok(());
        }
    };

    write_frame(&mut stream, &Message::Ready { session_id }).await?;
    bridge::bidirectional(stream, agent_stream).await
}

async fn unregister(state: &State, device_id: &str, device: &Arc<Device>) {
    let mut devices = state.devices.lock().await;
    if let Some(current) = devices.get(device_id) {
        if Arc::ptr_eq(current, device) {
            devices.remove(device_id);
        }
    }
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        let a = left.get(index).copied().unwrap_or(0);
        let b = right.get(index).copied().unwrap_or(0);
        difference |= usize::from(a ^ b);
    }
    difference == 0
}

pub fn session_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("{nanos:x}-{counter:x}")
}
