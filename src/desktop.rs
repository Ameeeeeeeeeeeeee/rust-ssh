use crate::agent;
use crate::connect::{self, ListConfig};
use anyhow::{anyhow, Context, Result};
use eframe::egui;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::runtime::Runtime;
use tokio::sync::oneshot;

const TOKEN_MIN_BYTES: usize = 32;
const TOKEN_MAX_BYTES: usize = 4096;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct ClientSettings {
    server: String,
    server_key: String,
    token_file: String,
    device_id: String,
    target: String,
}

impl Default for ClientSettings {
    fn default() -> Self {
        Self {
            server: "203.0.113.10:24443".to_owned(),
            server_key: String::new(),
            token_file: String::new(),
            device_id: default_device_id(),
            target: "127.0.0.1:22".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct ConnectSettings {
    server: String,
    server_key: String,
    token_file: String,
    user: String,
}

impl Default for ConnectSettings {
    fn default() -> Self {
        Self {
            server: "203.0.113.10:24443".to_owned(),
            server_key: String::new(),
            token_file: String::new(),
            user: default_user(),
        }
    }
}

pub struct ClientApp {
    settings: ClientSettings,
    status: String,
    stop_tx: Option<oneshot::Sender<()>>,
    agent_thread: Option<JoinHandle<()>>,
    status_rx: Option<Receiver<String>>,
}

impl ClientApp {
    pub fn new(_creation_context: &eframe::CreationContext<'_>) -> Self {
        Self {
            settings: load_settings("client.json"),
            status: "未运行".to_owned(),
            stop_tx: None,
            agent_thread: None,
            status_rx: None,
        }
    }

    fn save(&mut self) {
        match save_settings("client.json", &self.settings) {
            Ok(()) => self.status = "配置已保存".to_owned(),
            Err(error) => self.status = format!("保存失败：{error}"),
        }
    }

    fn start(&mut self) {
        if self.agent_thread.is_some() {
            self.status = "agent 已在运行或正在停止".to_owned();
            return;
        }

        let config = match self.agent_config() {
            Ok(config) => config,
            Err(error) => {
                self.status = format!("配置无效：{error}");
                return;
            }
        };
        if let Err(error) = save_settings("client.json", &self.settings) {
            self.status = format!("保存失败：{error}");
            return;
        }

        let (stop_tx, stop_rx) = oneshot::channel();
        let (status_tx, status_rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            let result = match Runtime::new() {
                Ok(runtime) => runtime.block_on(agent::run_until_stopped(config, stop_rx)),
                Err(error) => Err(anyhow!(error)),
            };
            let message = match result {
                Ok(()) => "agent 已停止".to_owned(),
                Err(error) => format!("agent 后台线程退出：{error}"),
            };
            let _ = status_tx.send(message);
        });

        self.stop_tx = Some(stop_tx);
        self.agent_thread = Some(thread);
        self.status_rx = Some(status_rx);
        self.status = "agent 运行中，正在连接 relay…".to_owned();
    }

    fn stop(&mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
            self.status = "正在停止 agent…".to_owned();
        } else {
            self.status = "agent 未运行".to_owned();
        }
    }

    fn poll_agent(&mut self) {
        if let Some(receiver) = self.status_rx.take() {
            match receiver.try_recv() {
                Ok(message) => self.status = message,
                Err(TryRecvError::Empty) => self.status_rx = Some(receiver),
                Err(TryRecvError::Disconnected) => {}
            }
        }

        let finished = self
            .agent_thread
            .as_ref()
            .map(JoinHandle::is_finished)
            .unwrap_or(false);
        if finished {
            if let Some(thread) = self.agent_thread.take() {
                let _ = thread.join();
            }
            self.stop_tx = None;
        }
    }

    fn agent_config(&self) -> Result<agent::Config> {
        let server = required_value(&self.settings.server, "relay 地址")?;
        let server_key = required_path(&self.settings.server_key, "relay 公钥文件")?;
        let token_file = required_path(&self.settings.token_file, "token 文件")?;
        let device_id = required_value(&self.settings.device_id, "设备 ID")?;
        let target = required_value(&self.settings.target, "本地 SSH 目标")?;
        if !device_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(anyhow!("设备 ID 只能包含字母、数字、.、_、-"));
        }

        Ok(agent::Config {
            server,
            server_key,
            token: read_token_file(&token_file)?,
            device_id,
            target,
        })
    }
}

