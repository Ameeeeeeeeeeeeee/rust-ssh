# rust-ssh

一个单仓库的 SSH 中继项目，借鉴 RustDesk 的“设备主动注册 + 中继转发”思路：Windows 被控端和 Mac 主控端都主动连接 VPS，因此两端即使处于不同 AP、开启 AP 隔离或没有端口映射，也可以建立 SSH。

项目由三类程序组成：

- `rust-ssh`：无界面 CLI。Ubuntu 上运行 `relay`；Windows 上也可以运行 `agent` 作为持久化被控服务；
- `rust-ssh-client`：Windows x86-64 极简被控端 GUI，内置 agent；
- `rust-ssh-connect`：Windows x86-64 / macOS ARM64 极简主控端 GUI，列出在线设备并打开系统 SSH 终端。

GUI 是可选构建目标，relay 仍是无界面持久化服务。运行已经编译好的程序不需要安装 Rust 或 Cargo。

## 设计结论

本项目不使用 X.509 证书，也不需要域名。三端使用 VPS 的公网 IP，例如 `203.0.113.10:24443`。

传输层使用标准 Noise 协议 `Noise_XX_25519_ChaChaPoly_SHA256`：

- VPS 生成一把长期 Noise 静态私钥 `identity.key`，只留在 VPS；
- 同时生成对应的十六进制公钥 `identity.pub`，复制给 client 和 connect；
- client/connect 在握手时固定校验这把公钥，确认连接到的是你的 relay；
- 每次连接仍使用临时会话密钥，控制帧、token 和 SSH 字节流都在 Noise 加密层内传输；
- token 是第二道共享授权，建议放在权限受限的文件中，而不是命令行参数中。

这里的 server key 是 Noise 公钥，不是 TLS 证书。它不需要证书签发机构，也不需要域名。SSH 本身仍由 Mac 的 OpenSSH 和 Windows 的 `sshd` 做端到端加密与主机密钥校验；relay 只转发加密后的 SSH 字节流。

## 架构与多设备

```text
Windows client A ──主动 TCP + Noise──┐
Windows client B ──主动 TCP + Noise──┼── VPS rust-ssh relay:24443
                                     │
Mac/Windows connect ─主动 TCP + Noise┘
        │
        └─选择 device ID → relay 配对对应 client → client 连接本机 127.0.0.1:22
```

同一个 relay 可以注册多个 client。connect 的设备列表来自 relay：

- 不同设备可以同时建立 SSH 会话；
- 同一设备同时只允许一个活动会话；
- 列表显示当前已注册且空闲的设备，正在使用中的设备暂时不会被列为可连接；
- client 默认只连接本机 loopback 的 `127.0.0.1:22`，agent 会拒绝非 loopback 目标，避免变成内网转发器。

| 角色 | 运行位置 | 实际程序 | 作用 | 入站端口 |
| --- | --- | --- | --- | --- |
| relay/server | Ubuntu VPS | `rust-ssh relay` | 注册 client、按设备 ID 配对并转发字节 | 只监听一个 TCP 端口，默认 `24443` |
| client/agent | Windows x86-64 | `rust-ssh-client.exe` 或 `rust-ssh agent` | 主动连 VPS，收到请求后连本机 `127.0.0.1:22` | 不需要公网入站 |
| connect/controller | Windows x86-64 / macOS ARM64 | `rust-ssh-connect` 或 `rust-ssh controller` | 主动连 VPS，列设备并为 OpenSSH 提供代理流 | 不需要公网入站 |

VPS 上已有的 RustDesk `hbbs/hbbr` 与本项目是两套独立服务。`rust-ssh` 默认使用 `24443/tcp`，只要该端口未被占用就不会冲突，不要复用 RustDesk 的端口。

## 目录

