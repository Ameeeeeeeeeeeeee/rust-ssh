//! End-to-end control recovery tests: heartbeat liveness, session survival
//! across control reconnects, establishment deadlines, and clean shutdown.
//!
//! These tests run a real relay, a real agent, and small fake peers on
//! loopback. A gated TCP proxy between the agent and the relay lets tests
//! selectively blackhole or fragment the agent's control connection while
//! data connections keep flowing.

use rust_ssh::agent;
use rust_ssh::connect::{self, ConnectTiming};
use rust_ssh::identity;
use rust_ssh::noise::{self, RelayStream};
use rust_ssh::protocol::{
    read_frame, write_frame, HeartbeatTiming, Message, Role, PROTOCOL_VERSION,
};
use rust_ssh::server::{self, ServerTiming};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinSet;
use tokio::time::{sleep, timeout};

const TEST_DEVICE_ID: &str = "rssh-0123456789abcdef0123456789abcdef";

fn unique_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "rust-ssh-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn short_server_timing() -> ServerTiming {
    ServerTiming {
        hello_timeout: Duration::from_secs(2),
        attach_timeout: Duration::from_secs(2),
        ready_timeout: Duration::from_secs(2),
        request_timeout: Duration::from_secs(2),
        open_timeout: Duration::from_secs(5),
        heartbeat: HeartbeatTiming {
            ping_interval: Duration::from_secs(15),
            dead_after: Duration::from_secs(45),
            write_timeout: Duration::from_secs(5),
        },
    }
}

fn short_connect_timing() -> ConnectTiming {
    ConnectTiming {
        hello_timeout: Duration::from_secs(3),
        response_timeout: Duration::from_secs(3),
    }
}

struct TestRelay {
    endpoint: String,
    server_key: [u8; identity::STATIC_KEY_SIZE],
    controller_token: String,
    device_token: String,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
    dir: PathBuf,
}

impl TestRelay {
    async fn start(timing: ServerTiming) -> Self {
        let dir = unique_dir("relay");
        let devices_dir = dir.join("devices");
        let identity_key = dir.join("identity.key");
        let identity_public = dir.join("identity.pub");
        let controller_token_path = dir.join("controller.token");
        identity::generate(&identity_key, &identity_public).unwrap();
        let controller_token = "c".repeat(64);
        identity::write_token(&controller_token_path, &controller_token).unwrap();

        // Bind first so the pairing code carries the real endpoint.
        let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = probe.local_addr().unwrap().to_string();
        drop(probe);
        let pairing_code =
            server::add_device(&devices_dir, TEST_DEVICE_ID, &endpoint, &identity_public).unwrap();
        let pairing = rust_ssh::bootstrap::decode(&pairing_code).unwrap();
        let device_token = pairing.token.clone();

        let task = tokio::spawn(server::run_with_timing(
            server::Config {
                listen: endpoint.clone(),
                identity_key: identity_key.clone(),
                controller_token_file: controller_token_path,
                devices_dir,
            },
            timing,
        ));

        let key = identity::load_public_key(&identity_public).unwrap();
        for _ in 0..50 {
            match noise::client_connect(&endpoint, &key).await {
                Ok(_) => break,
                Err(_) => sleep(Duration::from_millis(40)).await,
            }
        }
        Self {
            endpoint,
            server_key: key,
            controller_token,
            device_token,
            task,
            dir,
        }
    }

    fn agent_config(&self, target: String) -> agent::Config {
        agent::Config {
            server: self.endpoint.clone(),
            server_key: self.server_key,
            token: self.device_token.clone(),
            device_id: TEST_DEVICE_ID.to_owned(),
            target,
        }
    }
}

impl Drop for TestRelay {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Accepts the agent's loopback connections and speaks a tiny fake SSH
/// protocol: one banner line, then keeps the socket open until dropped.
struct FakeSshd {
    listener: TcpListener,
}

impl FakeSshd {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        Self { listener }
    }

