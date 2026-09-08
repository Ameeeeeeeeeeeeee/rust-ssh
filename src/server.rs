use crate::bootstrap;
use crate::bridge;
use crate::device_id;
use crate::identity::ServerIdentity;
use crate::noise::{self, RelayStream};
use crate::protocol::{
    read_frame, write_frame, DeviceInfo, HeartbeatTiming, Message, Role, HEARTBEAT_FEATURE,
    HELLO_TIMEOUT, PROTOCOL_VERSION, SESSION_ESTABLISH_TIMEOUT,
};
use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, Mutex, OwnedSemaphorePermit, RwLock, Semaphore};
use tokio::time::{sleep_until, timeout, Duration, Instant, MissedTickBehavior};
use tracing::{info, warn};

const MAX_CONNECTIONS: usize = 128;
const MAX_PENDING_SESSIONS_PER_DEVICE: usize = 32;
const NOISE_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const MIN_TOKEN_BYTES: usize = 32;
const MAX_TOKEN_BYTES: usize = 4096;
const CONTROL_FRAME_CHANNEL_CAPACITY: usize = 16;

/// Deadline policy for connection establishment and control-channel liveness.
/// Long-running SSH data transfer is never bound by these.
#[derive(Debug, Clone, Copy)]
pub struct ServerTiming {
    /// Absolute deadline for the first Hello frame after the Noise handshake
    /// and for each authentication response.
    pub hello_timeout: Duration,
    /// Deadline for an agent session to send SessionAttach after its Hello.
    pub attach_timeout: Duration,
    /// Deadline for an agent session to report Ready after SessionAccepted.
    pub ready_timeout: Duration,
    /// Deadline for a controller to send its first request.
    pub request_timeout: Duration,
    /// Total deadline for a new session: queuing the Open, writing it to the
    /// agent control channel, and the agent response all fit inside this.
    pub open_timeout: Duration,
    pub heartbeat: HeartbeatTiming,
}

impl Default for ServerTiming {
    fn default() -> Self {
        Self {
            hello_timeout: HELLO_TIMEOUT,
            attach_timeout: Duration::from_secs(10),
            ready_timeout: SESSION_ESTABLISH_TIMEOUT,
            request_timeout: Duration::from_secs(10),
            open_timeout: SESSION_ESTABLISH_TIMEOUT,
            heartbeat: HeartbeatTiming::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub listen: String,
    pub identity_key: PathBuf,
    pub controller_token_file: PathBuf,
    pub devices_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AuthSnapshot {
    controller_token: Arc<str>,
    device_tokens: HashMap<String, Arc<str>>,
}

#[derive(Debug, Clone)]
pub struct TokenRecord {
    pub device_id: String,
    pub path: PathBuf,
    pub token: String,
}

#[derive(Debug, Clone)]
pub struct Inventory {
    pub controller_token_path: PathBuf,
    pub controller_token: String,
    pub devices: Vec<TokenRecord>,
}

#[derive(Clone)]
struct State {
    auth: Arc<RwLock<AuthSnapshot>>,
    devices: Arc<Mutex<HashMap<String, Arc<Device>>>>,
    identity: Arc<ServerIdentity>,
    connections: Arc<Semaphore>,
    timing: ServerTiming,
}

/// An accepted agent session stream plus the connection-slot permit it
/// occupied. The permit travels with the stream so the global quota counts
/// the connection for as long as data actually flows through it.
type SessionHandoff = std::result::Result<(RelayStream, OwnedSemaphorePermit), String>;

struct Device {
    open_tx: mpsc::Sender<OpenRequest>,
    pending: Mutex<HashMap<String, oneshot::Sender<SessionHandoff>>>,
    shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
}

struct OpenRequest {
    session_id: String,
}

pub async fn run(config: Config) -> Result<()> {
    run_with_timing(config, ServerTiming::default()).await
}

/// Server with tunable deadlines for tests.
pub async fn run_with_timing(config: Config, timing: ServerTiming) -> Result<()> {
    let identity = Arc::new(ServerIdentity::load(&config.identity_key)?);
    let auth = Arc::new(RwLock::new(load_auth_snapshot(
        &config.controller_token_file,
        &config.devices_dir,
    )?));
    let listener = TcpListener::bind(&config.listen)
        .await
        .with_context(|| format!("binding relay listener {}", config.listen))?;
    let state = State {
        auth: auth.clone(),
        devices: Arc::new(Mutex::new(HashMap::new())),
        identity,
        connections: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
        timing,
    };
    spawn_auth_reload(
        auth,
        config.controller_token_file.clone(),
        config.devices_dir.clone(),
    );

    let configured_devices = state.auth.read().await.device_tokens.len();
    info!(
        listen = %config.listen,
        configured_devices,
        max_connections = MAX_CONNECTIONS,
        "rust-ssh-server is listening"
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
                    if let Err(error) = handle_connection(stream, state, permit).await {
                        warn!(%peer, %error, "relay connection ended with error");
                    }
                }
                Ok(Err(error)) => warn!(%peer, %error, "Noise handshake failed"),
                Err(_) => warn!(%peer, "Noise handshake timed out"),
            }
        });
    }
}