```text
src/main.rs                    CLI：keygen / relay / agent / list / controller
src/lib.rs                     共享库模块
src/server.rs                  relay 中继和多设备配对
src/agent.rs                   Windows 被控 agent
src/connect.rs                 controller、设备列表和 SSH ProxyCommand 流
src/desktop.rs                 可选 egui client/connect GUI
src/bin/rust-ssh-client.rs     Windows client GUI 入口
src/bin/rust-ssh-connect.rs    Windows/macOS connect GUI + 内部 proxy 入口
src/noise.rs                   Noise 握手、server key 固定和加密字节流
src/identity.rs                relay 静态密钥生成与读取
src/protocol.rs                控制帧协议和版本号
src/bridge.rs                  双向字节转发
examples/                      systemd、环境变量和 SSH 配置示例
ui/README.md                   GUI 使用和构建说明
```

## 构建与运行时依赖

源码构建需要 Rust stable、Cargo 和 Git；项目声明的最低 Rust 版本是 `1.82`。当前 Windows 若已安装 rustup，常见路径是：

```text
C:\Users\<用户名>\.cargo\bin\rustc.exe
C:\Users\<用户名>\.cargo\bin\cargo.exe
```

实际路径用下面命令查看：

```powershell
where.exe rustc
where.exe cargo
rustc --version
cargo --version
```

基础检查：

```bash
cargo fmt --all -- --check
cargo test --all-targets --locked
cargo build --release --locked --bin rust-ssh
```

构建 GUI：

```bash
cargo build --release --locked --features desktop --bin rust-ssh-client
cargo build --release --locked --features desktop --bin rust-ssh-connect
```

产物位置：

- relay/CLI：`target/release/rust-ssh`，Windows 上为 `target/release/rust-ssh.exe`；
- Windows client：`target/release/rust-ssh-client.exe`；
- Windows connect：`target/release/rust-ssh-connect.exe`；
- macOS ARM64 connect：`target/release/rust-ssh-connect`。

编译完成后，目标机器只需要对应的可执行文件、OpenSSH 和配置材料，不需要 Rust、Cargo、源码或 Node.js。只有从源码重新编译、升级版本或自己打包时才需要 Rust。

GitHub Actions 的 release 会生成以下资产：

```text
rust-ssh-relay-linux-x86_64
rust-ssh-agent-windows-x86_64.exe
rust-ssh-client-windows-x86_64.exe
rust-ssh-connect-windows-x86_64.exe
rust-ssh-connect-macos-aarch64
```

## 三端共享材料

只把下面两样材料分发给 client 和 connect：

1. VPS 的 `identity.pub`：非秘密，用于固定 relay 身份；
2. relay token：秘密，三端内容必须完全一致。

绝不能把 VPS 私钥 `identity.key` 复制到 Windows、Mac 或 GitHub。

### 生成 server key 和 token（VPS）

先登录你现有的 VPS：

```bash
ssh Volc-Engine-Test
```

安装 relay 后生成 key。下面的 `identity.key` 是原始 32 字节私钥，`identity.pub` 是可复制的 64 字符十六进制公钥：

```bash
sudo install -d -m 0750 /etc/rust-ssh
sudo /usr/local/bin/rust-ssh keygen \
  --identity-key /etc/rust-ssh/identity.key \
  --public-key /etc/rust-ssh/identity.pub
```

命令只允许创建新文件，不会覆盖已有私钥。token 至少需要 32 个非空白字节：

```bash
openssl rand -hex 32 | tr -d '\n' | sudo tee /etc/rust-ssh/token >/dev/null
```

把 `identity.pub` 和 token 安全地复制到 Windows client、Windows connect 和 Mac connect。实际复制时使用安全渠道，不要提交到 Git。

## 1. Ubuntu VPS：部署持久化 relay/server

### 选择安装方式

推荐直接下载 GitHub Release 的 `rust-ssh-relay-linux-x86_64`。VPS 不需要安装 Rust。

如果要从源码构建：

```bash
git clone <你的 GitHub 仓库地址> /opt/rust-ssh-src
cd /opt/rust-ssh-src
cargo build --release --locked --bin rust-ssh
sudo install -m 0755 target/release/rust-ssh /usr/local/bin/rust-ssh
```

### 创建服务账号、材料和配置