    fn address(&self) -> String {
        self.listener.local_addr().unwrap().to_string()
    }

    async fn accept_one(&self) -> TcpStream {
        let (mut connection, _) = self.listener.accept().await.unwrap();
        connection.write_all(b"banner\n").await.unwrap();
        connection
    }
}

/// Opens a controller connection to a relay and waits until the session with
/// `session_id` reaches Ready. On success the returned stream carries SSH
/// data; on failure the relay's Failed reason is returned.
async fn controller_open_raw(
    endpoint: String,
    server_key: [u8; identity::STATIC_KEY_SIZE],
    controller_token: String,
    device: &str,
    session_id: String,
) -> Result<RelayStream, String> {
    let mut stream = noise::client_connect(&endpoint, &server_key)
        .await
        .map_err(|error| error.to_string())?;
    write_frame(
        &mut stream,
        &Message::Hello {
            version: PROTOCOL_VERSION,
            role: Role::Controller,
            token: controller_token,
            device_id: None,
            features: Vec::new(),
        },
    )
    .await
    .map_err(|error| error.to_string())?;
    match read_frame(&mut stream)
        .await
        .map_err(|error| error.to_string())?
    {
        Message::HelloOk { .. } => {}
        other => return Err(format!("unexpected hello response: {other:?}")),
    }
    write_frame(
        &mut stream,
        &Message::OpenRequest {
            target: device.to_owned(),
            session_id: session_id.clone(),
        },
    )
    .await
    .map_err(|error| error.to_string())?;
    match read_frame(&mut stream)
        .await
        .map_err(|error| error.to_string())?
    {
        Message::Ready { session_id: id } if id == session_id => Ok(stream),
        Message::Failed {
            session_id: id,
            reason,
        } if id == session_id => Err(reason),
        other => Err(format!("unexpected session response: {other:?}")),
    }
}

async fn controller_open(
    relay: &TestRelay,
    device: &str,
    session_id: String,
) -> Result<RelayStream, String> {
    controller_open_raw(
        relay.endpoint.clone(),
        relay.server_key,
        relay.controller_token.clone(),
        device,
        session_id,
    )
    .await
}

/// Establishes a full session and returns the controller data stream plus the
/// fake-sshd side of the connection.
async fn open_session(
    relay: &TestRelay,
    sshd: &FakeSshd,
) -> Result<(RelayStream, TcpStream), String> {
    let mut controller = controller_open(relay, TEST_DEVICE_ID, server::session_id()).await?;
    let ssh_connection = sshd.accept_one().await;
    let mut banner = [0_u8; 7];
    timeout(Duration::from_secs(5), controller.read_exact(&mut banner))
        .await
        .map_err(|_| "timed out reading fake sshd banner".to_owned())?
        .map_err(|error| error.to_string())?;
    assert_eq!(&banner, b"banner\n");
    Ok((controller, ssh_connection))
}

fn spawn_agent(
    config: agent::Config,
    timing: HeartbeatTiming,
) -> (
    tokio::task::JoinHandle<anyhow::Result<()>>,
    oneshot::Sender<()>,
    std::sync::mpsc::Receiver<agent::Status>,
) {
    let (stop_tx, stop_rx) = oneshot::channel();
    let (status_tx, status_rx) = std::sync::mpsc::channel();
    let task = tokio::spawn(agent::run_until_stopped_with_timing(
        config,
        stop_rx,
        Some(status_tx),
        timing,
    ));
    (task, stop_tx, status_rx)
}

async fn wait_for_status(
    receiver: &std::sync::mpsc::Receiver<agent::Status>,
    matches: impl Fn(&agent::Status) -> bool,
    within: Duration,
) -> bool {
    let start = tokio::time::Instant::now();
    while start.elapsed() < within {
        while let Ok(status) = receiver.try_recv() {
            if matches(&status) {
                return true;
            }
        }
        sleep(Duration::from_millis(20)).await;
    }
    false
}

