# Rust-SSH

Rust-SSH 是面向个人使用的 SSH 中继工具，借鉴 RustDesk 的“设备主动连接服务器 + 服务器中继”模式。Windows Rust-SSH-Client 和 Mac/Windows Rust-SSH-Connect 都只主动访问服务器，因此可以跨越不同 AP、AP 隔离和没有端口映射的网络。

普通用户部署请直接阅读：

- [三端部署手册（简洁版）](docs/deployment-quickstart.md)
- [三端部署手册（详细版）](docs/deployment-detailed.md)

## Architecture

```text
Windows client ──主动 Noise 连接──┐
                                   ├── Ubuntu 服务器 rust-ssh-server:24443
Mac/Windows connect ─主动连接─────┘
                                      │
                                      └──目标 client ──本机 127.0.0.1:22
```

- relay 可以同时管理多台 Windows client；
- 不同设备可以并行连接；
- 同一设备可以同时承载多个 SSH 会话；
- client 只允许连接本机 loopback SSH，避免成为内网代理；
- 公网只需要开放服务器的一个 TCP 端口，默认 `24443`；
- client 和 connect 不监听公网端口；
- RustDesk 的 `hbbs/hbbr` 端口保持不变，不能与 rust-ssh-server 复用同一个端口。

传输层使用 `Noise_XX_25519_ChaChaPoly_SHA256`。不使用 X.509 证书，也不需要域名；配置码中的 server key 用于确认连接的是正确的服务器。

## Authentication model

relay 启动时读取：

```text
/etc/rust-ssh-server/identity.key                  # Noise 私钥，只留服务器
/etc/rust-ssh-server/identity.pub                  # Noise 公钥
/etc/rust-ssh-server/controller.token              # controller 主控 token
/etc/rust-ssh-server/devices/<device_id>.token     # 每台设备一个 token
```

认证规则：

- `Role::Agent` 和 `Role::AgentSession` 都先按 `device_id` 查找对应的 `<device_id>.token`，再比较 token；
- `Role::Controller` 只比较 `controller.token`；
- 不存在全局 agent token，也没有回退路径；
- 单个 agent token 泄露只能注册或伪装对应设备，不能 list 或连接其他设备；
- controller token 是服务器的主控密钥，泄露后可 list 和连接所有设备，绝不能分发给 client；
- 设备 token 和 controller token 在 relay 启动时加载，并每约 2 秒自动热加载；新增、删除或修改文件通常不需要重启 rust-ssh-server；
- 如果热加载时发现文件暂时不存在或内容无效，relay 会保留上一份有效配置，避免半写入文件导致服务失效；已建立的连接不会被强制断开；
- v0.4 起，client 首次启动时生成 `rssh-` 开头的随机设备 ID，并保存在本机配置中；它与 Windows 计算机名无关，也不会因修改计算机名而改变。

Noise 和现有配置码格式保持不变；v0.5.0 包含 agent session 通道，让一个 client 的控制连接和多个 SSH 数据连接分离，并让 Client 在网络或服务器暂时不可用时持续自动重连。v0.5.1 修复 Windows GUI 退出和 MSI 覆盖升级时旧进程残留的问题。升级 v0.5.x 时，server、client、connect 三端需要一起升级；旧版程序不能与新协议互通。

## Pairing codes

`pair-code` 的格式是 `rssh1:<base64url-json>`，内容包含服务器 IP、Noise 公钥和 endpoint token。设备配置码还包含设备 ID。

v0.4 推荐流程是：

1. Windows client UI 显示并允许复制本机随机设备 ID；
2. 服务器执行 `rust-ssh-server device add`，为该 ID 生成 token 文件和设备配置码；
3. 把设备配置码粘贴回对应 client；
4. 服务器用 `controller.token` 生成另一份主控配置码，交给 connect。

配置码是秘密材料，不要提交到 GitHub、issue 或公共聊天中。

```bash
rust-ssh-server device add --device-id rssh-0123456789abcdef0123456789abcdef --server 198.51.100.10:24443 --server-key /etc/rust-ssh-server/identity.pub --devices-dir /etc/rust-ssh-server/devices
```

## rust-ssh-server configuration

服务器端 `rust-ssh-server` 的 CLI/env 配置如下；服务器子命令仍然是 `relay`，因此可以执行 `rust-ssh-server relay`：

| CLI | Environment | Meaning |
| --- | --- | --- |
| `--listen` | `RUST_SSH_LISTEN` | relay 监听地址，默认 `0.0.0.0:24443` |
| `--identity-key` | `RUST_SSH_IDENTITY_KEY` | Noise 私钥路径 |
| `--controller-token-file` | `RUST_SSH_CONTROLLER_TOKEN_FILE` | controller token 文件路径 |
| `--devices-dir` | `RUST_SSH_DEVICES_DIR` | `<device_id>.token` 目录 |

示例：