```bash
sudo useradd --system --no-create-home --shell /usr/sbin/nologin rustssh 2>/dev/null || true
sudo install -d -o root -g rustssh -m 0750 /etc/rust-ssh
sudo /usr/local/bin/rust-ssh keygen \
  --identity-key /etc/rust-ssh/identity.key \
  --public-key /etc/rust-ssh/identity.pub
sudo chown root:rustssh /etc/rust-ssh/identity.key /etc/rust-ssh/identity.pub
sudo chmod 0640 /etc/rust-ssh/identity.key
sudo chmod 0644 /etc/rust-ssh/identity.pub
sudo chown root:rustssh /etc/rust-ssh/token
sudo chmod 0640 /etc/rust-ssh/token
```

如果 key 或 token 已经存在，跳过对应的生成命令，只执行正确的 `chown/chmod`。`identity.key` 必须只能被 relay 服务账号所属组读取。

创建 `/etc/rust-ssh/relay.env`：

```ini
RUST_SSH_LISTEN=0.0.0.0:24443
RUST_SSH_IDENTITY_KEY=/etc/rust-ssh/identity.key
RUST_SSH_TOKEN_FILE=/etc/rust-ssh/token
```

安装 systemd 服务：

```bash
sudo install -m 0644 examples/rust-ssh-relay.service /etc/systemd/system/rust-ssh-relay.service
sudo systemctl daemon-reload
sudo systemctl enable --now rust-ssh-relay
sudo systemctl status rust-ssh-relay
journalctl -u rust-ssh-relay -f
```

确认端口没有冲突，并在云安全组和 VPS 防火墙放行同一个 TCP 端口：

```bash
sudo ss -lntup | grep ':24443' || true
```

公网只放行 `24443/tcp`。如果主控/被控的出口 IP 固定，最好把规则收窄为这些来源 IP；如果 IP 不固定，至少不要开放 Windows 的 22 端口到公网。

## 2. Windows x86-64：部署 client/被控端

Windows 必须先有本机 OpenSSH Server，因为 rust-ssh 只负责把链路送到本机 SSH，不替代 `sshd`：

```powershell
Get-Service sshd
Test-NetConnection 127.0.0.1 -Port 22
```

### 方式 A：使用 client GUI exe

从 GitHub Release 下载 `rust-ssh-client-windows-x86_64.exe`，例如放到：

```powershell
$dir = 'C:\ProgramData\rust-ssh'
New-Item -ItemType Directory -Force $dir | Out-Null
Copy-Item .\rust-ssh-client-windows-x86_64.exe "$dir\rust-ssh-client.exe"
Copy-Item .\identity.pub "$dir\server-identity.pub"
Set-Content -NoNewline -Path "$dir\token.txt" -Value '替换为与 VPS 完全相同的 token'
icacls "$dir\token.txt" /inheritance:r /grant:r 'SYSTEM:F' 'Administrators:F'
```

打开 `rust-ssh-client.exe`，填写并保存：

- `Relay`：VPS IP 和端口，例如 `203.0.113.10:24443`；
- `Server 公钥`：`C:\ProgramData\rust-ssh\server-identity.pub`；
- `Token 文件`：`C:\ProgramData\rust-ssh\token.txt`；
- `设备 ID`：每台 Windows 唯一，例如 `DESKTOP-KH8O1JM`；只用字母、数字、`.`、`_`、`-`；
- `本地 SSH`：保持 `127.0.0.1:22`。

点击“启动 agent”后，client 会在窗口进程内主动连接 relay。关闭窗口会停止 agent；此方式适合先验证配置。

GUI 配置会保存到 `%APPDATA%\\rust-ssh\\client.json`，不保存 token 内容，只保存 token 文件路径。

### 方式 B：源码构建或无界面 exe

源码构建 GUI：

```powershell
git clone <你的 GitHub 仓库地址>
cd rust-ssh
cargo build --release --locked --features desktop --bin rust-ssh-client
```

使用 `target\\release\\rust-ssh-client.exe`，配置方式与方式 A 相同。

如果要做真正的开机持久化，推荐使用 GitHub Release 的 `rust-ssh-agent-windows-x86_64.exe`，或源码构建默认 CLI 后得到的 `target\\release\\rust-ssh.exe`。前台验证：

