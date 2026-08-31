# rust-ssh 桌面前端

桌面前端已经实现为可选的原生 Rust/egui 界面，不需要 Node.js、Tauri 或单独的前端运行时。

## 两个程序

- `rust-ssh-client`：Windows x86-64 被控端。界面负责保存配置、启动/停止 agent，并显示运行状态；agent 在同一个进程内运行。
- `rust-ssh-connect`：Windows x86-64 和 macOS ARM64 主控端。界面定时读取 relay 的在线设备列表，选择设备后生成临时 SSH 配置并打开系统终端；真正的终端仍由系统 OpenSSH 提供。

## 构建

```bash
cargo build --release --locked --features desktop --bin rust-ssh-client
cargo build --release --locked --features desktop --bin rust-ssh-connect
```

GUI 依赖通过 `desktop` feature 隔离。Ubuntu relay 默认构建无界面二进制：

```bash
cargo build --release --locked --bin rust-ssh
```

编译完成后，运行这些 exe/可执行文件不需要安装 Rust 或 Cargo。源码开发、升级版本或重新打包时才需要 Rust 工具链。

## UI 配置

首次打开程序时填写：

- Relay：VPS 的公网 IP 和端口，例如 `203.0.113.10:24443`；
- Server 公钥：VPS 生成的 `identity.pub` 副本；
- Token 文件：与 relay 完全一致的 token 文件；
- client 额外填写设备 ID 和本机 SSH 目标，默认 `127.0.0.1:22`；
- connect 额外填写 Windows OpenSSH 用户名。

配置保存位置：

- Windows：`%APPDATA%\\rust-ssh\\client.json` 或 `connect.json`；
- macOS：`~/.config/rust-ssh/client.json` 或 `connect.json`。

connect 的自动刷新只显示当前空闲且已注册的 agent。不同设备可以并行建立会话；同一设备同时只允许一个会话。点击“连接选中设备”后，程序会打开系统 SSH 终端，不在 GUI 内嵌终端窗口。

## 后续增强方向

目前界面刻意保持极简，核心链路和无界面 CLI 不受影响。后续可以在不改变 relay 协议的前提下增加托盘、开机启动、日志查看、设备别名、断线重连提示和内嵌终端。