/// Selective blackhole/fragmentation for the first connection the agent opens
/// (its control channel). All other connections pass through untouched.
#[derive(Default)]
enum GateMode {
    #[default]
    Pass,
    /// Hold the first bytes after arming until released.
    HoldNext {
        hold_after: usize,
        release: Option<oneshot::Receiver<()>>,
        reached: Option<oneshot::Sender<()>>,
    },
    /// Read and discard instead of forwarding; lifted when the gated
    /// connection's upstream side closes.
    Discard,
}

#[derive(Clone)]
struct Gate {
    mode: Arc<Mutex<GateMode>>,
}

impl Gate {
    fn new() -> Self {
        Self {
            mode: Arc::new(Mutex::new(GateMode::Pass)),
        }
    }

    async fn arm_hold(&self, hold_after: usize) -> (oneshot::Sender<()>, oneshot::Receiver<()>) {
        let (release_tx, release_rx) = oneshot::channel();
        let (reached_tx, reached_rx) = oneshot::channel();
        *self.mode.lock().await = GateMode::HoldNext {
            hold_after,
            release: Some(release_rx),
            reached: Some(reached_tx),
        };
        (release_tx, reached_rx)
    }

    async fn set_discard(&self) {
        *self.mode.lock().await = GateMode::Discard;
    }

    async fn clear(&self) {
        *self.mode.lock().await = GateMode::Pass;
    }
}

struct GateProxy {
    endpoint: String,
    task: tokio::task::JoinHandle<()>,
}

impl GateProxy {
    async fn start(relay: &TestRelay, gate: Gate) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        let upstream = relay.endpoint.clone();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            let mut connection_id = 0usize;
            loop {
                let (connection, _) = match listener.accept().await {
                    Ok(connection) => connection,
                    Err(_) => break,
                };
                connection_id += 1;
                let first = connection_id == 1;
                // Raw TCP passthrough: the relay talks Noise directly with
                // the agent through this proxy, so the byte offsets below
                // refer to real wire bytes.
                let upstream = match TcpStream::connect(&upstream).await {
                    Ok(upstream) => upstream,
                    Err(_) => continue,
                };
                let gate = gate.clone();
                connections.spawn(async move {
                    let (mut agent_reader, mut agent_writer) = connection.into_split();
                    let (mut relay_reader, mut relay_writer) = upstream.into_split();
                    let upstream_gate = gate.clone();
                    let upstream = tokio::spawn(async move {
                        let result = tokio::io::copy(&mut agent_reader, &mut relay_writer).await;
                        // The agent closing its control channel lifts the
                        // blackhole so the reconnect passes through.
                        if first {
                            upstream_gate.clear().await;
                        }
                        result
                    });
                    let downstream = tokio::spawn(async move {
                        if first {
                            forward_gated(&mut relay_reader, &mut agent_writer, &gate).await
                        } else {
                            tokio::io::copy(&mut relay_reader, &mut agent_writer)
                                .await
                                .map(|_| ())
                        }
                    });
                    let _ = tokio::join!(upstream, downstream);
                });
            }
        });
        Self { endpoint, task }
    }
}

impl Drop for GateProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn forward_gated<R, W>(from: &mut R, to: &mut W, gate: &Gate) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buffer = [0_u8; 4096];
    let mut armed_sent = 0usize;
    loop {
        let n = from.read(&mut buffer).await?;
        if n == 0 {
            return Ok(());
        }
        let mut state = gate.mode.lock().await;
        match &mut *state {
            GateMode::Pass => {
                to.write_all(&buffer[..n]).await?;
            }
            GateMode::Discard => {}
            GateMode::HoldNext {
                hold_after,
                release,
                reached,
            } => {
                if armed_sent + n <= *hold_after {
                    to.write_all(&buffer[..n]).await?;
                    armed_sent += n;
                } else {
                    let head = hold_after.saturating_sub(armed_sent);
                    if head > 0 {
                        to.write_all(&buffer[..head]).await?;
                        armed_sent += head;
                    }
                    if let Some(tx) = reached.take() {
                        let _ = tx.send(());
                    }
                    if let Some(release) = release.take() {
                        drop(state);
                        let _ = release.await;
                        state = gate.mode.lock().await;
                    }
                    to.write_all(&buffer[head..n]).await?;
                    armed_sent += n - head;
                    *state = GateMode::Pass;
                }
            }
        }
    }
}

