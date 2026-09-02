use crate::agent;
use crate::bootstrap;
use crate::connect::{self, ListConfig};
use crate::device_id;
use anyhow::{anyhow, Context, Result};
use eframe::egui;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;
use tokio::sync::oneshot;

#[cfg(windows)]
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
#[cfg(windows)]
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[cfg(windows)]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

const CLIENT_CONFIG_VERSION: u8 = 1;
const CJK_FONT_NAME: &str = "rust-ssh-system-cjk";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientConnectionState {
    Stopped,
    Connecting,
    Connected,
    Retrying,
    Error,
}

fn connection_state_color(state: ClientConnectionState) -> egui::Color32 {
    match state {
        ClientConnectionState::Stopped => egui::Color32::GRAY,
        ClientConnectionState::Connecting | ClientConnectionState::Retrying => {
            egui::Color32::from_rgb(235, 170, 35)
        }
        ClientConnectionState::Connected => egui::Color32::from_rgb(40, 185, 95),
        ClientConnectionState::Error => egui::Color32::from_rgb(220, 70, 70),
    }
}

#[cfg(windows)]
struct AppTray {
    icon: TrayIcon,
    app_name: &'static str,
}

#[cfg(windows)]
#[derive(Default)]
struct TraySignals {
    show: AtomicBool,
    quit: AtomicBool,
}

#[cfg(windows)]
impl AppTray {
    fn new(app_name: &'static str) -> Option<Self> {
        let show = MenuItem::with_id("show", "显示主界面", true, None);
        let quit = MenuItem::with_id("quit", "关闭", true, None);
        let menu = Menu::new();
        menu.append_items(&[&show, &quit]).ok()?;
        let icon = tray_icon_for_state(ClientConnectionState::Stopped)?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(app_name)
            .with_menu_on_left_click(false)
            .with_menu_on_right_click(true)
            .with_icon(icon)
            .build()
            .ok()?;
        Some(Self { icon, app_name })
    }

    fn set_state(&self, state: ClientConnectionState) {
        if let Some(icon) = tray_icon_for_state(state) {
            let _ = self.icon.set_icon(Some(icon));
        }
        let status = match state {
            ClientConnectionState::Stopped => "未运行",
            ClientConnectionState::Connecting => "正在连接",
            ClientConnectionState::Connected => "已连接",
            ClientConnectionState::Retrying => "正在重试",
            ClientConnectionState::Error => "连接失败",
        };
        let tooltip = format!("{}：{status}", self.app_name);
        let _ = self.icon.set_tooltip(Some(&tooltip));
    }
}

#[cfg(windows)]
fn tray_icon_for_state(state: ClientConnectionState) -> Option<Icon> {
    let color = connection_state_color(state);
    let size = 16u32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let dx = x as i32 - 7;
            let dy = y as i32 - 7;
            if dx * dx + dy * dy <= 42 {
                let index = ((y * size + x) * 4) as usize;
                rgba[index] = color.r();
                rgba[index + 1] = color.g();
                rgba[index + 2] = color.b();
                rgba[index + 3] = 255;
            }
        }
    }
    Icon::from_rgba(rgba, size, size).ok()
}

#[cfg(windows)]
fn install_tray_event_handlers(signals: &Arc<TraySignals>, context: &egui::Context) {
    let show_signals = Arc::clone(signals);
    let show_context = context.clone();
    TrayIconEvent::set_event_handler(Some(move |event| {
        let should_show = matches!(
            event,
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } | TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            }
        );
        if should_show {
            show_signals.show.store(true, Ordering::Release);
            show_context.request_repaint();
        }
    }));

    let menu_signals = Arc::clone(signals);
    let menu_context = context.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        match event.id().as_ref() {
            "show" => menu_signals.show.store(true, Ordering::Release),
            "quit" => menu_signals.quit.store(true, Ordering::Release),
            _ => return,
        }
        menu_context.request_repaint();
    }));
}

#[cfg(windows)]
pub struct SingleInstance {
    handle: *mut std::ffi::c_void,
}

#[cfg(windows)]
impl SingleInstance {
    pub fn acquire(name: &str) -> Option<Self> {
        use std::iter::once;
        use std::os::windows::ffi::OsStrExt;

        let name: Vec<u16> = std::ffi::OsStr::new(name)
            .encode_wide()
            .chain(once(0))
            .collect();
        let handle = unsafe { CreateMutexW(std::ptr::null_mut(), 0, name.as_ptr()) };
        if handle.is_null() {
            return None;
        }
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe {
                CloseHandle(handle);
            }
            return None;
        }
        Some(Self { handle })
    }
}

