use crate::agent;
use crate::bootstrap;
use crate::connect::{self, ListConfig};
use anyhow::{anyhow, Context, Result};
use eframe::egui;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;
use tokio::sync::oneshot;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct ClientSettings {
    pairing_code: String,
    device_id: String,
    target: String,
}

impl Default for ClientSettings {
    fn default() -> Self {
        Self {
            pairing_code: String::new(),
            device_id: default_device_id(),
            target: "127.0.0.1:22".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct ConnectSettings {
    pairing_code: String,
    user: String,
}

impl Default for ConnectSettings {
    fn default() -> Self {
        Self {
            pairing_code: String::new(),
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

        let config = match client_agent_config(&self.settings) {
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
        self.status = "已启动，正在连接服务器…".to_owned();
    }

    fn stop(&mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
            self.status = "正在停止…".to_owned();
        } else {
            self.status = "未运行".to_owned();
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
}

impl eframe::App for ClientApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_agent();
        egui::CentralPanel::default().show(context, |ui| {
            ui.heading("rust-ssh client");
            ui.label("Windows 被控端");
            ui.add_space(8.0);
            text_area(
                ui,
                "配置码",
                &mut self.settings.pairing_code,
                "从服务器执行 pair-code 后复制整段内容",
            );
            text_field(ui, "设备 ID", &mut self.settings.device_id);
            text_field(ui, "本地 SSH", &mut self.settings.target);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if self.agent_thread.is_some() {
                    if ui.button("停止").clicked() {
                        self.stop();
                    }
                } else if ui.button("启动").clicked() {
                    self.start();
                }
                if ui.button("保存").clicked() {
                    self.save();
                }
            });
            ui.add_space(8.0);
            ui.label(&self.status);
            ui.separator();
            ui.small("启动后保持窗口打开；client 只主动连接服务器，不监听公网端口。关闭窗口会停止 client。");
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
            status: "请粘贴配置码".to_owned(),
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
        let pairing = match bootstrap::decode(&self.settings.pairing_code) {
            Ok(pairing) => pairing,
            Err(error) => {
                self.status = format!("配置码无效：{error}");
                return;
            }
        };
        if let Err(error) = save_settings("connect.json", &self.settings) {
            self.status = format!("保存配置失败：{error}");
            return;
        }

        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = match Runtime::new() {
                Ok(runtime) => runtime
                    .block_on(connect::list_devices(ListConfig {
                        server: pairing.server,
                        server_key: pairing.server_key,
                        token: pairing.token,
                    }))
                    .map(|devices| devices.into_iter().map(|device| device.device_id).collect())
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            let _ = sender.send(result);
        });
        self.refresh_rx = Some(receiver);
        self.status = "正在查找在线设备…".to_owned();
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
                self.status = format!("已找到 {} 台在线设备", self.devices.len());
                self.last_refresh = Instant::now();
            }
            Ok(Err(error)) => {
                self.status = format!("查找失败：{error}");
                self.last_refresh = Instant::now();
            }
            Err(TryRecvError::Empty) => self.refresh_rx = Some(receiver),
            Err(TryRecvError::Disconnected) => {
                self.status = "查找线程已退出".to_owned();
                self.last_refresh = Instant::now();
            }
        }
    }

    fn connect_selected(&mut self) {
        let Some(device_id) = self.selected_device.clone() else {
            self.status = "请先选择设备".to_owned();
            return;
        };
        if let Err(error) = self.validate_connection() {
            self.status = format!("配置无效：{error}");
            return;
        }
        match install_ssh_host(&self.settings, &device_id) {
            Ok(host) => match launch_ssh_terminal(&host) {
                Ok(()) => self.status = format!("已打开 SSH：{device_id}"),
                Err(error) => self.status = format!("打开 SSH 失败：{error}"),
            },
            Err(error) => self.status = format!("配置 SSH 失败：{error}"),
        }
    }

    fn configure_selected(&mut self) {
        let Some(device_id) = self.selected_device.clone() else {
            self.status = "请先选择设备".to_owned();
            return;
        };
        if let Err(error) = self.validate_connection() {
            self.status = format!("配置无效：{error}");
            return;
        }
        match install_ssh_host(&self.settings, &device_id) {
            Ok(host) => self.status = format!("已配置 SSH：{host}，Terminal/VS Code 均可使用"),
            Err(error) => self.status = format!("配置 SSH 失败：{error}"),
        }
    }

    fn validate_connection(&self) -> Result<()> {
        bootstrap::decode(&self.settings.pairing_code)?;
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
            if !self.settings.pairing_code.trim().is_empty() {
                self.refresh();
            }
        } else if self.refresh_rx.is_none()
            && self.last_refresh.elapsed() >= Duration::from_secs(10)
            && !self.settings.pairing_code.trim().is_empty()
        {
            self.refresh();
        }

        egui::CentralPanel::default().show(context, |ui| {
            ui.heading("rust-ssh connect");
            ui.label("主控端");
            ui.add_space(8.0);
            text_area(
                ui,
                "配置码",
                &mut self.settings.pairing_code,
                "与 client 使用服务器生成的同一配置码",
            );
            text_field(ui, "SSH 用户", &mut self.settings.user);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("刷新设备").clicked() {
                    self.refresh();
                }
                if ui.button("保存").clicked() {
                    self.save();
                }
            });
            ui.add_space(8.0);
            ui.label(format!("在线设备（{}）", self.devices.len()));
            egui::ScrollArea::vertical()
                .max_height(150.0)
                .show(ui, |ui| {
                    for device in &self.devices {
                        let selected = self.selected_device.as_deref() == Some(device.as_str());
                        if ui.selectable_label(selected, device).clicked() {
                            self.selected_device = Some(device.clone());
                        }
                    }
                    if self.devices.is_empty() {
                        ui.small("暂无在线 client");
                    }
                });
            ui.add_space(8.0);
            if ui
                .add_enabled(
                    self.selected_device.is_some(),
                    egui::Button::new("配置 SSH"),
                )
                .clicked()
            {
                self.configure_selected();
            }
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
            ui.small("配置一次后，可直接使用生成的 rust-ssh-设备名 连接 Terminal 或 VS Code。");
        });
        context.request_repaint_after(Duration::from_millis(250));
    }
}