pub fn add_device(
    devices_dir: &Path,
    device_id: &str,
    server: &str,
    server_key_path: &Path,
) -> Result<String> {
    if !device_id::is_generated(device_id) {
        return Err(anyhow!(
            "invalid device id; use the generated ID shown by the v0.4 client"
        ));
    }
    fs::create_dir_all(devices_dir)
        .with_context(|| format!("creating device token directory {}", devices_dir.display()))?;
    let token_path = devices_dir.join(format!("{device_id}.token"));
    if token_path.exists() {
        return Err(anyhow!(
            "device is already registered: {device_id}; remove the old token first if this is intentional"
        ));
    }

    let server_key = crate::identity::load_public_key(server_key_path)?;
    let token = crate::identity::generate_token()?;
    let pairing_code = bootstrap::encode(bootstrap::Config {
        server: server.to_owned(),
        server_key,
        token: token.clone(),
        device_id: Some(device_id.to_owned()),
    })?;
    crate::identity::write_token(&token_path, &token)
        .with_context(|| format!("writing device token {}", token_path.display()))?;
    Ok(pairing_code)
}

fn load_auth_snapshot(controller_token_file: &Path, devices_dir: &Path) -> Result<AuthSnapshot> {
    let controller_token = fs::read_to_string(controller_token_file).with_context(|| {
        format!(
            "reading controller token file {}",
            controller_token_file.display()
        )
    })?;
    let controller_token = Arc::<str>::from(validate_token(&controller_token, "controller token")?);
    let device_tokens = load_device_tokens(devices_dir)?;
    Ok(AuthSnapshot {
        controller_token,
        device_tokens,
    })
}

fn spawn_auth_reload(
    auth: Arc<RwLock<AuthSnapshot>>,
    controller_token_file: PathBuf,
    devices_dir: PathBuf,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            match load_auth_snapshot(&controller_token_file, &devices_dir) {
                Ok(next) => {
                    let mut current = auth.write().await;
                    if *current != next {
                        let device_count = next.device_tokens.len();
                        *current = next;
                        info!(
                            configured_devices = device_count,
                            "reloaded rust-ssh-server token configuration"
                        );
                    }
                }
                Err(error) => {
                    warn!(%error, "could not reload token configuration; keeping the last valid snapshot");
                }
            }
        }
    });
}

async fn handle_connection(
    mut stream: RelayStream,
    state: State,
    permit: OwnedSemaphorePermit,
) -> Result<()> {
    // Absolute deadline for the first Hello: slow drips cannot keep extending
    // it, and connections that never authenticate release their slot.
    let hello = match timeout(state.timing.hello_timeout, read_frame(&mut stream)).await {
        Ok(Ok(hello)) => hello,
        Ok(Err(error)) => return Err(error),
        Err(_) => return Err(anyhow!("timed out waiting for hello")),
    };
    let (version, role, device_id, token, features) = match hello {
        Message::Hello {
            version,
            role,
            device_id,
            token,
            features,
        } => (version, role, device_id, token, features),
        _ => return Err(anyhow!("first control message must be hello")),
    };

    if version != PROTOCOL_VERSION {
        return Err(anyhow!(
            "unsupported protocol version {version}; expected {PROTOCOL_VERSION}"
        ));
    }
    {
        let auth = state.auth.read().await;
        authenticate(
            &role,
            device_id.as_deref(),
            &token,
            &auth.controller_token,
            &auth.device_tokens,
        )?;
    }
    // Heartbeat runs only when the peer advertised support; old endpoints
    // keep working without it.
    let heartbeat = features.iter().any(|feature| feature == HEARTBEAT_FEATURE);
    write_frame(
        &mut stream,
        &Message::HelloOk {
            features: if heartbeat {
                vec![HEARTBEAT_FEATURE.to_owned()]
            } else {
                Vec::new()
            },
        },
    )
    .await?;

    match role {
        Role::Agent => {
            let device_id = device_id.ok_or_else(|| anyhow!("agent did not provide device id"))?;
            handle_agent(stream, state, device_id, heartbeat, permit).await
        }
        Role::AgentSession => {
            let device_id =
                device_id.ok_or_else(|| anyhow!("agent session did not provide device id"))?;
            handle_agent_session(stream, state, device_id, permit).await
        }
        Role::Controller => handle_controller(stream, state).await,
    }
}