#[cfg(windows)]
impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
pub fn set_windows_app_user_model_id(id: &str) {
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;

    let id: Vec<u16> = std::ffi::OsStr::new(id)
        .encode_wide()
        .chain(once(0))
        .collect();
    let result = unsafe { SetCurrentProcessExplicitAppUserModelID(id.as_ptr()) };
    if result != 0 {
        tracing::debug!(code = result, "could not set Windows AppUserModelID");
    }
}

#[cfg(windows)]
const ERROR_ALREADY_EXISTS: u32 = 183;

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    fn CreateMutexW(
        attributes: *mut std::ffi::c_void,
        initial_owner: i32,
        name: *const u16,
    ) -> *mut std::ffi::c_void;
    fn GetLastError() -> u32;
}

#[cfg(windows)]
#[link(name = "shell32")]
unsafe extern "system" {
    fn SetCurrentProcessExplicitAppUserModelID(app_id: *const u16) -> i32;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct ClientSettings {
    pairing_code: String,
    device_id: String,
    target: String,
    config_version: u8,
}

impl Default for ClientSettings {
    fn default() -> Self {
        Self {
            pairing_code: String::new(),
            device_id: new_device_id(),
            target: "127.0.0.1:22".to_owned(),
            config_version: CLIENT_CONFIG_VERSION,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct ConnectSettings {
    pairing_code: String,
    user: String,
    host_aliases: BTreeMap<String, String>,
}

impl Default for ConnectSettings {
    fn default() -> Self {
        Self {
            pairing_code: String::new(),
            user: default_user(),
            host_aliases: BTreeMap::new(),
        }
    }
}

pub struct ClientApp {
    settings: ClientSettings,
    status: String,
    connection_state: ClientConnectionState,
    stop_tx: Option<oneshot::Sender<()>>,
    agent_thread: Option<JoinHandle<()>>,
    status_rx: Option<Receiver<agent::Status>>,
    #[cfg(windows)]
    tray: Option<AppTray>,
    #[cfg(windows)]
    tray_signals: Arc<TraySignals>,
    #[cfg(windows)]
    allow_close: bool,
}

impl ClientApp {
    pub fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
        install_cjk_font(&creation_context.egui_ctx);
        install_ui_style(&creation_context.egui_ctx);
        #[cfg(windows)]
        let tray_signals = Arc::new(TraySignals::default());
        #[cfg(windows)]
        install_tray_event_handlers(&tray_signals, &creation_context.egui_ctx);
        Self {
            settings: load_client_settings(),
            status: "未运行".to_owned(),
            connection_state: ClientConnectionState::Stopped,
            stop_tx: None,
            agent_thread: None,
            status_rx: None,
            #[cfg(windows)]
            tray: AppTray::new("Rust-SSH-Client"),
            #[cfg(windows)]
            tray_signals,
            #[cfg(windows)]
            allow_close: false,
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
                Ok(runtime) => runtime.block_on(agent::run_until_stopped_with_status(
                    config,
                    stop_rx,
                    status_tx.clone(),
                )),
                Err(error) => Err(anyhow!(error)),
            };
            if let Err(error) = result {
                let _ = status_tx.send(agent::Status::Failed(format!(
                    "agent 后台线程退出：{error}"
                )));
            }
        });

        self.stop_tx = Some(stop_tx);
        self.agent_thread = Some(thread);
        self.status_rx = Some(status_rx);
        self.set_connection_state(
            ClientConnectionState::Connecting,
            "正在连接服务器…".to_owned(),
        );
    }

    fn stop(&mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
            self.set_connection_state(ClientConnectionState::Stopped, "正在断开…".to_owned());
        } else {
            self.set_connection_state(ClientConnectionState::Stopped, "未运行".to_owned());
        }
    }

    fn set_connection_state(&mut self, state: ClientConnectionState, status: String) {
        self.connection_state = state;
        self.status = status;
        #[cfg(windows)]
        if let Some(tray) = &self.tray {
            tray.set_state(state);
        }
    }

    fn poll_agent(&mut self) {
        if let Some(receiver) = self.status_rx.take() {
            let mut receiver = Some(receiver);
            while let Some(current) = receiver.as_ref() {
                match current.try_recv() {
                    Ok(update) => self.apply_agent_status(update),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        receiver = None;
                        break;
                    }
                }
            }
            if let Some(receiver) = receiver {
                self.status_rx = Some(receiver);
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

    fn apply_agent_status(&mut self, update: agent::Status) {
        match update {
            agent::Status::Connecting => self.set_connection_state(
                ClientConnectionState::Connecting,
                "正在连接服务器…".to_owned(),
            ),
            agent::Status::Connected => self.set_connection_state(
                ClientConnectionState::Connected,
                "已连接服务器，等待 SSH 连接".to_owned(),
            ),
            agent::Status::Retrying(reason) => self.set_connection_state(
                ClientConnectionState::Retrying,
                format!("连接中断，正在重试：{reason}"),
            ),
            agent::Status::Stopped => {
                self.set_connection_state(ClientConnectionState::Stopped, "已断开".to_owned())
            }
            agent::Status::Failed(reason) => self
                .set_connection_state(ClientConnectionState::Error, format!("连接失败：{reason}")),
        }
    }

    #[cfg(windows)]
    fn poll_tray(&mut self, context: &egui::Context) {
        if self.tray_signals.quit.swap(false, Ordering::AcqRel) {
            self.allow_close = true;
            self.stop();
            context.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if self.tray_signals.show.swap(false, Ordering::AcqRel) {
            context.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            context.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            context.send_viewport_cmd(egui::ViewportCommand::Focus);
        }

        if context.input(|input| input.viewport().close_requested())
            && !self.allow_close
            && self.tray.is_some()
        {
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            context.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.status = "窗口已隐藏到托盘，client 仍在运行".to_owned();
        }
    }
}

impl eframe::App for ClientApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_agent();
        #[cfg(windows)]
        self.poll_tray(context);
        egui::CentralPanel::default().show(context, |ui| {
            ui.heading("Rust-SSH-Client");
            ui.colored_label(egui::Color32::GRAY, "Windows 被控端");
            ui.add_space(8.0);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                text_area(
                    ui,
                    "配置码",
                    &mut self.settings.pairing_code,
                    "从服务器 device add 命令输出的设备配置码复制整段内容",
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("设备 ID");
                    ui.monospace(&self.settings.device_id);
                    if ui.button("复制").clicked() {
                        ui.ctx().copy_text(self.settings.device_id.clone());
                        self.status = "设备 ID 已复制".to_owned();
                    }
                });
                ui.small("首次启动时随机生成并保存；与 Windows 计算机名无关。需要更换身份时请重新注册新设备。");
                text_field(ui, "本地 SSH", &mut self.settings.target);
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if self.agent_thread.is_some() {
                    if ui.button("断开连接").clicked() {
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
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(connection_state_color(self.connection_state), "●");
                    ui.label(&self.status);
                });
            });
            ui.add_space(4.0);
            ui.small("关闭窗口会隐藏到右下角托盘；托盘菜单中的“关闭”才会停止 Rust-SSH-Client。它只主动连接服务器，不监听公网端口。");
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
    connection_state: ClientConnectionState,
    refresh_rx: Option<Receiver<std::result::Result<Vec<String>, String>>>,
    last_refresh: Instant,
    first_update: bool,
    host_editor: Option<HostAliasEditor>,
    #[cfg(windows)]
    tray: Option<AppTray>,
    #[cfg(windows)]
    tray_signals: Arc<TraySignals>,
    #[cfg(windows)]
    allow_close: bool,
}

struct HostAliasEditor {
    device_id: String,
    value: String,
}

impl ConnectApp {
    pub fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
        install_cjk_font(&creation_context.egui_ctx);
        install_ui_style(&creation_context.egui_ctx);
        #[cfg(windows)]
        let tray_signals = Arc::new(TraySignals::default());
        #[cfg(windows)]
        install_tray_event_handlers(&tray_signals, &creation_context.egui_ctx);
        Self {
            settings: load_settings("connect.json"),
            devices: Vec::new(),
            selected_device: None,
            status: "请粘贴配置码".to_owned(),
            connection_state: ClientConnectionState::Stopped,
            refresh_rx: None,
            last_refresh: Instant::now(),
            first_update: true,
            host_editor: None,
            #[cfg(windows)]
            tray: AppTray::new("Rust-SSH-Connect"),
            #[cfg(windows)]
            tray_signals,
            #[cfg(windows)]
            allow_close: false,
        }
    }

    fn set_connection_state(&mut self, state: ClientConnectionState, status: String) {
        self.connection_state = state;
        self.status = status;
        #[cfg(windows)]
        if let Some(tray) = &self.tray {
            tray.set_state(state);
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
                self.set_connection_state(
                    ClientConnectionState::Error,
                    format!("配置码无效：{error}"),
                );
                return;
            }
        };
        if let Err(error) = save_settings("connect.json", &self.settings) {
            self.set_connection_state(
                ClientConnectionState::Error,
                format!("保存配置失败：{error}"),
            );
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
        self.set_connection_state(
            ClientConnectionState::Connecting,
            "正在查找在线设备…".to_owned(),
        );
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
                self.set_connection_state(
                    ClientConnectionState::Connected,
                    format!("已找到 {} 台在线设备", self.devices.len()),
                );
                self.last_refresh = Instant::now();
            }
            Ok(Err(error)) => {
                self.set_connection_state(
                    ClientConnectionState::Error,
                    format!("查找失败：{error}"),
                );
                self.last_refresh = Instant::now();
            }
            Err(TryRecvError::Empty) => self.refresh_rx = Some(receiver),
            Err(TryRecvError::Disconnected) => {
                self.set_connection_state(
                    ClientConnectionState::Error,
                    "查找线程已退出".to_owned(),
                );
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
                Ok(()) => self.status = format!("已打开 SSH 终端：{host}；登录结果请看终端窗口"),
                Err(error) => self.status = format!("打开 SSH 失败：{error}"),
            },
            Err(error) => self.status = format!("配置 SSH 失败：{error}"),
        }
    }

    fn host_alias_for(&self, device_id: &str) -> String {
        self.settings
            .host_aliases
            .get(device_id)
            .filter(|alias| valid_host_alias(alias))
            .cloned()
            .unwrap_or_else(|| device_id.to_owned())
    }

    fn save_host_alias(&mut self, device_id: &str, value: &str) {
        let alias = value.trim();
        if alias.is_empty() || alias == device_id {
            self.settings.host_aliases.remove(device_id);
            self.status = "已恢复默认 Host 昵称".to_owned();
        } else if !valid_host_alias(alias) {
            self.status = "Host 昵称只能使用字母、数字、点、下划线和短横线".to_owned();
            return;
        } else if self
            .settings
            .host_aliases
            .iter()
            .any(|(other_id, other_alias)| other_id != device_id && other_alias == alias)
        {
            self.status = "这个 Host 昵称已经被其他设备使用".to_owned();
            return;
        } else {
            self.settings
                .host_aliases
                .insert(device_id.to_owned(), alias.to_owned());
            self.status = format!("Host 昵称已设置为 {alias}");
        }
        if let Err(error) = save_settings("connect.json", &self.settings) {
            self.status = format!("保存 Host 昵称失败：{error}");
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

    fn show_host_editor(&mut self, context: &egui::Context) {
        let Some(editor) = self.host_editor.as_mut() else {
            return;
        };
        let mut save = false;
        let mut cancel = false;
        egui::Window::new("设置 Host 昵称")
            .collapsible(false)
            .resizable(false)
            .show(context, |ui| {
                ui.label(format!("设备 ID：{}", editor.device_id));
                text_field(ui, "Host", &mut editor.value);
                ui.small("之后可以直接执行 ssh Host；只能使用字母、数字、点、下划线和短横线。");
                ui.horizontal(|ui| {
                    if ui.button("保存").clicked() {
                        save = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel = true;
                    }
                });
            });

        if save {
            if let Some(editor) = self.host_editor.take() {
                self.save_host_alias(&editor.device_id, &editor.value);
            }
        } else if cancel {
            self.host_editor = None;
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

    #[cfg(windows)]
    fn poll_tray(&mut self, context: &egui::Context) {
        if self.tray_signals.quit.swap(false, Ordering::AcqRel) {
            self.allow_close = true;
            self.set_connection_state(ClientConnectionState::Stopped, "正在关闭…".to_owned());
            context.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if self.tray_signals.show.swap(false, Ordering::AcqRel) {
            context.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            context.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            context.send_viewport_cmd(egui::ViewportCommand::Focus);
        }

        if context.input(|input| input.viewport().close_requested())
            && !self.allow_close
            && self.tray.is_some()
        {
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            context.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.status = "窗口已隐藏到托盘，Connect 仍可用于 SSH/VS Code".to_owned();
        }
    }
}

impl eframe::App for ConnectApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        #[cfg(windows)]
        self.poll_tray(context);
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
            ui.heading("Rust-SSH-Connect");
            ui.colored_label(egui::Color32::GRAY, "主控端");
            ui.add_space(8.0);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                text_area(
                    ui,
                    "配置码",
                    &mut self.settings.pairing_code,
                    "从服务器为 controller token 生成 pair-code 后复制整段内容",
                );
                text_field(ui, "SSH 用户", &mut self.settings.user);
            });
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
                    let devices = self.devices.clone();
                    for device in devices {
                        let selected = self.selected_device.as_deref() == Some(device.as_str());
                        let alias = self.host_alias_for(&device);
                        let label = if alias == device {
                            alias.clone()
                        } else {
                            format!("{alias}  ({device})")
                        };
                        let response = ui
                            .horizontal(|ui| {
                                ui.colored_label(egui::Color32::from_rgb(40, 185, 95), "●");
                                ui.selectable_label(selected, label)
                            })
                            .inner;
                        if response.clicked() {
                            self.selected_device = Some(device.clone());
                        }
                        response.context_menu(|ui| {
                            if ui.button("设置 Host 昵称").clicked() {
                                self.host_editor = Some(HostAliasEditor {
                                    device_id: device.clone(),
                                    value: alias.clone(),
                                });
                                ui.close_menu();
                            }
                            if ui.button("复制设备 ID").clicked() {
                                ui.ctx().copy_text(device.clone());
                                self.status = "设备 ID 已复制".to_owned();
                                ui.close_menu();
                            }
                        });
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
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.label(&self.status);
            });
            ui.separator();
            ui.small("关闭窗口会隐藏到右下角托盘；托盘菜单中的“关闭”才会退出 Connect。配置一次后可直接使用 ssh 昵称或 VS Code 连接。");
        });
        self.show_host_editor(context);
        context.request_repaint_after(Duration::from_millis(250));
    }
}