impl eframe::App for ClientApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_agent();
        egui::CentralPanel::default().show(context, |ui| {
            ui.heading("rust-ssh client");
            ui.label("Windows 被控端");
            ui.add_space(8.0);
            text_field(ui, "Relay", &mut self.settings.server);
            text_field(ui, "Server 公钥", &mut self.settings.server_key);
            text_field(ui, "Token 文件", &mut self.settings.token_file);
            text_field(ui, "设备 ID", &mut self.settings.device_id);
            text_field(ui, "本地 SSH（loopback）", &mut self.settings.target);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if self.agent_thread.is_some() {
                    if ui.button("停止 agent").clicked() {
                        self.stop();
                    }
                } else if ui.button("启动 agent").clicked() {
                    self.start();
                }
                if ui.button("保存配置").clicked() {
                    self.save();
                }
            });
            ui.add_space(8.0);
            ui.label(&self.status);
            ui.separator();
            ui.small("agent 只主动连接 relay，不监听公网端口；SSH 目标必须是本机 loopback。");
        });
        context.request_repaint_after(Duration::from_millis(250));
    }
}

impl Drop for ClientApp {
    fn drop(&mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        if let Some(thread) = self.agent_thread.take() {
            let _ = thread.join();
        }
    }
}

pub struct ConnectApp {
    settings: ConnectSettings,
    devices: Vec<String>,
    selected_device: Option<String>,
    status: String,
    refresh_rx: Option<Receiver<std::result::Result<Vec<String>, String>>>,
    last_refresh: Instant,
    first_update: bool,
}

impl ConnectApp {
    pub fn new(_creation_context: &eframe::CreationContext<'_>) -> Self {
        Self {
            settings: load_settings("connect.json"),
            devices: Vec::new(),
            selected_device: None,
            status: "等待刷新设备".to_owned(),
            refresh_rx: None,
            last_refresh: Instant::now(),
            first_update: true,
        }
    }

    fn save(&mut self) {
        match save_settings("connect.json", &self.settings) {
            Ok(()) => self.status = "配置已保存".to_owned(),
            Err(error) => self.status = format!("保存失败：{error}"),
        }
    }

    fn refresh(&mut self) {
        if self.refresh_rx.is_some() {
            return;
        }
        let server = match required_value(&self.settings.server, "relay 地址") {
            Ok(value) => value,
            Err(error) => {
                self.status = format!("配置无效：{error}");
                return;
            }
        };
        let server_key = match required_path(&self.settings.server_key, "relay 公钥文件") {
            Ok(value) => value,
            Err(error) => {
                self.status = format!("配置无效：{error}");
                return;
            }
        };
        let token_file = match required_path(&self.settings.token_file, "token 文件") {
            Ok(value) => value,
            Err(error) => {
                self.status = format!("配置无效：{error}");
                return;
            }
        };
        let token = match read_token_file(&token_file) {
            Ok(token) => token,
            Err(error) => {
                self.status = format!("token 无效：{error}");
                return;
            }
        };

        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = match Runtime::new() {
                Ok(runtime) => runtime
                    .block_on(connect::list_devices(ListConfig {
                        server,
                        server_key,
                        token,
                    }))
                    .map(|devices| devices.into_iter().map(|device| device.device_id).collect())
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            let _ = sender.send(result);
        });
        self.refresh_rx = Some(receiver);
        self.status = "正在刷新设备列表…".to_owned();
    }

    fn poll_refresh(&mut self) {
        let Some(receiver) = self.refresh_rx.take() else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(devices)) => {
                let previous = self.selected_device.clone();
                self.devices = devices;
                self.selected_device =
                    previous.filter(|id| self.devices.iter().any(|item| item == id));
                self.status = format!("已刷新，在线设备 {} 台", self.devices.len());
                self.last_refresh = Instant::now();
            }
            Ok(Err(error)) => {
                self.status = format!("刷新失败：{error}");
                self.last_refresh = Instant::now();
            }
            Err(TryRecvError::Empty) => self.refresh_rx = Some(receiver),
            Err(TryRecvError::Disconnected) => {
                self.status = "刷新线程已退出".to_owned();
                self.last_refresh = Instant::now();
            }
        }
    }

    fn connect_selected(&mut self) {
        let Some(device_id) = self.selected_device.clone() else {
            self.status = "请先选择在线设备".to_owned();
            return;
        };
        if let Err(error) = self.validate_connection() {
            self.status = format!("配置无效：{error}");
            return;
        }
        match write_session_config(&self.settings, &device_id) {
            Ok(path) => match launch_ssh_terminal(&path) {
                Ok(()) => self.status = format!("已打开 SSH：{device_id}"),
                Err(error) => self.status = format!("打开 SSH 失败：{error}"),
            },
            Err(error) => self.status = format!("生成 SSH 配置失败：{error}"),
        }
    }

    fn validate_connection(&self) -> Result<()> {
        required_value(&self.settings.server, "relay 地址")?;
        required_path(&self.settings.server_key, "relay 公钥文件")?;
        let token_file = required_path(&self.settings.token_file, "token 文件")?;
        read_token_file(&token_file)?;
        let user = required_value(&self.settings.user, "SSH 用户名")?;
        if user.chars().any(char::is_whitespace) {
            return Err(anyhow!("SSH 用户名不能包含空白字符"));
        }
        Ok(())
    }
}