fn authenticate(
    role: &Role,
    device_id: Option<&str>,
    token: &str,
    controller_token: &str,
    device_tokens: &HashMap<String, Arc<str>>,
) -> Result<()> {
    match role {
        Role::Agent | Role::AgentSession => {
            let device_id = device_id.ok_or_else(|| anyhow!("agent did not provide device id"))?;
            if !device_id::is_valid(device_id) {
                return Err(anyhow!("invalid device id"));
            }
            let expected_token = device_tokens
                .get(device_id)
                .ok_or_else(|| anyhow!("device is not configured: {device_id}"))?;
            if !valid_token_length(token)
                || !constant_time_equal(token.as_bytes(), expected_token.as_bytes())
            {
                return Err(anyhow!("invalid device token for {device_id}"));
            }
        }
        Role::Controller => {
            if !valid_token_length(token)
                || !constant_time_equal(token.as_bytes(), controller_token.as_bytes())
            {
                return Err(anyhow!("invalid controller token"));
            }
        }
    }
    Ok(())
}

fn load_device_tokens(directory: &Path) -> Result<HashMap<String, Arc<str>>> {
    Ok(load_device_token_records(directory)?
        .into_iter()
        .map(|record| (record.device_id, Arc::<str>::from(record.token)))
        .collect())
}

fn load_device_token_records(directory: &Path) -> Result<Vec<TokenRecord>> {
    let entries = fs::read_dir(directory)
        .with_context(|| format!("reading device token directory {}", directory.display()))?;
    let mut records = Vec::new();
    for entry in entries {
        let entry = entry
            .with_context(|| format!("reading device token directory {}", directory.display()))?;
        if !entry
            .file_type()
            .with_context(|| format!("reading device token entry {}", entry.path().display()))?
            .is_file()
        {
            continue;
        }
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(device_id) = file_name.strip_suffix(".token") else {
            continue;
        };
        if !device_id::is_valid(device_id) {
            return Err(anyhow!(
                "device token filename must be <device_id>.token with a valid device id: {}",
                path.display()
            ));
        }
        let token = fs::read_to_string(&path)
            .with_context(|| format!("reading device token {}", path.display()))?;
        let token = validate_token(&token, &format!("device token for {device_id}"))?;
        if records
            .iter()
            .any(|record: &TokenRecord| record.device_id == device_id)
        {
            return Err(anyhow!("duplicate device token for {device_id}"));
        }
        records.push(TokenRecord {
            device_id: device_id.to_owned(),
            path,
            token,
        });
    }
    records.sort_by(|left, right| left.device_id.cmp(&right.device_id));
    Ok(records)
}

pub fn inventory(controller_token_file: &Path, devices_dir: &Path) -> Result<Inventory> {
    let controller_token = fs::read_to_string(controller_token_file).with_context(|| {
        format!(
            "reading controller token file {}",
            controller_token_file.display()
        )
    })?;
    let controller_token = validate_token(&controller_token, "controller token")?;
    Ok(Inventory {
        controller_token_path: controller_token_file.to_owned(),
        controller_token,
        devices: load_device_token_records(devices_dir)?,
    })
}

fn validate_token(token: &str, label: &str) -> Result<String> {
    let token = token.trim();
    if token.len() < MIN_TOKEN_BYTES {
        return Err(anyhow!(
            "{label} must contain at least {MIN_TOKEN_BYTES} non-whitespace bytes"
        ));
    }
    if token.len() > MAX_TOKEN_BYTES {
        return Err(anyhow!("{label} is too large"));
    }
    Ok(token.to_owned())
}

