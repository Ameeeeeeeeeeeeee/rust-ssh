use crate::bootstrap;
use crate::bridge;
use crate::device_id;
use crate::identity::ServerIdentity;
use crate::noise::{self, RelayStream};
use crate::protocol::{read_frame, write_frame, DeviceInfo, Message, Role, PROTOCOL_VERSION};
use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, Mutex, RwLock, Semaphore};
use tokio::time::{timeout, Duration, MissedTickBehavior};
use tracing::{info, warn};

const MAX_CONNECTIONS: usize = 128;
const NOISE_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const MIN_TOKEN_BYTES: usize = 32;
const MAX_TOKEN_BYTES: usize = 4096;

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
                            "reloaded rust-ssh token configuration"
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
    write_frame(&mut stream, &Message::HelloOk).await?;

    match role {
        Role::Agent => {
            let device_id = device_id.ok_or_else(|| anyhow!("agent did not provide device id"))?;
            handle_agent(stream, state, device_id).await
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
        Role::Agent => {
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

async fn handle_agent(mut stream: RelayStream, state: State, device_id: String) -> Result<()> {
    if !device_id::is_valid(&device_id) {
        return Err(anyhow!("invalid device id"));
    }

    let (open_tx, mut open_rx) = mpsc::channel(1);
    let device = Arc::new(Device {
        open_tx,
        busy: AtomicBool::new(false),
    });
    {
        let mut devices = state.devices.lock().await;
        if devices.contains_key(&device_id) {
            return Err(anyhow!("device already has an active client: {device_id}"));
        }
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