#[tokio::test]
async fn fragmented_open_frame_survives_interleaved_session_completion() {
    let mut timing = short_server_timing();
    timing.open_timeout = Duration::from_secs(8);
    let relay = TestRelay::start(timing).await;
    let sshd = FakeSshd::start().await;
    let gate = Gate::new();
    let proxy = GateProxy::start(&relay, gate.clone()).await;
    // Long ping interval so no Pong interferes with the armed hold below.
    let agent_timing = HeartbeatTiming {
        ping_interval: Duration::from_secs(30),
        dead_after: Duration::from_secs(90),
        write_timeout: Duration::from_secs(5),
    };
    let mut config = relay.agent_config(sshd.address());
    config.server = proxy.endpoint.clone();
    let (_task, _stop, status_rx) = spawn_agent(config, agent_timing);
    assert!(
        wait_for_status(
            &status_rx,
            |status| matches!(status, agent::Status::Connected),
            Duration::from_secs(10)
        )
        .await,
        "agent did not connect"
    );

    // Session 1 flows while the gate passes everything.
    let (controller1, ssh_connection1) = open_session(&relay, &sshd).await.unwrap();

    // Arm: hold after 24 bytes — the whole first Noise frame (ciphertext
    // length + the u32 frame length + tag). At that point the agent has read
    // the control frame length and is waiting for its payload, exactly the
    // state where a cancelled read_frame used to desync the stream.
    let (_release_tx, hold_reached) = gate.arm_hold(24).await;
    let session_id = server::session_id();
    let open2 = tokio::spawn(controller_open_raw(
        relay.endpoint.clone(),
        relay.server_key,
        relay.controller_token.clone(),
        TEST_DEVICE_ID,
        session_id,
    ));
    // The relay has written the Open frame and the proxy holds it mid-frame.
    timeout(Duration::from_secs(5), hold_reached)
        .await
        .expect("proxy never reached the hold point")
        .unwrap();

    // Complete session 1 while the Open frame for session 2 is half-read.
    // This is the interleave that used to cancel read_frame mid-frame.
    drop(ssh_connection1);
    drop(controller1);
    sleep(Duration::from_millis(400)).await;

    _release_tx.send(()).unwrap();
    let mut controller2 = timeout(Duration::from_secs(10), open2)
        .await
        .expect("session 2 open did not finish")
        .unwrap()
        .expect("session 2 was refused");

    let ssh_connection2 = sshd.accept_one().await;
    let mut banner = [0_u8; 7];
    timeout(Duration::from_secs(5), controller2.read_exact(&mut banner))
        .await
        .expect("session 2 banner did not arrive")
        .unwrap();
    assert_eq!(&banner, b"banner\n");
    drop(ssh_connection2);
    assert!(
        !wait_for_status(
            &status_rx,
            |status| matches!(status, agent::Status::Retrying(_)),
            Duration::from_secs(2)
        )
        .await,
        "agent control channel must survive the interleave"
    );
}