```powershell
$dir = 'C:\ProgramData\rust-ssh'
& "$dir\rust-ssh-agent-windows-x86_64.exe" agent `
  --server 203.0.113.10:24443 `
  --server-key "$dir\server-identity.pub" `
  --token-file "$dir\token.txt" `
  --device-id DESKTOP-KH8O1JM `
  --target 127.0.0.1:22
```

再用管理员 PowerShell 注册开机任务：

```powershell
$dir = 'C:\ProgramData\rust-ssh'
$binary = "$dir\rust-ssh-agent-windows-x86_64.exe"
$args = 'agent --server 203.0.113.10:24443 --server-key C:\ProgramData\rust-ssh\server-identity.pub --token-file C:\ProgramData\rust-ssh\token.txt --device-id DESKTOP-KH8O1JM --target 127.0.0.1:22'
$action = New-ScheduledTaskAction -Execute $binary -Argument $args -WorkingDirectory $dir
$trigger = New-ScheduledTaskTrigger -AtStartup
$principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
Register-ScheduledTask -TaskName 'rust-ssh-agent' -Action $action -Trigger $trigger -Principal $principal -Force
Start-ScheduledTask -TaskName 'rust-ssh-agent'
```

查看或移除：

```powershell
Get-ScheduledTask -TaskName 'rust-ssh-agent'
Unregister-ScheduledTask -TaskName 'rust-ssh-agent' -Confirm:$false
```

agent 会自动重连 relay。Windows 不需要为 agent 新开入站防火墙规则；`sshd` 仍可按原来的策略只允许本机或受信任网络。

## 3. Mac ARM64 或 Windows x86-64：部署 connect/主控端

connect 端需要系统 OpenSSH：

```bash
ssh -V
```

macOS 使用系统自带的 `/usr/bin/ssh`；Windows 需要安装并能在 PATH 中运行 OpenSSH Client。

### 方式 A：使用 connect GUI

下载对应 release：

- Windows：`rust-ssh-connect-windows-x86_64.exe`；
- Mac：`rust-ssh-connect-macos-aarch64`。

打开后填写：

- `Relay`：VPS IP 和端口；
- `Server 公钥`：本地的 `identity.pub` 副本；
- `Token 文件`：与 relay 一致的 token 文件；
- `SSH 用户`：Windows 上的 OpenSSH 用户名。

GUI 会自动刷新设备列表。选择设备后点击“连接选中设备”，程序会生成临时 SSH 配置并打开系统终端：Windows 使用 `cmd.exe`，macOS 使用 Terminal。终端里的认证、SSH 公钥和 Windows host key 校验仍由标准 OpenSSH 负责。

connect GUI 配置位置：

- Windows：`%APPDATA%\\rust-ssh\\connect.json`；
- macOS：`~/.config/rust-ssh/connect.json`。

### 方式 B：源码构建或 CLI/SSH config

Windows x86-64：

```powershell
cargo build --release --locked --features desktop --bin rust-ssh-connect
```

macOS ARM64：

```bash
cargo build --release --locked --features desktop --bin rust-ssh-connect
```

如果使用无界面 CLI，可以构建默认的 `rust-ssh`，然后使用 `controller` 子命令。先列出在线设备：

```bash
rust-ssh list \
  --server 203.0.113.10:24443 \
  --server-key ~/.config/rust-ssh/server-identity.pub \
  --token-file ~/.config/rust-ssh/token
```

手动测试连接：

```bash
rust-ssh controller \
  --server 203.0.113.10:24443 \
  --server-key ~/.config/rust-ssh/server-identity.pub \
  --token-file ~/.config/rust-ssh/token \
  --target DESKTOP-KH8O1JM
```

正常使用建议交给 OpenSSH 的 `ProxyCommand`：

```sshconfig
Host windows-main
    HostName rust-ssh-proxy-placeholder
    User ame
    ProxyCommand /usr/local/bin/rust-ssh controller --server 203.0.113.10:24443 --server-key /Users/you/.config/rust-ssh/server-identity.pub --token-file /Users/you/.config/rust-ssh/token --target DESKTOP-KH8O1JM