fn valid_token_length(token: &str) -> bool {
    (MIN_TOKEN_BYTES..=MAX_TOKEN_BYTES).contains(&token.len())
}

async fn handle_agent(
    stream: RelayStream,
    state: State,
    device_id: String,
    heartbeat: bool,
    _permit: OwnedSemaphorePermit,
) -> Result<()> {
    if !device_id::is_valid(&device_id) {
        return Err(anyhow!("invalid device id"));
    }

    let (open_tx, mut open_rx) = mpsc::channel(MAX_PENDING_SESSIONS_PER_DEVICE);
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let device = Arc::new(Device {
        open_tx,
        pending: Mutex::new(HashMap::new()),
        shutdown_tx: Mutex::new(Some(shutdown_tx)),
    });
    let previous = {
        let mut devices = state.devices.lock().await;
        devices.insert(device_id.clone(), device.clone())
    };
    if let Some(previous) = previous {
        stop_device(&previous).await;
        info!(device = %device_id, "replaced previous agent connection");
    }
    info!(device = %device_id, "agent registered");

    let (mut reader, mut writer) = tokio::io::split(stream);
    let timing = state.timing;

    // A dedicated reader task decodes whole frames and delivers them over a
    // channel. `read_frame` is not cancellation-safe and must never be
    // cancelled mid-frame from a select!.
    let (frame_tx, mut frame_rx) = mpsc::channel(CONTROL_FRAME_CHANNEL_CAPACITY);
    let mut reader_task = tokio::spawn(async move {
        loop {
            let message = read_frame(&mut reader).await?;
            if frame_tx.send(message).await.is_err() {
                return Err(anyhow!("agent control reader consumer dropped"));
            }
        }
    });

    // All control writes go through one writer task so Open and Pong frames
    // never interleave, and every write is bounded: a blackholed connection
    // cannot wedge the handler forever.
    let (writer_tx, mut writer_rx) = mpsc::channel(CONTROL_FRAME_CHANNEL_CAPACITY);
    let mut writer_task = tokio::spawn(async move {
        while let Some(message) = writer_rx.recv().await {
            timeout(
                timing.heartbeat.write_timeout,
                write_frame(&mut writer, &message),
            )
            .await
            .map_err(|_| anyhow!("timed out writing control frame to agent"))??;
        }
        Ok(())
    });

    let mut last_agent_frame = Instant::now();
    let mut reader_consumed = false;
    let mut writer_consumed = false;
    let result: Result<()> = loop {
        tokio::select! {
            _ = &mut shutdown_rx => {
                break Err(anyhow!("agent connection replaced by a newer client"));
            }
            request = open_rx.recv() => {
                let Some(request) = request else {
                    break Ok(());
                };
                if writer_tx
                    .send(Message::Open {
                        session_id: request.session_id,
                    })
                    .await
                    .is_err()
                {
                    break Err(anyhow!("agent control writer ended"));
                }
            }
            frame = frame_rx.recv() => match frame {
                Some(Message::Ping) => {
                    last_agent_frame = Instant::now();
                    if writer_tx.send(Message::Pong).await.is_err() {
                        break Err(anyhow!("agent control writer ended"));
                    }
                }
                Some(other) => break Err(anyhow!("unexpected agent control message: {other:?}")),
                None => break Err(anyhow!("agent control reader ended")),
            },
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
            result = &mut writer_task, if !writer_consumed => {
                writer_consumed = true;
                break match result {
                    Ok(Ok(())) => Err(anyhow!("agent control writer ended")),
                    Ok(Err(error)) => Err(error),
                    Err(error) => Err(anyhow!("agent control writer task failed: {error}")),
                };
            }
            _ = sleep_until(last_agent_frame + timing.heartbeat.dead_after), if heartbeat => {
                break Err(anyhow!(
                    "agent heartbeat timed out after {:?}",
                    timing.heartbeat.dead_after
                ));
            }
        }
    };

    if !reader_consumed {
        reader_task.abort();
        let _ = reader_task.await;
    }
    if !writer_consumed {
        writer_task.abort();
        let _ = writer_task.await;
    }
    fail_pending(&device, "device control connection ended".to_owned()).await;
    unregister(&state, &device_id, &device).await;
    match result {
        Ok(()) => {
            info!(device = %device_id, "agent disconnected");
            Ok(())
        }
        Err(error) => {
            warn!(device = %device_id, %error, "agent disconnected");
            Err(error)
        }
    }
}