fn client_agent_config(settings: &ClientSettings) -> Result<agent::Config> {
    let pairing = bootstrap::decode(&settings.pairing_code)?;
    let device_id = required_value(&settings.device_id, "设备 ID")?;
    if !valid_device_id(&device_id) {
        return Err(anyhow!("设备 ID 只能包含字母、数字、.、_、-"));
    }
    let target = required_value(&settings.target, "本地 SSH 目标")?;
    validate_loopback_target(&target)?;

    Ok(agent::Config {
        server: pairing.server,
        server_key: pairing.server_key,
        token: pairing.token,
        device_id,
        target,
    })
}

fn valid_device_id(device_id: &str) -> bool {
    !device_id.is_empty()
        && device_id.len() <= 128
        && device_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn validate_loopback_target(target: &str) -> Result<()> {
    let address: SocketAddr = target
        .parse()
        .map_err(|_| anyhow!("本地 SSH 必须是 loopback IP:端口，例如 127.0.0.1:22"))?;
    if !address.ip().is_loopback() {
        return Err(anyhow!("本地 SSH 只能连接本机 loopback 地址"));
    }
    Ok(())
}

fn text_field(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add_sized(
            egui::vec2(ui.available_width(), 24.0),
            egui::TextEdit::singleline(value),
        );
    });
}

fn text_area(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) {
    ui.label(label);
    ui.add_sized(
        egui::vec2(ui.available_width(), 76.0),
        egui::TextEdit::multiline(value).desired_rows(3),
    );
    ui.small(hint);
}