```

之后：

```bash
ssh windows-main
```

`HostName` 不会被真正连接；`ProxyCommand` 才负责通过 VPS 找到目标 client。connect GUI 内部也采用同样机制，只是自动生成配置并启动系统 SSH。

## 谁向谁暴露什么

| 端点 | 监听什么 | 主动连接什么 | 需要携带的材料 |
| --- | --- | --- | --- |
| Ubuntu relay | `0.0.0.0:24443/tcp` | 不主动连接 client/connect | `identity.key` 私钥、token |
| Windows client | 不监听新公网端口 | VPS 的 `IP:24443`；本机 `127.0.0.1:22` | relay `identity.pub`、token、唯一设备 ID |
| Mac/Windows connect | 不监听新公网端口 | VPS 的 `IP:24443`；本地启动 `ssh` | relay `identity.pub`、token、目标设备 ID |

因此公网只需要暴露 VPS 的一个 relay TCP 端口。Windows 的 22 端口不需要暴露到公网，Mac 也不需要暴露端口。`Volc-Engine-Test` 只是你登录 VPS 的 SSH 别名，不是 rust-ssh relay 地址；relay 配置里使用 VPS 的公网 IP。

## 端口冲突与防护

一个公网端口并不等于不会被攻击。RustDesk 的公开服务端口同样会被扫描、探测和尝试连接；server key、认证和防火墙共同降低风险，不能让端口从网络上消失。

当前 rust-ssh relay 已有：

- Noise 加密握手，client/connect 固定校验 relay 公钥；
- token 只在加密握手后发送，服务端做常量时间比较；
- Noise 握手 10 秒超时；
- 同时最多 128 个 TCP/Noise 连接；
- 控制帧和加密数据帧都有长度上限；
- 每个设备同一时间只允许一个 SSH 会话，配对等待 15 秒超时；
- 设备 ID 只允许安全字符；
- systemd 示例使用非 root 的 `rustssh` 账号和基础沙箱选项。

这些措施不能抵挡大型带宽洪泛或全部资源耗尽攻击。长期运行时建议：

1. 云安全组只开放 `24443/tcp`，SSH 管理端口使用另一套规则；
2. 如果出口 IP 固定，relay 只允许主控/被控出口 IP；
3. 定期轮换 token；`identity.key` 泄露时重新生成 key，并把新的 `identity.pub` 分发到所有 client/connect；
4. 不提交 token、`identity.key`、个人 SSH 配置或其他凭据。

当前版本还没有按 IP 限速、失败封禁、每设备独立 token、controller ACL、吊销列表和完整审计日志；如果要把它公开给多人使用，应先补这些能力。

## 排错

- VPS 日志：`journalctl -u rust-ssh-relay -f`；
- Windows 端口测试：`Test-NetConnection 203.0.113.10 -Port 24443`；
- Mac 端口测试：`nc -vz 203.0.113.10 24443`；
- `device is offline`：检查 client 是否运行、token 是否一致、设备 ID 是否一致；
- `public key does not match`：检查 `server-identity.pub` 是否来自当前 VPS 的 `identity.pub`；
- 已到达 Windows SSH 认证但登录失败：中继链路已经打通，继续检查 Windows 用户名、密码、公钥或 `sshd` 配置；
- GUI 列表为空：先看 client 的 agent 状态，再从 connect 使用 `rust-ssh list` 验证 relay 返回结果。

## 发布到 GitHub

这是一个单仓库，relay、CLI agent、GUI client、CLI controller 和 GUI connect 都在这里。当前仓库需要你自己绑定 GitHub 远程仓库：

```bash
git add .
git commit -m "Initial rust-ssh relay and desktop clients"
git branch -M main
git remote add origin git@github.com:<你的用户名>/rust-ssh.git
git push -u origin main
```

推送 `v*` 标签会触发 `.github/workflows/release.yml`，自动构建 Ubuntu relay、Windows agent/client/connect 和 macOS ARM64 connect。不要提交真实 token、`identity.key`、个人 SSH 配置或任何本地凭据；`.gitignore` 已忽略常见密钥和 token 文件。