#[tokio::test]
async fn blackholed_control_channel_reconnects_without_tearing_down_sessions() {
    let mut timing = short_server_timing();
    timing.heartbeat = HeartbeatTiming {
        ping_interval: Duration::from_millis(200),
        dead_after: Duration::from_secs(5),
        write_timeout: Duration::from_secs(2),
    };
    let relay = TestRelay::start(timing).await;
    let sshd = FakeSshd::start().await;
    let gate = Gate::new();
    let proxy = GateProxy::start(&relay, gate.clone()).await;
    let agent_timing = HeartbeatTiming {
        ping_interval: Duration::from_millis(200),
        dead_after: Duration::from_secs(1),
        write_timeout: Duration::from_secs(2),
    };
    let mut config = relay.agent_config(sshd.address());
    config.server = proxy.endpoint.clone();
    let (_task, _stop, status_rx) = spawn_agent(config, agent_timing);
    assert!(
        wait_for_status(
            &status_rx,
            |status| matches!(status, agent::Status::Connected),
            Duration::from_secs(10)
        )
        .await,
        "agent did not connect"
    );

    let (mut controller1, mut ssh_connection1) = open_session(&relay, &sshd).await.unwrap();

    // Blackhole the control channel's relay->agent direction: pings reach the
    // relay and Pongs are discarded, so the relay stays quiet while the
    // agent must declare the channel dead.
    gate.set_discard().await;
    assert!(
        wait_for_status(
            &status_rx,
            |status| matches!(status, agent::Status::Retrying(_)),
            Duration::from_secs(5)
        )
        .await,
        "agent did not notice the dead control channel"
    );
    assert!(
        wait_for_status(
            &status_rx,
            |status| matches!(status, agent::Status::Connected),
            Duration::from_secs(10)
        )
        .await,
        "agent did not reconnect the control channel"
    );

    // The old data session keeps carrying bytes in both directions.
    ssh_connection1.write_all(b"more\n").await.unwrap();
    let mut line = [0_u8; 5];
    timeout(Duration::from_secs(5), controller1.read_exact(&mut line))
        .await
        .expect("old data session stopped carrying data")
        .unwrap();
    assert_eq!(&line, b"more\n");

    // The reconnect registered a fresh generation and new sessions work.
    // open_session already verifies the banner end to end.
    let (controller2, ssh_connection2) = open_session(&relay, &sshd).await.unwrap();
    drop(controller2);
    drop(ssh_connection2);
    drop(ssh_connection1);
}

#[tokio::test]
async fn stopping_the_client_terminates_control_and_data_tasks() {
    let relay = TestRelay::start(short_server_timing()).await;
    let sshd = FakeSshd::start().await;
    let config = relay.agent_config(sshd.address());
    let (task, stop_tx, status_rx) = spawn_agent(config, HeartbeatTiming::default());
    assert!(
        wait_for_status(
            &status_rx,
            |status| matches!(status, agent::Status::Connected),
            Duration::from_secs(10)
        )
        .await,
        "agent did not connect"
    );

    let (mut controller, mut ssh_connection) = open_session(&relay, &sshd).await.unwrap();
    ssh_connection.write_all(b"alive\n").await.unwrap();
    let mut line = [0_u8; 6];
    timeout(Duration::from_secs(5), controller.read_exact(&mut line))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&line, b"alive\n");

    stop_tx.send(()).unwrap();
    let result = timeout(Duration::from_secs(5), task)
        .await
        .expect("agent did not stop in time")
        .unwrap();
    assert!(result.is_ok(), "agent stop must succeed: {result:?}");

    // Both directions of the data session observe the teardown.
    let mut leftover = [0_u8; 16];
    timeout(Duration::from_secs(5), controller.read(&mut leftover))
        .await
        .expect("controller side did not see session end")
        .unwrap();
    timeout(Duration::from_secs(5), ssh_connection.read(&mut leftover))
        .await
        .expect("sshd side did not see session end")
        .unwrap();
}

/// The production limit; tests must stay aligned with it.
fn server_slot_limit() -> usize {
    128
}