fn required_value(value: &str, label: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(anyhow!("{label}不能为空"));
    }
    Ok(value.to_owned())
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

const MANAGED_SSH_BEGIN: &str = "# >>> rust-ssh managed begin >>>";
const MANAGED_SSH_END: &str = "# <<< rust-ssh managed end <<<";

fn install_ssh_host(settings: &ConnectSettings, device_id: &str) -> Result<String> {
    if !valid_device_id(device_id) {
        return Err(anyhow!("设备 ID 包含不支持的字符"));
    }
    save_settings("connect.json", settings)?;
    let executable = std::env::current_exe().context("定位 rust-ssh-connect 可执行文件")?;
    let executable = executable
        .to_str()
        .ok_or_else(|| anyhow!("rust-ssh-connect 路径不是有效 UTF-8"))?;
    let setup_code = required_value(&settings.pairing_code, "配置码")?;
    let setup_code_path = write_setup_code_file(&setup_code)?;
    let setup_code_path = setup_code_path
        .to_str()
        .ok_or_else(|| anyhow!("配置码文件路径不是有效 UTF-8"))?;
    let user = required_value(&settings.user, "SSH 用户名")?;
    let host = format!("rust-ssh-{device_id}");

    let text = format!(
        "Host {host}\n\
    HostName rust-ssh-proxy\n\
    HostKeyAlias {device_id}\n\
    User {user}\n\
    ProxyCommand {} --proxy --setup-code-file {} --target {}\n",
        shell_double_quote(executable),
        shell_double_quote(setup_code_path),
        shell_double_quote(device_id),
    );
    let path = user_ssh_config_path()?;
    update_managed_ssh_config(&path, &text)?;
    Ok(host)
}

fn user_ssh_config_path() -> Result<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| anyhow!("找不到用户目录，无法配置 SSH"))?;
    Ok(PathBuf::from(home).join(".ssh").join("config"))
}

fn update_managed_ssh_config(path: &Path, host_block: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建 SSH 配置目录 {}", parent.display()))?;
    }
    let existing = if path.exists() {
        fs::read_to_string(path).with_context(|| format!("读取 SSH 配置 {}", path.display()))?
    } else {
        String::new()
    };
    let managed = format!("{MANAGED_SSH_BEGIN}\n{host_block}{MANAGED_SSH_END}\n");
    let updated = if let Some(begin) = existing.find(MANAGED_SSH_BEGIN) {
        let after_begin = &existing[begin..];
        let end_offset = after_begin
            .find(MANAGED_SSH_END)
            .ok_or_else(|| anyhow!("已有 rust-ssh SSH 配置块不完整"))?;
        let end = begin + end_offset + MANAGED_SSH_END.len();
        format!("{}{}{}", &existing[..begin], managed, &existing[end..])
    } else {
        let mut updated = existing.trim_end().to_owned();
        if !updated.is_empty() {
            updated.push_str("\n\n");
        }
        updated.push_str(&managed);
        updated
    };
    fs::write(path, updated).with_context(|| format!("写入 SSH 配置 {}", path.display()))?;
    set_private_permissions(path)
}

fn write_setup_code_file(code: &str) -> Result<PathBuf> {
    let directory = config_dir();
    fs::create_dir_all(&directory)
        .with_context(|| format!("创建配置目录 {}", directory.display()))?;
    let path = directory.join("connect.setup");
    fs::write(&path, code).with_context(|| format!("写入配置码文件 {}", path.display()))?;
    set_private_permissions(&path)?;
    Ok(path)
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("保护文件 {}", path.display()))
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

fn launch_ssh_terminal(host: &str) -> Result<()> {
    #[cfg(windows)]
    {
        Command::new("cmd.exe")
            .arg("/K")
            .arg("ssh")
            .arg(host)
            .spawn()
            .context("启动 Windows SSH 终端")?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        let command = format!("ssh {}", shell_double_quote(host));
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
        Command::new("ssh").arg(host).spawn().context("启动 SSH")?;
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