async fn handle_agent_session(
    mut stream: RelayStream,
    state: State,
    device_id: String,
    permit: OwnedSemaphorePermit,
) -> Result<()> {
    let session_id = match timeout(state.timing.attach_timeout, read_frame(&mut stream)).await {
        Ok(Ok(Message::SessionAttach { session_id })) => session_id,
        Ok(Ok(other)) => {
            return Err(anyhow!(
                "agent session did not send an attach request: {other:?}"
            ))
        }
        Ok(Err(error)) => return Err(error),
        Err(_) => return Err(anyhow!("timed out waiting for session attach")),
    };
    let device = {
        let devices = state.devices.lock().await;
        devices.get(&device_id).cloned()
    };
    let Some(device) = device else {
        write_frame(
            &mut stream,
            &Message::Failed {
                session_id,
                reason: format!("device is offline: {device_id}"),
            },
        )
        .await?;
        return Ok(());
    };

    let Some(response_tx) = take_pending(&device, &session_id).await else {
        write_frame(
            &mut stream,
            &Message::Failed {
                session_id,
                reason: "SSH session is no longer waiting".to_owned(),
            },
        )
        .await?;
        return Ok(());
    };

    if let Err(error) = write_frame(
        &mut stream,
        &Message::SessionAccepted {
            session_id: session_id.clone(),
        },
    )
    .await
    {
        let _ = response_tx.send(Err(format!("could not accept agent session: {error}")));
        return Err(error);
    }

    // The Ready deadline ends the establishment phase; once the stream is
    // handed to the controller it becomes ordinary data forwarding without
    // any fixed time limit.
    match timeout(state.timing.ready_timeout, read_frame(&mut stream)).await {
        Ok(Ok(Message::Ready { session_id: id })) if id == session_id => {
            let _ = response_tx.send(Ok((stream, permit)));
            Ok(())
        }
        Ok(Ok(Message::Failed {
            session_id: id,
            reason,
        })) if id == session_id => {
            let _ = response_tx.send(Err(reason));
            Ok(())
        }
        Ok(Ok(other)) => {
            let reason = format!("unexpected agent session response: {other:?}");
            let _ = response_tx.send(Err(reason.clone()));
            Err(anyhow!(reason))
        }
        Ok(Err(error)) => {
            let reason = format!("agent session ended before ready: {error}");
            let _ = response_tx.send(Err(reason));
            Err(error)
        }
        Err(_) => {
            let reason = "agent session did not become ready in time".to_owned();
            let _ = response_tx.send(Err(reason.clone()));
            Err(anyhow!(reason))
        }
    }
}

async fn handle_controller(mut stream: RelayStream, state: State) -> Result<()> {
    let request = match timeout(state.timing.request_timeout, read_frame(&mut stream)).await {
        Ok(Ok(request)) => request,
        Ok(Err(error)) => return Err(error),
        Err(_) => return Err(anyhow!("timed out waiting for controller request")),
    };
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

    let (response_tx, response_rx) = oneshot::channel();
    if !register_pending(&device, &session_id, response_tx).await {
        write_frame(
            &mut stream,
            &Message::Failed {
                session_id,
                reason: format!(
                    "device has too many pending SSH sessions (limit: {MAX_PENDING_SESSIONS_PER_DEVICE})"
                ),
            },
        )
        .await?;
        return Ok(());
    }

    // One absolute deadline covers queuing the Open, writing it to the agent
    // control channel, and the agent response. Only the establishment phase
    // is bounded; the data stream that follows is not.
    let open_result = timeout(state.timing.open_timeout, async {
        device
            .open_tx
            .send(OpenRequest {
                session_id: session_id.clone(),
            })
            .await
            .map_err(|_| "device agent disconnected before session started".to_owned())?;
        response_rx
            .await
            .map_err(|_| "agent closed before accepting session".to_owned())
    })
    .await;

    let agent_stream = match open_result {
        Ok(Ok(Ok((agent_stream, agent_permit)))) => (agent_stream, agent_permit),
        Ok(Ok(Err(reason))) => {
            write_frame(&mut stream, &Message::Failed { session_id, reason }).await?;
            return Ok(());
        }
        Ok(Err(reason)) => {
            if let Some(response_tx) = take_pending(&device, &session_id).await {
                let _ = response_tx.send(Err(reason.clone()));
            }
            write_frame(&mut stream, &Message::Failed { session_id, reason }).await?;
            return Ok(());
        }
        Err(_) => {
            let _ = take_pending(&device, &session_id).await;
            write_frame(
                &mut stream,
                &Message::Failed {
                    session_id,
                    reason: format!(
                        "agent did not respond within {} seconds",
                        state.timing.open_timeout.as_secs()
                    ),
                },
            )
            .await?;
            return Ok(());
        }
    };
    let (agent_stream, _agent_permit) = agent_stream;

    write_frame(&mut stream, &Message::Ready { session_id }).await?;
    bridge::bidirectional(stream, agent_stream).await
}