/// Add a system CJK font as a fallback while keeping egui's bundled Latin font first.
/// The release binaries stay small and use the fonts already installed by the OS.
pub fn install_cjk_font(context: &egui::Context) {
    let Some(path) = cjk_font_path() else {
        tracing::warn!("no system CJK font found; Chinese UI text may be unavailable");
        return;
    };
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "could not read system CJK font");
            return;
        }
    };

    let mut definitions = egui::FontDefinitions::default();
    definitions
        .font_data
        .insert(CJK_FONT_NAME.to_owned(), egui::FontData::from_owned(bytes));
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        if let Some(fonts) = definitions.families.get_mut(&family) {
            fonts.push(CJK_FONT_NAME.to_owned());
        }
    }
    context.set_fonts(definitions);
    tracing::debug!(path = %path.display(), "installed system CJK font fallback");
}

fn install_ui_style(context: &egui::Context) {
    let mut style = (*context.style()).clone();
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.visuals.window_fill = egui::Color32::from_rgb(25, 29, 36);
    style.visuals.panel_fill = egui::Color32::from_rgb(19, 23, 29);
    style.visuals.faint_bg_color = egui::Color32::from_rgb(35, 41, 50);
    context.set_style(style);
}

/// Generate the small rust-ssh mark used by the native window and taskbar.
///
/// Keeping this icon in code means source builds and Release builds use the
/// same icon without needing an additional runtime asset.
pub fn app_icon() -> egui::IconData {
    const SIZE: u32 = 64;
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];

    for y in 0..SIZE {
        for x in 0..SIZE {
            let inside = (12..52).contains(&x) && (8..56).contains(&y);
            let corner = [
                (x < 18 && y < 14, (18 - x) as i32, (14 - y) as i32),
                (x >= 46 && y < 14, (x - 45) as i32, (14 - y) as i32),
                (x < 18 && y >= 50, (18 - x) as i32, (y - 49) as i32),
                (x >= 46 && y >= 50, (x - 45) as i32, (y - 49) as i32),
            ];
            let rounded_corner = corner.iter().any(|(active, dx, dy)| {
                *active && dx.saturating_mul(*dx) + dy.saturating_mul(*dy) > 36
            });
            if inside && !rounded_corner {
                set_icon_pixel(&mut rgba, SIZE, x, y, [20, 32, 49, 255]);
            }
        }
    }

    draw_icon_line(
        &mut rgba,
        SIZE,
        (17.0, 16.0),
        (47.0, 16.0),
        [52, 207, 185, 255],
        2.5,
    );
    draw_icon_line(
        &mut rgba,
        SIZE,
        (47.0, 16.0),
        (47.0, 48.0),
        [52, 207, 185, 255],
        2.5,
    );
    draw_icon_line(
        &mut rgba,
        SIZE,
        (47.0, 48.0),
        (17.0, 48.0),
        [52, 207, 185, 255],
        2.5,
    );
    draw_icon_line(
        &mut rgba,
        SIZE,
        (17.0, 48.0),
        (17.0, 16.0),
        [52, 207, 185, 255],
        2.5,
    );
    draw_icon_line(
        &mut rgba,
        SIZE,
        (23.0, 27.0),
        (29.0, 32.0),
        [235, 248, 246, 255],
        3.5,
    );
    draw_icon_line(
        &mut rgba,
        SIZE,
        (29.0, 32.0),
        (23.0, 37.0),
        [235, 248, 246, 255],
        3.5,
    );
    draw_icon_line(
        &mut rgba,
        SIZE,
        (35.0, 39.0),
        (43.0, 39.0),
        [104, 225, 146, 255],
        3.5,
    );

    egui::IconData {
        rgba,
        width: SIZE,
        height: SIZE,
    }
}

