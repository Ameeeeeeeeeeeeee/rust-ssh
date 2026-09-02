# rust-ssh

面向个人使用的 SSH 中继工具，借鉴 RustDesk 的“设备主动连接服务器 + 服务器中继”模式。Windows client 和 Mac/Windows connect 都只需要主动访问 VPS，因此可以跨越不同 AP、AP 隔离和没有端口映射的网络。

普通用户部署请直接阅读：

- [三端部署手册（简洁版）](docs/deployment-quickstart.md)
- [三端部署手册（详细版）](docs/deployment-detailed.md)

## Architecture

```text
Windows client ──主动 Noise 连接──┐
                                  ├── Ubuntu VPS relay:24443
Mac/Windows connect ─主动连接─────┘
                                      │
                                      └──目标 client ──本机 127.0.0.1:22
```

- relay 可以同时管理多台 Windows client；
- 不同设备可以并行连接；
- 同一设备同时只允许一个 SSH 会话；
- client 默认只允许连接本机 loopback SSH，避免成为内网代理；
- 公网只需要开放 VPS 的一个 TCP 端口，默认 `24443`；
- client 和 connect 不监听公网端口；
- RustDesk 的 `hbbs/hbbr` 端口保持不变，不要复用。

传输层使用 `Noise_XX_25519_ChaChaPoly_SHA256`。不使用 X.509 证书，也不需要域名。

## Authentication model

relay 启动时读取：

```text
/etc/rust-ssh/identity.key                         # Noise 私钥，只留 relay
/etc/rust-ssh/identity.pub                         # Noise 公钥
/etc/rust-ssh/controller.token                     # controller 主控 token
/etc/rust-ssh/devices/<device_id>.token            # 每台设备一个 token
```

认证规则：

- `Role::Agent` 先按 `device_id` 查找 `<device_id>.token`，再比较 token；
- `Role::Controller` 只比较 `controller.token`；
- 不存在全局 agent token，也没有回退路径；
- 单个 agent token 泄露只能注册或伪装对应设备，不能 list 或连接其他设备；
- controller token 是 relay 的主控密钥，泄露后可 list 和连接所有设备，绝不能分发给 agent；
- 设备 token 文件在 relay 启动时加载，新增、删除或修改后重启 relay 生效。

Hello wire 格式、Noise、控制帧格式和 bridge 未改变。

## Pairing codes

`pair-code` 的格式仍是 `rssh1:<base64url-json>`，内容包含 relay IP、Noise 公钥和一个 endpoint token。

每台 client 使用自己的设备 token 生成一段配置码；connect 使用 controller token 生成另一段配置码。配置码是秘密材料，不要提交到 GitHub、issue 或公共聊天中。

```bash
rust-ssh pair-code \
  --server 203.0.113.10:24443 \
  --server-key /etc/rust-ssh/identity.pub \
  --token-file /etc/rust-ssh/devices/DESKTOP-KH8O1JM.token
```

## Relay configuration

`relay` 的 CLI/env 配置如下：

| CLI | Environment | Meaning |
| --- | --- | --- |
| `--listen` | `RUST_SSH_LISTEN` | relay 监听地址，默认 `0.0.0.0:24443` |
| `--identity-key` | `RUST_SSH_IDENTITY_KEY` | Noise 私钥路径 |
| `--controller-token-file` | `RUST_SSH_CONTROLLER_TOKEN_FILE` | controller token 文件路径 |
| `--devices-dir` | `RUST_SSH_DEVICES_DIR` | `<device_id>.token` 目录 |

示例：

```text
RUST_SSH_LISTEN=0.0.0.0:24443
RUST_SSH_IDENTITY_KEY=/etc/rust-ssh/identity.key
RUST_SSH_CONTROLLER_TOKEN_FILE=/etc/rust-ssh/controller.token
RUST_SSH_DEVICES_DIR=/etc/rust-ssh/devices
```

systemd 示例见 [`examples/rust-ssh-relay.service`](examples/rust-ssh-relay.service)，环境文件见 [`examples/rust-ssh-relay.env.example`](examples/rust-ssh-relay.env.example)。

## CLI surface

生成 server identity：

```bash
rust-ssh keygen \
  --identity-key /etc/rust-ssh/identity.key \
  --public-key /etc/rust-ssh/identity.pub
```

运行 relay：

```bash
rust-ssh relay \
  --listen 0.0.0.0:24443 \
  --identity-key /etc/rust-ssh/identity.key \
  --controller-token-file /etc/rust-ssh/controller.token \
  --devices-dir /etc/rust-ssh/devices
```

运行 agent 时，`--token-file` 是该 `--device-id` 对应的设备 token：

```powershell
rust-ssh.exe agent `
  --server 203.0.113.10:24443 `
  --server-key C:\ProgramData\rust-ssh\server-identity.pub `
  --token-file C:\ProgramData\rust-ssh\DESKTOP-KH8O1JM.token `
  --device-id DESKTOP-KH8O1JM `
  --target 127.0.0.1:22
```

运行 `list` 或 `controller` 时，`--token-file` 是 controller token：

```bash
rust-ssh list \
  --server 203.0.113.10:24443 \
  --server-key ~/.config/rust-ssh/server-identity.pub \
  --token-file ~/.config/rust-ssh/controller.token
```

## Build

源码构建需要 Rust stable、Cargo 和 Git；运行 release 二进制不需要 Rust。

```bash
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets --all-features -- -D warnings
```

构建 relay：

```bash
cargo build --release --locked --bin rust-ssh
```

构建桌面端：

```bash
cargo build --release --locked --features desktop --bin rust-ssh-client
cargo build --release --locked --features desktop --bin rust-ssh-connect
```

`desktop` feature 隔离 egui 依赖；Ubuntu relay 默认使用无界面的 `rust-ssh` binary。

## Release

向 Git push `v*` 标签会触发 [`.github/workflows/release.yml`](.github/workflows/release.yml)，构建并上传：

```text
rust-ssh-relay-linux-x86_64
rust-ssh-agent-windows-x86_64.exe
rust-ssh-client-windows-x86_64.exe
rust-ssh-connect-windows-x86_64.exe
rust-ssh-connect-macos-aarch64
```

编译文件不会进入 Git 源码仓库，`target/` 已被 `.gitignore` 忽略。

## Security scope

公网端口仍可能被扫描，部署时应使用云安全组和 VPS 防火墙。当前实现包含 Noise server key pinning、握手超时、连接数限制、帧长度限制、设备 ID 校验、per-device token 和单设备单会话限制。

当前明确不包含：IP 限速、失败封禁、ACL、token 热吊销、审计、BitLocker 和证书体系。