```text
RUST_SSH_LISTEN=0.0.0.0:24443
RUST_SSH_IDENTITY_KEY=/etc/rust-ssh-server/identity.key
RUST_SSH_CONTROLLER_TOKEN_FILE=/etc/rust-ssh-server/controller.token
RUST_SSH_DEVICES_DIR=/etc/rust-ssh-server/devices
```

systemd 示例见 [`examples/rust-ssh-server.service`](examples/rust-ssh-server.service)，环境文件见 [`examples/rust-ssh-server.env.example`](examples/rust-ssh-server.env.example)。升级时只替换程序和 systemd 服务，不要改动 `/etc/rust-ssh-server` 下的 identity、token 和设备目录。

## CLI surface

生成服务器 identity：

```bash
rust-ssh-server keygen --identity-key /etc/rust-ssh-server/identity.key --public-key /etc/rust-ssh-server/identity.pub
```

注册一个已在 client UI 中显示的设备：

```bash
rust-ssh-server device add --device-id rssh-0123456789abcdef0123456789abcdef --server 198.51.100.10:24443 --server-key /etc/rust-ssh-server/identity.pub --devices-dir /etc/rust-ssh-server/devices
```

运行 rust-ssh-server：

```bash
rust-ssh-server relay --listen 0.0.0.0:24443 --identity-key /etc/rust-ssh-server/identity.key --controller-token-file /etc/rust-ssh-server/controller.token --devices-dir /etc/rust-ssh-server/devices
```

运行 agent 时，`--token-file` 是该 `--device-id` 对应的设备 token；GUI client 会自动从设备配置码读取它：

```powershell
rust-ssh-server.exe agent --server 198.51.100.10:24443 --server-key C:\ProgramData\rust-ssh-server\server-identity.pub --token-file C:\ProgramData\rust-ssh-server\device.token --device-id rssh-0123456789abcdef0123456789abcdef --target 127.0.0.1:22
```

运行 `list` 或 `controller` 时，`--token-file` 是 controller token：

```bash
rust-ssh-server list --server 198.51.100.10:24443 --server-key ~/.config/rust-ssh/server-identity.pub --token-file ~/.config/rust-ssh/controller.token
```

查看服务器上实际保存的 controller/device 配置（默认隐藏 token 内容）：

```bash
rust-ssh-server inventory --controller-token-file /etc/rust-ssh-server/controller.token --devices-dir /etc/rust-ssh-server/devices
```

只有在服务器本地可信终端上排查时，才追加 `--show-tokens`。connect 实例不会单独登记；它们共享 controller token，因此 inventory 能显示 token 文件和设备注册情况，但不能知道有多少台 connect 保存过配置码。

## Build

源码构建需要 Rust stable、Cargo 和 Git；运行 Release 二进制不需要 Rust。

```bash
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets --all-features -- -D warnings
```

构建 relay：

```bash
cargo build --release --locked --bin rust-ssh-server
```

构建桌面端：

```bash
cargo build --release --locked --features desktop --bin rust-ssh-client
cargo build --release --locked --features desktop --bin rust-ssh-connect
```

`desktop` feature 隔离 egui 依赖；Ubuntu 服务器使用无界面的 `rust-ssh-server` binary。新 Release 不再发布旧的 `rust-ssh` 服务器程序。

## Release

向 Git push `v*` 标签会触发 [`.github/workflows/release.yml`](.github/workflows/release.yml)，构建并上传：

```text
rust-ssh-server-linux-x86_64
rust-ssh-server.service
Rust-SSH-Client-windows-x86_64.msi
Rust-SSH-Connect-windows-x86_64.msi
Rust-SSH-Connect-macos-aarch64
```

Windows Release 只提供 MSI 安装包，不单独提供便携版 `.exe`；MSI 内部包含 Rust-SSH-Client 或 Rust-SSH-Connect，需要管理员权限并默认安装到 `C:\Program Files` 下，配置放在安装目录的 `data` 文件夹。Client 和 Connect 的 GUI 都会常驻 Windows 通知区域，关闭窗口只隐藏，托盘菜单中的“关闭”才会退出；每个程序同时只运行一个 GUI 实例。MSI 支持覆盖安装新版本，升级时会先关闭旧实例。Windows agent 的命令行程序仍可从源码编译，但不会作为 Release 附件发布。编译文件不会进入 Git 源码仓库，`target/` 已被 `.gitignore` 忽略。

## Security scope

公网端口仍可能被扫描，部署时应使用云安全组和服务器防火墙。当前实现包含 Noise server key pinning、握手超时、连接数限制、帧长度限制、设备 ID 校验、per-device token、未知设备拒绝和单设备多会话的待处理数量限制。

当前明确不包含：IP 限速、失败封禁、细粒度 ACL、审计、BitLocker、证书体系和自动下载新版本。修改 token 文件后的新认证会热加载；已经建立的会话不会被强制断开。

Windows MSI 默认安装到 `C:\Program Files`；为了让普通用户保存配置，安装目录中的 `data` 文件夹对本机 Users 组可写。单用户电脑适合此布局，多用户电脑应额外保护其中的配置码。