fn set_icon_pixel(rgba: &mut [u8], size: u32, x: u32, y: u32, color: [u8; 4]) {
    let index = ((y * size + x) * 4) as usize;
    rgba[index..index + 4].copy_from_slice(&color);
}

fn draw_icon_line(
    rgba: &mut [u8],
    size: u32,
    start: (f32, f32),
    end: (f32, f32),
    color: [u8; 4],
    width: f32,
) {
    let min_x = start.0.min(end.0) - width;
    let max_x = start.0.max(end.0) + width;
    let min_y = start.1.min(end.1) - width;
    let max_y = start.1.max(end.1) + width;
    let denominator = (end.0 - start.0).powi(2) + (end.1 - start.1).powi(2);

    for y in 0..size {
        for x in 0..size {
            let point = (x as f32 + 0.5, y as f32 + 0.5);
            if point.0 < min_x || point.0 > max_x || point.1 < min_y || point.1 > max_y {
                continue;
            }
            let progress = if denominator == 0.0 {
                0.0
            } else {
                ((point.0 - start.0) * (end.0 - start.0) + (point.1 - start.1) * (end.1 - start.1))
                    / denominator
            };
            let progress = progress.clamp(0.0, 1.0);
            let nearest = (
                start.0 + progress * (end.0 - start.0),
                start.1 + progress * (end.1 - start.1),
            );
            let distance = (point.0 - nearest.0).powi(2) + (point.1 - nearest.1).powi(2);
            if distance <= (width / 2.0).powi(2) {
                set_icon_pixel(rgba, size, x, y, color);
            }
        }
    }
}

