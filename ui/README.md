# rust-ssh 桌面前端

桌面前端是可选的原生 Rust/egui 应用，不需要 Node.js、Tauri 或额外运行时。运行 Release 二进制不需要安装 Rust；GUI 会使用系统中文字体作为 fallback。

## client

`rust-ssh-client` 面向 Windows x86-64 被控端：

- 第一次打开时随机生成 `rssh-...` 设备 ID，并在 UI 中显示；
- 点击“复制”把设备 ID交给服务器管理员；
- 服务器执行 `device add` 后，粘贴返回的设备配置码；
- 设备 ID 保存在 `%APPDATA%\rust-ssh\client.json`，与 Windows 计算机名无关；
- 本地 SSH 默认 `127.0.0.1:22`；
- 点击“启动”后在当前窗口进程内运行 agent；
- 关闭窗口即停止 client，不自动安装开机自启。

设备配置码必须绑定当前设备 ID，不能使用 controller 配置码，也不能手动把 ID 改成另一台设备的 ID。

## connect

`rust-ssh-connect` 面向 Windows x86-64 和 macOS ARM64 主控端：

- 粘贴由服务器 controller token 生成的配置码并填写 SSH 用户名；
- 自动刷新 relay 上的在线设备；
- 点击“配置 SSH”后自动维护用户 SSH 配置；
- 点击“连接选中设备”后打开系统 Terminal/cmd；
- 生成的 `rust-ssh-设备ID` 可供 Terminal 和 VS Code Remote-SSH 使用。

GUI 只负责配置和连接入口，真正的 SSH 认证和终端仍由系统 OpenSSH 提供。

## 配置码

先在服务器使用 client UI 显示的 ID 注册设备：

```bash
rust-ssh device add --device-id rssh-0123456789abcdef0123456789abcdef --server 198.51.100.10:24443 --server-key /etc/rust-ssh/identity.pub --devices-dir /etc/rust-ssh/devices
```

把命令输出的整行设备配置码只粘贴到对应的 client。connect 要另用 controller token 生成配置码：

```bash
rust-ssh pair-code --server 198.51.100.10:24443 --server-key /etc/rust-ssh/identity.pub --token-file /etc/rust-ssh/controller.token
```

配置码包含服务器 IP、server 公钥和 token，应当按秘密材料保存；server 私钥不会进入配置码。controller token 绝不能分发给 agent。

## 构建

```bash
cargo build --release --locked --features desktop --bin rust-ssh-client
cargo build --release --locked --features desktop --bin rust-ssh-connect
```

运行编译后的 exe/可执行文件不需要 Rust 或 Cargo。GUI 依赖通过 `desktop` feature 隔离，Ubuntu relay 默认使用无界面 `rust-ssh` 二进制。