impl eframe::App for ConnectApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_refresh();
        if self.first_update {
            self.first_update = false;
            if !self.settings.server.trim().is_empty() {
                self.refresh();
            }
        } else if self.refresh_rx.is_none()
            && self.last_refresh.elapsed() >= Duration::from_secs(10)
            && !self.settings.server.trim().is_empty()
        {
            self.refresh();
        }

        egui::CentralPanel::default().show(context, |ui| {
            ui.heading("rust-ssh connect");
            ui.label("主控端：在线设备和 SSH 连接");
            ui.add_space(8.0);
            text_field(ui, "Relay", &mut self.settings.server);
            text_field(ui, "Server 公钥", &mut self.settings.server_key);
            text_field(ui, "Token 文件", &mut self.settings.token_file);
            text_field(ui, "SSH 用户", &mut self.settings.user);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("刷新设备").clicked() {
                    self.refresh();
                }
                if ui.button("保存配置").clicked() {
                    self.save();
                }
            });
            ui.add_space(8.0);
            ui.label(format!("在线设备（{}）", self.devices.len()));
            egui::ScrollArea::vertical()
                .max_height(170.0)
                .show(ui, |ui| {
                    for device in &self.devices {
                        let selected = self.selected_device.as_deref() == Some(device.as_str());
                        if ui.selectable_label(selected, device).clicked() {
                            self.selected_device = Some(device.clone());
                        }
                    }
                    if self.devices.is_empty() {
                        ui.small("暂无在线 agent");
                    }
                });
            ui.add_space(8.0);
            if ui
                .add_enabled(
                    self.selected_device.is_some(),
                    egui::Button::new("连接选中设备"),
                )
                .clicked()
            {
                self.connect_selected();
            }
            ui.add_space(8.0);
            ui.label(&self.status);
            ui.separator();
            ui.small("连接会打开系统 SSH 终端；不同设备可以同时连接，单台设备仍限制一个活动会话。");
        });
        context.request_repaint_after(Duration::from_millis(250));
    }
}

fn text_field(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add_sized(egui::vec2(430.0, 24.0), egui::TextEdit::singleline(value));
    });
}

fn required_value(value: &str, label: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(anyhow!("{label}不能为空"));
    }
    Ok(value.to_owned())
}

fn required_path(value: &str, label: &str) -> Result<PathBuf> {
    let path = required_value(value, label)?;
    let path = PathBuf::from(path);
    if !path.is_file() {
        return Err(anyhow!("{label}不存在：{}", path.display()));
    }
    Ok(path)
}

fn read_token_file(path: &Path) -> Result<String> {
    let token =
        fs::read_to_string(path).with_context(|| format!("读取 token 文件 {}", path.display()))?;
    let token = token.trim().to_owned();
    if token.len() < TOKEN_MIN_BYTES {
        return Err(anyhow!("token 至少需要 {TOKEN_MIN_BYTES} 个非空白字节"));
    }
    if token.len() > TOKEN_MAX_BYTES {
        return Err(anyhow!("token 超过 {TOKEN_MAX_BYTES} 字节"));
    }
    Ok(token)
}