fn cjk_font_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let font_directory = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("Fonts");
        ["msyh.ttc", "simhei.ttf", "simsun.ttc", "Deng.ttf"]
            .into_iter()
            .map(|name| font_directory.join(name))
            .find(|path| path.is_file())
    }

    #[cfg(target_os = "macos")]
    {
        [
            "/System/Library/Fonts/PingFang.ttc",
            "/System/Library/Fonts/STHeiti Light.ttc",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
    }

    #[cfg(target_os = "linux")]
    {
        [
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.otf",
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
    }

    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

fn client_agent_config(settings: &ClientSettings) -> Result<agent::Config> {
    let pairing = bootstrap::decode(&settings.pairing_code)?;
    let device_id = required_value(&settings.device_id, "设备 ID")?;
    if !device_id::is_generated(&device_id) {
        return Err(anyhow!(
            "设备 ID 不是有效的 rust-ssh 自动生成 ID，请重新注册 client"
        ));
    }
    let pairing_device_id = pairing.device_id.ok_or_else(|| {
        anyhow!("设备配置码没有绑定设备 ID，请使用服务器 device add 生成的配置码")
    })?;
    if pairing_device_id != device_id {
        return Err(anyhow!(
            "设备配置码属于其他设备，请在服务器使用当前设备 ID 重新生成"
        ));
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
    migrate_legacy_config(name);
    let path = config_dir().join(name);
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn load_client_settings() -> ClientSettings {
    let mut settings: ClientSettings = load_settings("client.json");
    let mut changed = !config_dir().join("client.json").is_file();
    if settings.config_version != CLIENT_CONFIG_VERSION {
        settings.device_id = new_device_id();
        settings.pairing_code.clear();
        settings.config_version = CLIENT_CONFIG_VERSION;
        changed = true;
    }
    if settings.device_id.trim().is_empty() {
        settings.device_id = new_device_id();
        changed = true;
    }
    if changed {
        let _ = save_settings("client.json", &settings);
    }
    settings
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
    {
        if let Ok(executable) = std::env::current_exe() {
            if let Some(parent) = executable.parent() {
                return parent.join("data");
            }
        }

        if let Some(path) = std::env::var_os("APPDATA") {
            return PathBuf::from(path).join("rust-ssh");
        }
    }

    if let Some(path) = std::env::var_os("HOME") {
        return PathBuf::from(path).join(".config").join("rust-ssh");
    }
    PathBuf::from(".").join("rust-ssh")
}

#[cfg(windows)]
fn migrate_legacy_config(name: &str) {
    let directory = config_dir();
    let mut source_directories = Vec::new();
    if let Some(path) = std::env::var_os("LOCALAPPDATA") {
        let path = PathBuf::from(path);
        match name {
            "client.json" => {
                source_directories.push(path.join("rust-ssh-client").join("data"));
            }
            "connect.json" | "connect.setup" => {
                source_directories.push(path.join("rust-ssh-connect").join("data"));
            }
            _ => {}
        }
    }
    if let Some(path) = std::env::var_os("APPDATA") {
        source_directories.push(PathBuf::from(path).join("rust-ssh"));
    }

    for source_directory in source_directories {
        if source_directory == directory {
            continue;
        }
        let source = source_directory.join(name);
        let destination = directory.join(name);
        if !source.is_file() || destination.exists() {
            continue;
        }
        if let Err(error) = fs::create_dir_all(&directory) {
            tracing::warn!(%error, path = %directory.display(), "could not create Rust-SSH data directory");
            return;
        }
        match fs::copy(&source, &destination) {
            Ok(_) => {
                tracing::info!(file = name, path = %directory.display(), "migrated old Rust-SSH data file")
            }
            Err(error) => {
                tracing::warn!(%error, file = name, "could not migrate old Rust-SSH data file")
            }
        }
        break;
    }
}

#[cfg(not(windows))]
fn migrate_legacy_config(_name: &str) {}

fn new_device_id() -> String {
    device_id::generate().unwrap_or_else(|error| {
        tracing::error!(%error, "could not generate device ID");
        "rssh-device-uninitialized".to_owned()
    })
}

fn default_user() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "windows-user".to_owned())
}

fn valid_host_alias(alias: &str) -> bool {
    !alias.is_empty()
        && alias.len() <= 255
        && alias.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
}

const MANAGED_SSH_BEGIN: &str = "# >>> rust-ssh managed begin >>>";
const MANAGED_SSH_END: &str = "# <<< rust-ssh managed end <<<";

fn install_ssh_host(settings: &ConnectSettings, device_id: &str) -> Result<String> {
    if !device_id::is_valid(device_id) {
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
    let host = settings
        .host_aliases
        .get(device_id)
        .filter(|alias| valid_host_alias(alias))
        .cloned()
        .unwrap_or_else(|| device_id.to_owned());
    if !valid_host_alias(&host) {
        return Err(anyhow!("Host 昵称包含不支持的字符"));
    }
    let newline = if cfg!(windows) { "\r\n" } else { "\n" };

    let text = ssh_host_block(
        &host,
        device_id,
        &user,
        executable,
        setup_code_path,
        newline,
    );
    let path = user_ssh_config_path()?;
    update_managed_ssh_config(&path, &text, device_id, &host)?;
    Ok(host)
}

fn ssh_host_block(
    host: &str,
    device_id: &str,
    user: &str,
    executable: &str,
    setup_code_path: &str,
    newline: &str,
) -> String {
    [
        format!("Host {host}"),
        "\tHostName rust-ssh-proxy".to_owned(),
        format!("\tHostKeyAlias {device_id}"),
        format!("\tUser {user}"),
        format!(
            "\tProxyCommand {} --proxy --setup-code-file {} --target {}",
            shell_double_quote(executable),
            shell_double_quote(setup_code_path),
            shell_double_quote(device_id),
        ),
    ]
    .join(newline)
        + newline
}

fn user_ssh_config_path() -> Result<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| anyhow!("找不到用户目录，无法配置 SSH"))?;
    Ok(PathBuf::from(home).join(".ssh").join("config"))
}

fn update_managed_ssh_config(
    path: &Path,
    host_block: &str,
    device_id: &str,
    host: &str,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建 SSH 配置目录 {}", parent.display()))?;
    }
    let existing = if path.exists() {
        fs::read_to_string(path).with_context(|| format!("读取 SSH 配置 {}", path.display()))?
    } else {
        String::new()
    };
    let existing = normalize_line_endings(&existing);
    let host_block = normalize_line_endings(host_block).trim_end().to_owned();
    let device_marker = format!("HostKeyAlias {device_id}");
    let managed = if let Some(begin) = existing.find(MANAGED_SSH_BEGIN) {
        let after_begin = &existing[begin + MANAGED_SSH_BEGIN.len()..];
        let end_offset = after_begin
            .find(MANAGED_SSH_END)
            .ok_or_else(|| anyhow!("已有 rust-ssh SSH 配置块不完整"))?;
        let managed_body = &after_begin[..end_offset];
        let mut blocks = split_host_blocks(managed_body);
        blocks.retain(|block| {
            let same_host = block
                .lines()
                .next()
                .and_then(|line| line.strip_prefix("Host "))
                .map(str::trim)
                == Some(host);
            let same_device = block.lines().any(|line| line.trim() == device_marker);
            !same_host && !same_device
        });
        blocks.push(host_block.clone());
        format!(
            "{MANAGED_SSH_BEGIN}\n{}\n{MANAGED_SSH_END}\n",
            blocks.join("\n\n")
        )
    } else {
        format!("{MANAGED_SSH_BEGIN}\n{host_block}\n{MANAGED_SSH_END}\n")
    };
    let updated = if let Some(begin) = existing.find(MANAGED_SSH_BEGIN) {
        let after_begin = &existing[begin + MANAGED_SSH_BEGIN.len()..];
        let end_offset = after_begin
            .find(MANAGED_SSH_END)
            .ok_or_else(|| anyhow!("已有 rust-ssh SSH 配置块不完整"))?;
        let end = begin + MANAGED_SSH_BEGIN.len() + end_offset + MANAGED_SSH_END.len();
        format!("{}{}{}", &existing[..begin], managed, &existing[end..])
    } else {
        let mut updated = existing.trim_end().to_owned();
        if !updated.is_empty() {
            updated.push_str("\n\n");
        }
        updated.push_str(&managed);
        updated
    };
    let updated = if cfg!(windows) {
        updated.replace('\n', "\r\n")
    } else {
        updated
    };
    fs::write(path, updated).with_context(|| format!("写入 SSH 配置 {}", path.display()))?;
    set_private_permissions(path)
}

fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn split_host_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = Vec::new();
    for line in text.lines() {
        if line.starts_with("Host ") && !current.is_empty() {
            blocks.push(current.join("\n"));
            current.clear();
        }
        if line.trim().is_empty() && current.is_empty() {
            continue;
        }
        current.push(line.to_owned());
    }
    if !current.is_empty() {
        blocks.push(current.join("\n"));
    }
    blocks
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_alias_accepts_ssh_safe_names_only() {
        assert!(valid_host_alias("office-pc"));
        assert!(valid_host_alias("rssh_01.example"));
        assert!(!valid_host_alias("office pc"));
        assert!(!valid_host_alias("电脑"));
    }

    #[test]
    fn generated_ssh_host_block_uses_tabs_for_directives() {
        let block = ssh_host_block(
            "example-host",
            "rssh-device-a",
            "windows-user",
            r"C:\rust-ssh-connect.exe",
            r"C:\rust-ssh\data\connect.setup",
            "\n",
        );
        assert!(block.contains("\n\tHostName rust-ssh-proxy\n"));
        assert!(block.contains("\n\tUser windows-user\n"));
        assert!(!block.contains("\n    HostName"));
    }

    #[test]
    fn managed_ssh_config_keeps_multiple_devices_and_replaces_one_device() {
        let path = std::env::temp_dir().join(format!(
            "rust-ssh-ssh-config-test-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        let first = "Host rssh-device-a\n\tHostKeyAlias rssh-device-a\n";
        let second = "Host rssh-device-b\n\tHostKeyAlias rssh-device-b\n";

        update_managed_ssh_config(&path, first, "rssh-device-a", "rssh-device-a").unwrap();
        update_managed_ssh_config(&path, second, "rssh-device-b", "rssh-device-b").unwrap();
        let replacement = "Host renamed-device-a\n\tHostKeyAlias rssh-device-a\n";
        update_managed_ssh_config(&path, replacement, "rssh-device-a", "renamed-device-a").unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("Host renamed-device-a"));
        assert!(text.contains("Host rssh-device-b"));
        assert!(!text.contains("Host rssh-device-a"));
        assert_eq!(text.matches(MANAGED_SSH_BEGIN).count(), 1);
        assert_eq!(text.matches(MANAGED_SSH_END).count(), 1);
        let _ = fs::remove_file(path);
    }
}