#[tokio::test]
async fn server_reclaims_slots_from_connections_that_never_send_hello() {
    let mut timing = short_server_timing();
    timing.hello_timeout = Duration::from_secs(1);
    let relay = TestRelay::start(timing).await;

    // Fill every connection slot with a peer that completes the Noise
    // handshake but never sends Hello.
    let mut silent_peers = Vec::new();
    for _ in 0..server_slot_limit() {
        silent_peers.push(
            noise::client_connect(&relay.endpoint, &relay.server_key)
                .await
                .expect("filling slots must succeed"),
        );
    }
    // After the hello deadline every silent peer is reclaimed.
    sleep(Duration::from_millis(1500)).await;
    let devices = timeout(
        Duration::from_secs(5),
        connect::list_devices_with_timing(
            connect::ListConfig {
                server: relay.endpoint.clone(),
                server_key: relay.server_key,
                token: relay.controller_token.clone(),
            },
            short_connect_timing(),
        ),
    )
    .await
    .expect("list must finish after slots are reclaimed")
    .expect("list must succeed after slots are reclaimed");
    assert!(devices.is_empty());
    drop(silent_peers);
}

#[tokio::test]
async fn server_kills_a_slow_hello_drip_and_reclaims_the_slot() {
    let mut timing = short_server_timing();
    timing.hello_timeout = Duration::from_secs(1);
    let relay = TestRelay::start(timing).await;

    let mut drip = noise::client_connect(&relay.endpoint, &relay.server_key)
        .await
        .unwrap();
    drip.write_all(&[0_u8; 1]).await.unwrap();
    drip.flush().await.unwrap();
    // The drip cannot stretch the absolute deadline.
    sleep(Duration::from_millis(1500)).await;

    let devices = timeout(
        Duration::from_secs(5),
        connect::list_devices_with_timing(
            connect::ListConfig {
                server: relay.endpoint.clone(),
                server_key: relay.server_key,
                token: relay.controller_token.clone(),
            },
            short_connect_timing(),
        ),
    )
    .await
    .expect("list must finish after the drip is killed")
    .expect("list must succeed after the drip is killed");
    assert!(devices.is_empty());
}

#[tokio::test]
async fn server_enforces_attach_and_ready_deadlines() {
    let mut timing = short_server_timing();
    timing.attach_timeout = Duration::from_secs(1);
    timing.ready_timeout = Duration::from_secs(1);
    let relay = TestRelay::start(timing).await;

    // An agent session that authenticates but never attaches is reclaimed.
    let mut stalled_attach = noise::client_connect(&relay.endpoint, &relay.server_key)
        .await
        .unwrap();
    write_frame(
        &mut stalled_attach,
        &Message::Hello {
            version: PROTOCOL_VERSION,
            role: Role::AgentSession,
            token: relay.device_token.clone(),
            device_id: Some(TEST_DEVICE_ID.to_owned()),
            features: Vec::new(),
        },
    )
    .await
    .unwrap();
    match read_frame(&mut stalled_attach).await.unwrap() {
        Message::HelloOk { .. } => {}
        other => panic!("unexpected hello response: {other:?}"),
    }
    let mut buffer = [0_u8; 4];
    timeout(Duration::from_secs(3), stalled_attach.read(&mut buffer))
        .await
        .expect("stalled attach connection must be closed")
        .unwrap();

    // A session that attaches but never reaches Ready fails the pending
    // request within the deadline.
    let mut control = noise::client_connect(&relay.endpoint, &relay.server_key)
        .await
        .unwrap();
    write_frame(
        &mut control,
        &Message::Hello {
            version: PROTOCOL_VERSION,
            role: Role::Agent,
            token: relay.device_token.clone(),
            device_id: Some(TEST_DEVICE_ID.to_owned()),
            features: Vec::new(),
        },
    )
    .await
    .unwrap();
    match read_frame(&mut control).await.unwrap() {
        Message::HelloOk { .. } => {}
        other => panic!("unexpected hello response: {other:?}"),
    }

    let session_id = server::session_id();
    let open = tokio::spawn(controller_open_raw(
        relay.endpoint.clone(),
        relay.server_key,
        relay.controller_token.clone(),
        TEST_DEVICE_ID,
        session_id.clone(),
    ));
    sleep(Duration::from_millis(200)).await;

    let mut session = noise::client_connect(&relay.endpoint, &relay.server_key)
        .await
        .unwrap();
    write_frame(
        &mut session,
        &Message::Hello {
            version: PROTOCOL_VERSION,
            role: Role::AgentSession,
            token: relay.device_token.clone(),
            device_id: Some(TEST_DEVICE_ID.to_owned()),
            features: Vec::new(),
        },
    )
    .await
    .unwrap();
    match read_frame(&mut session).await.unwrap() {
        Message::HelloOk { .. } => {}
        other => panic!("unexpected hello response: {other:?}"),
    }
    write_frame(
        &mut session,
        &Message::SessionAttach {
            session_id: session_id.clone(),
        },
    )
    .await
    .unwrap();
    match read_frame(&mut session).await.unwrap() {
        Message::SessionAccepted { session_id: id } if id == session_id => {}
        other => panic!("unexpected attach response: {other:?}"),
    }
    // Never send Ready.

    let refused = timeout(Duration::from_secs(5), open)
        .await
        .expect("pending request must fail within the deadline")
        .unwrap();
    let refused = match refused {
        Ok(_) => panic!("session must be refused"),
        Err(reason) => reason,
    };
    assert!(
        refused.contains("ready"),
        "refusal must come from the ready deadline: {refused}"
    );
    let mut buffer = [0_u8; 4];
    timeout(Duration::from_secs(3), session.read(&mut buffer))
        .await
        .expect("stalled session connection must be closed")
        .unwrap();
    drop(control);
}