async fn register_pending(
    device: &Arc<Device>,
    session_id: &str,
    response_tx: oneshot::Sender<SessionHandoff>,
) -> bool {
    let mut pending = device.pending.lock().await;
    if pending.len() >= MAX_PENDING_SESSIONS_PER_DEVICE || pending.contains_key(session_id) {
        return false;
    }
    pending.insert(session_id.to_owned(), response_tx);
    true
}

async fn take_pending(
    device: &Arc<Device>,
    session_id: &str,
) -> Option<oneshot::Sender<SessionHandoff>> {
    device.pending.lock().await.remove(session_id)
}

async fn fail_pending(device: &Arc<Device>, reason: String) {
    let pending = {
        let mut pending = device.pending.lock().await;
        std::mem::take(&mut *pending)
    };
    for response_tx in pending.into_values() {
        let _ = response_tx.send(Err(reason.clone()));
    }
}

async fn stop_device(device: &Arc<Device>) {
    if let Some(shutdown_tx) = device.shutdown_tx.lock().await.take() {
        let _ = shutdown_tx.send(());
    }
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
    let process_id = std::process::id();
    let entropy = crate::identity::generate_token().unwrap_or_else(|_| "no-entropy".to_owned());
    format!("{nanos:x}-{process_id:x}-{counter:x}-{entropy}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTROLLER_TOKEN: &str = "controller-token-012345678901234567890123";
    const DEVICE_A_TOKEN: &str = "device-a-token-012345678901234567890123456";
    const DEVICE_B_TOKEN: &str = "device-b-token-012345678901234567890123456";

    fn device_tokens() -> HashMap<String, Arc<str>> {
        HashMap::from([
            ("device-a".to_owned(), Arc::<str>::from(DEVICE_A_TOKEN)),
            ("device-b".to_owned(), Arc::<str>::from(DEVICE_B_TOKEN)),
        ])
    }

    #[test]
    fn agent_accepts_the_matching_device_token() {
        let result = authenticate(
            &Role::Agent,
            Some("device-a"),
            DEVICE_A_TOKEN,
            CONTROLLER_TOKEN,
            &device_tokens(),
        );

        assert!(result.is_ok());
    }

    #[test]
    fn agent_session_accepts_the_matching_device_token() {
        let result = authenticate(
            &Role::AgentSession,
            Some("device-a"),
            DEVICE_A_TOKEN,
            CONTROLLER_TOKEN,
            &device_tokens(),
        );

        assert!(result.is_ok());
    }

    #[test]
    fn agent_rejects_another_devices_token() {
        let result = authenticate(
            &Role::Agent,
            Some("device-a"),
            DEVICE_B_TOKEN,
            CONTROLLER_TOKEN,
            &device_tokens(),
        );

        assert!(result.is_err());
    }

    #[test]
    fn agent_rejects_a_missing_device() {
        let result = authenticate(
            &Role::Agent,
            Some("missing-device"),
            DEVICE_A_TOKEN,
            CONTROLLER_TOKEN,
            &device_tokens(),
        );

        assert!(result.is_err());
    }

    #[test]
    fn agent_requires_a_device_id() {
        let result = authenticate(
            &Role::Agent,
            None,
            DEVICE_A_TOKEN,
            CONTROLLER_TOKEN,
            &device_tokens(),
        );

        assert!(result.is_err());
    }

    #[test]
    fn controller_rejects_a_device_token() {
        let result = authenticate(
            &Role::Controller,
            None,
            DEVICE_A_TOKEN,
            CONTROLLER_TOKEN,
            &device_tokens(),
        );

        assert!(result.is_err());
    }

    #[test]
    fn agent_does_not_fall_back_to_the_controller_token() {
        let result = authenticate(
            &Role::Agent,
            Some("device-a"),
            CONTROLLER_TOKEN,
            CONTROLLER_TOKEN,
            &device_tokens(),
        );

        assert!(result.is_err());
    }

    #[test]
    fn controller_rejects_an_unrelated_token() {
        let result = authenticate(
            &Role::Controller,
            None,
            "wrong-controller-token-012345678901234567",
            CONTROLLER_TOKEN,
            &device_tokens(),
        );

        assert!(result.is_err());
    }

    #[test]
    fn controller_accepts_only_the_controller_token() {
        let result = authenticate(
            &Role::Controller,
            None,
            CONTROLLER_TOKEN,
            CONTROLLER_TOKEN,
            &device_tokens(),
        );

        assert!(result.is_ok());
    }

    #[test]
    fn session_ids_are_unique_across_requests() {
        let first = session_id();
        let second = session_id();

        assert_ne!(first, second);
        assert!(first.contains('-'));
        assert!(second.contains('-'));
    }

    #[tokio::test]
    async fn device_can_hold_multiple_pending_sessions() {
        let (open_tx, _open_rx) = mpsc::channel(MAX_PENDING_SESSIONS_PER_DEVICE);
        let (shutdown_tx, _shutdown_rx) = oneshot::channel();
        let device = Arc::new(Device {
            open_tx,
            pending: Mutex::new(HashMap::new()),
            shutdown_tx: Mutex::new(Some(shutdown_tx)),
        });
        let (first_tx, _first_rx) = oneshot::channel();
        let (second_tx, _second_rx) = oneshot::channel();

        assert!(register_pending(&device, "session-a", first_tx).await);
        assert!(register_pending(&device, "session-b", second_tx).await);
        assert!(take_pending(&device, "session-a").await.is_some());
        assert!(take_pending(&device, "session-b").await.is_some());
    }

    #[tokio::test]
    async fn stopping_device_signals_the_previous_connection() {
        let (open_tx, _open_rx) = mpsc::channel(MAX_PENDING_SESSIONS_PER_DEVICE);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let device = Arc::new(Device {
            open_tx,
            pending: Mutex::new(HashMap::new()),
            shutdown_tx: Mutex::new(Some(shutdown_tx)),
        });

        stop_device(&device).await;
        assert!(shutdown_rx.await.is_ok());
        stop_device(&device).await;
    }

    #[test]
    fn add_device_persists_token_and_binds_pairing_code() {
        let root = std::env::temp_dir().join(format!("rust-ssh-device-test-{}", session_id()));
        let devices_dir = root.join("devices");
        let identity_key = root.join("identity.key");
        let identity_public = root.join("identity.pub");
        let controller_token_path = root.join("controller.token");
        let device_id = "rssh-0123456789abcdef0123456789abcdef";

        crate::identity::generate(&identity_key, &identity_public).unwrap();
        let pairing_code = add_device(
            &devices_dir,
            device_id,
            "198.51.100.10:24443",
            &identity_public,
        )
        .unwrap();

        let pairing = crate::bootstrap::decode(&pairing_code).unwrap();
        assert_eq!(pairing.device_id.as_deref(), Some(device_id));
        let token_path = devices_dir.join(format!("{device_id}.token"));
        let token = fs::read_to_string(&token_path).unwrap();
        assert_eq!(token.trim().len(), 64);
        crate::identity::write_token(&controller_token_path, CONTROLLER_TOKEN).unwrap();
        let inventory = inventory(&controller_token_path, &devices_dir).unwrap();
        assert_eq!(inventory.devices.len(), 1);
        assert_eq!(inventory.devices[0].device_id, device_id);
        assert_eq!(inventory.devices[0].token, token.trim());
        assert!(add_device(
            &devices_dir,
            device_id,
            "198.51.100.10:24443",
            &identity_public,
        )
        .is_err());

        fs::remove_dir_all(root).unwrap();
    }
}