fn load_settings<T>(name: &str) -> T
where
    T: DeserializeOwned + Default,
{
    let path = config_dir().join(name);
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_settings<T>(name: &str, settings: &T) -> Result<()>
where
    T: Serialize,
{
    let directory = config_dir();
    fs::create_dir_all(&directory)
        .with_context(|| format!("创建配置目录 {}", directory.display()))?;
    let path = directory.join(name);
    let text = serde_json::to_vec_pretty(settings).context("编码 GUI 配置")?;
    fs::write(&path, text).with_context(|| format!("写入 GUI 配置 {}", path.display()))
}

fn config_dir() -> PathBuf {
    #[cfg(windows)]
    if let Some(path) = std::env::var_os("APPDATA") {
        return PathBuf::from(path).join("rust-ssh");
    }

    if let Some(path) = std::env::var_os("HOME") {
        return PathBuf::from(path).join(".config").join("rust-ssh");
    }
    PathBuf::from(".").join("rust-ssh")
}

fn default_device_id() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "windows-agent".to_owned())
}

fn default_user() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "ame".to_owned())
}

fn write_session_config(settings: &ConnectSettings, device_id: &str) -> Result<PathBuf> {
    let directory = config_dir().join("sessions");
    fs::create_dir_all(&directory)
        .with_context(|| format!("创建 SSH 会话目录 {}", directory.display()))?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let path = directory.join(format!("session-{timestamp}.conf"));
    let executable = std::env::current_exe().context("定位 rust-ssh-connect 可执行文件")?;
    let executable = executable
        .to_str()
        .ok_or_else(|| anyhow!("rust-ssh-connect 路径不是有效 UTF-8"))?;
    let server_key = required_value(&settings.server_key, "relay 公钥文件")?;
    let token_file = required_value(&settings.token_file, "token 文件")?;
    let server = required_value(&settings.server, "relay 地址")?;
    let user = required_value(&settings.user, "SSH 用户名")?;

    let text = format!(
        "Host rust-ssh-session\n\
    HostName rust-ssh-proxy\n\
    HostKeyAlias {device_id}\n\
    User {user}\n\
    ProxyCommand {} --proxy --server {} --server-key {} --token-file {} --target {}\n",
        shell_double_quote(executable),
        shell_double_quote(&server),
        shell_double_quote(&server_key),
        shell_double_quote(&token_file),
        shell_double_quote(device_id),
    );
    fs::write(&path, text).with_context(|| format!("写入 SSH 会话配置 {}", path.display()))?;
    Ok(path)
}

fn launch_ssh_terminal(config_path: &Path) -> Result<()> {
    let config_path = config_path
        .to_str()
        .ok_or_else(|| anyhow!("SSH 配置路径不是有效 UTF-8"))?;

    #[cfg(windows)]
    {
        Command::new("cmd.exe")
            .arg("/K")
            .arg("ssh")
            .arg("-F")
            .arg(config_path)
            .arg("rust-ssh-session")
            .spawn()
            .context("启动 Windows SSH 终端")?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        let command = format!(
            "ssh -F {} rust-ssh-session",
            shell_double_quote(config_path)
        );
        let script = format!(
            "tell application \"Terminal\" to do script {}",
            apple_script_string(&command)
        );
        Command::new("osascript")
            .arg("-e")
            .arg(script)
            .spawn()
            .context("启动 macOS Terminal")
            .map(|_| ())
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        Command::new("ssh")
            .arg("-F")
            .arg(config_path)
            .arg("rust-ssh-session")
            .spawn()
            .context("启动 SSH")?;
        Ok(())
    }
}

#[cfg(windows)]
fn shell_double_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

#[cfg(not(windows))]
fn shell_double_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        if matches!(character, '"' | '\\' | '$' | '`') {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    quoted.push('"');
    quoted
}

#[cfg(target_os = "macos")]
fn apple_script_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        if matches!(character, '"' | '\\') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped.push('"');
    escaped
}