#[tokio::test]
async fn server_heartbeat_declares_a_silent_agent_dead() {
    let mut timing = short_server_timing();
    timing.heartbeat = HeartbeatTiming {
        ping_interval: Duration::from_secs(15),
        dead_after: Duration::from_secs(1),
        write_timeout: Duration::from_secs(2),
    };
    let relay = TestRelay::start(timing).await;

    // A heartbeat-capable agent registers and then goes silent.
    let mut silent_agent = noise::client_connect(&relay.endpoint, &relay.server_key)
        .await
        .unwrap();
    write_frame(
        &mut silent_agent,
        &Message::Hello {
            version: PROTOCOL_VERSION,
            role: Role::Agent,
            token: relay.device_token.clone(),
            device_id: Some(TEST_DEVICE_ID.to_owned()),
            features: vec![rust_ssh::protocol::HEARTBEAT_FEATURE.to_owned()],
        },
    )
    .await
    .unwrap();
    match read_frame(&mut silent_agent).await.unwrap() {
        Message::HelloOk { features } => assert!(features
            .iter()
            .any(|feature| feature == rust_ssh::protocol::HEARTBEAT_FEATURE)),
        other => panic!("unexpected hello response: {other:?}"),
    }

    // A pending session fails as soon as the heartbeat marks the agent dead.
    let refused = match controller_open(&relay, TEST_DEVICE_ID, server::session_id()).await {
        Ok(_) => panic!("pending session must fail once the agent is dead"),
        Err(reason) => reason,
    };
    assert!(
        refused.contains("control connection ended"),
        "unexpected refusal reason: {refused}"
    );

    // The dead agent was unregistered.
    let devices = timeout(
        Duration::from_secs(5),
        connect::list_devices_with_timing(
            connect::ListConfig {
                server: relay.endpoint.clone(),
                server_key: relay.server_key,
                token: relay.controller_token.clone(),
            },
            short_connect_timing(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(devices.is_empty(), "dead agent must be unregistered");
    drop(silent_agent);
}

/// A relay that completes the Noise handshake and then stalls either before
/// or after the HelloOk response.
#[derive(Clone, Copy)]
enum StallPoint {
    AfterHandshake,
    AfterHelloOk,
}

struct StallingRelay {
    endpoint: String,
    server_key: [u8; identity::STATIC_KEY_SIZE],
    task: tokio::task::JoinHandle<()>,
}

impl StallingRelay {
    async fn start(stall: StallPoint) -> Self {
        let dir = unique_dir("stall");
        let identity_key = dir.join("identity.key");
        let identity_public = dir.join("identity.pub");
        identity::generate(&identity_key, &identity_public).unwrap();
        let identity = identity::ServerIdentity::load(&identity_key).unwrap();
        let server_key = identity::load_public_key(&identity_public).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        let task = tokio::spawn(async move {
            loop {
                let (connection, _) = match listener.accept().await {
                    Ok(connection) => connection,
                    Err(_) => break,
                };
                let identity = identity.clone();
                tokio::spawn(async move {
                    let mut stream = match noise::server_handshake(connection, &identity).await {
                        Ok(stream) => stream,
                        Err(_) => return,
                    };
                    match stall {
                        StallPoint::AfterHandshake => {
                            // Never answer the Hello.
                            sleep(Duration::from_secs(3600)).await;
                        }
                        StallPoint::AfterHelloOk => {
                            let Ok(message) = read_frame(&mut stream).await else {
                                return;
                            };
                            if !matches!(message, Message::Hello { .. }) {
                                return;
                            }
                            if write_frame(
                                &mut stream,
                                &Message::HelloOk {
                                    features: Vec::new(),
                                },
                            )
                            .await
                            .is_err()
                            {
                                return;
                            }
                            // Never answer the follow-up request.
                            sleep(Duration::from_secs(3600)).await;
                        }
                    }
                });
            }
        });
        Self {
            endpoint,
            server_key,
            task,
        }
    }
}

impl Drop for StallingRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn connect_list_times_out_when_the_relay_never_answers() {
    let fast = ConnectTiming {
        hello_timeout: Duration::from_millis(500),
        response_timeout: Duration::from_millis(500),
    };

    let stalled_hello = StallingRelay::start(StallPoint::AfterHandshake).await;
    let error = timeout(
        Duration::from_secs(5),
        connect::list_devices_with_timing(
            connect::ListConfig {
                server: stalled_hello.endpoint.clone(),
                server_key: stalled_hello.server_key,
                token: "t".repeat(64),
            },
            fast,
        ),
    )
    .await
    .expect("list must not hang when HelloOk never arrives")
    .expect_err("list must fail when HelloOk never arrives");
    assert!(error.to_string().contains("timed out"), "error: {error}");

    let stalled_list = StallingRelay::start(StallPoint::AfterHelloOk).await;
    let error = timeout(
        Duration::from_secs(5),
        connect::list_devices_with_timing(
            connect::ListConfig {
                server: stalled_list.endpoint.clone(),
                server_key: stalled_list.server_key,
                token: "t".repeat(64),
            },
            fast,
        ),
    )
    .await
    .expect("list must not hang when the device list never arrives")
    .expect_err("list must fail when the device list never arrives");
    assert!(error.to_string().contains("timed out"), "error: {error}");
}

#[tokio::test]
async fn connect_session_times_out_when_the_relay_never_answers() {
    let stalled = StallingRelay::start(StallPoint::AfterHelloOk).await;
    let fast = ConnectTiming {
        hello_timeout: Duration::from_millis(500),
        response_timeout: Duration::from_millis(500),
    };
    // run_with_timing only reaches stdin/stdout after Ready; the deadline
    // chain before that is fully testable.
    let error = timeout(
        Duration::from_secs(5),
        connect::run_with_timing(
            connect::Config {
                server: stalled.endpoint.clone(),
                server_key: stalled.server_key,
                token: "t".repeat(64),
                target: TEST_DEVICE_ID.to_owned(),
            },
            fast,
        ),
    )
    .await
    .expect("session establishment must not hang")
    .expect_err("session establishment must fail when Ready never arrives");
    assert!(error.to_string().contains("timed out"), "error: {error}");
}
