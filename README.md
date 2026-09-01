# rust-ssh

面向个人使用的 SSH 中继工具，借鉴 RustDesk 的“设备主动连接服务器 + 服务器中继”的方式。Windows client 和 Mac/Windows connect 都只需要主动访问 VPS，因此即使两台设备处于不同 AP、开启 AP 隔离或没有端口映射，也可以通过 SSH 互通。

## 你实际需要做什么

| 端 | 程序 | 用户操作 |
| --- | --- | --- |
| Ubuntu VPS | `rust-ssh relay` | 管理员配置一次，长期运行 systemd 服务 |
| Windows 被控机 | `rust-ssh-client.exe` | 粘贴服务器生成的配置码，点击“启动” |
| Mac/Windows 主控机 | `rust-ssh-connect` | 粘贴同一配置码，选择在线设备，点击“连接” |

client 和 connect 的 GUI 不需要填写 server key 路径、token 路径，也不需要手写 `ProxyCommand`。运行已编译的程序不需要安装 Rust、Cargo、Node.js 或源码。

## 工作方式

```text
Windows client ──主动 Noise 连接──┐
                                  ├── Ubuntu VPS relay:24443
Mac/Windows connect ─主动连接─────┘
                                      │
                                      └──配对目标 client──本机 127.0.0.1:22
```

- relay 可以同时管理多台 Windows client；
- 不同设备可以并行连接；
- 同一设备同时只允许一个 SSH 会话；
- client 默认只允许连接本机 loopback SSH，避免成为内网代理；
- 公网只需要开放 VPS 的一个 TCP 端口，默认 `24443`；
- RustDesk 的 `hbbs/hbbr` 端口保持不变，不要复用。

传输层使用 `Noise_XX_25519_ChaChaPoly_SHA256`。不使用 X.509 证书，也不需要域名。VPS 的 Noise 静态私钥只留在服务器；client/connect 只使用包含公钥和 token 的配置码。

## 1. 下载程序

从 [GitHub Releases](https://github.com/Ameeeeeeeeeeeeee/rust-ssh/releases) 下载对应平台文件：

```text
rust-ssh-relay-linux-x86_64
rust-ssh-client-windows-x86_64.exe
rust-ssh-connect-windows-x86_64.exe
rust-ssh-connect-macos-aarch64
```

如果需要无界面 Windows agent，也可以下载：

```text
rust-ssh-agent-windows-x86_64.exe
```

这个文件用于高级部署；按你的使用方式，先使用 client GUI 即可。

## 2. Ubuntu VPS：一次性配置 relay

下面示例使用 VPS 的公网 IP `203.0.113.10` 和端口 `24443`。`Volc-Engine-Test` 只是本机 SSH 登录别名，不是 relay 地址。

登录 VPS：

```bash
ssh Volc-Engine-Test
```

把 release 文件安装为：

```bash
sudo install -m 0755 rust-ssh-relay-linux-x86_64 /usr/local/bin/rust-ssh
```

创建服务账号和配置目录：

```bash
sudo useradd --system --no-create-home --shell /usr/sbin/nologin rustssh 2>/dev/null || true
sudo install -d -o root -g rustssh -m 0750 /etc/rust-ssh
```

第一次部署时生成 server key：

```bash
sudo /usr/local/bin/rust-ssh keygen \
  --identity-key /etc/rust-ssh/identity.key \
  --public-key /etc/rust-ssh/identity.pub
```

如果这些文件已经存在，不要重复生成。文件含义：

```text
/etc/rust-ssh/identity.key：Noise 私钥，只留在 VPS
/etc/rust-ssh/identity.pub：Noise 公钥，会被写入配置码
```

生成 token。已有 token 时不要再次执行：

```bash
openssl rand -hex 32 | sudo tee /etc/rust-ssh/token >/dev/null
```

设置权限：

```bash
sudo chown root:rustssh /etc/rust-ssh/identity.key
sudo chown root:rustssh /etc/rust-ssh/identity.pub
sudo chown root:rustssh /etc/rust-ssh/token
sudo chmod 0640 /etc/rust-ssh/identity.key
sudo chmod 0644 /etc/rust-ssh/identity.pub
sudo chmod 0640 /etc/rust-ssh/token
```

创建 relay 环境配置：

```bash
sudo tee /etc/rust-ssh/relay.env >/dev/null <<'EOF'
RUST_SSH_LISTEN=0.0.0.0:24443
RUST_SSH_IDENTITY_KEY=/etc/rust-ssh/identity.key
RUST_SSH_TOKEN_FILE=/etc/rust-ssh/token
EOF
```

将仓库中的 `examples/rust-ssh-relay.service` 安装到 systemd：

```bash
sudo install -m 0644 examples/rust-ssh-relay.service \
  /etc/systemd/system/rust-ssh-relay.service
sudo systemctl daemon-reload
sudo systemctl enable --now rust-ssh-relay
sudo systemctl status rust-ssh-relay
```

确认 relay 在监听：

```bash
sudo ss -lntp | grep ':24443'
```

云安全组和 VPS 防火墙只放行 `24443/tcp`。如果该端口与其他程序冲突，可以改成其他端口，但必须同时修改 `relay.env` 和重新生成配置码。

### 生成配置码

执行：

```bash
/usr/local/bin/rust-ssh pair-code \
  --server 203.0.113.10:24443 \
  --server-key /etc/rust-ssh/identity.pub \
  --token-file /etc/rust-ssh/token
```

终端会输出一整行以 `rssh1:` 开头的内容。把这一整行通过安全方式复制给 Windows client 和 Mac/Windows connect。

配置码包含 relay IP、公钥和 token，应当视为秘密，不要公开贴到 GitHub、群聊或 issue。server 私钥 `identity.key` 永远不进入配置码。

## 3. Windows：使用 client 被控端

Windows 本机需要 OpenSSH Server：

```powershell
Get-Service sshd
Test-NetConnection 127.0.0.1 -Port 22
```

下载并打开：

```text
rust-ssh-client-windows-x86_64.exe
```

首次使用只填写：

```text
配置码：粘贴 VPS 上 pair-code 输出的整行内容
设备 ID：例如 DESKTOP-KH8O1JM
本地 SSH：保持 127.0.0.1:22
```

点击：

```text
保存
启动
```

成功后 client 会主动连接 VPS，并出现在 connect 的在线设备列表中。client 不监听公网端口，关闭 GUI 后 client 会停止；按当前设计不自动设置开机自启。

配置保存在：

```text
Windows：%APPDATA%\\rust-ssh\\client.json
```

源码构建方式：

```powershell
cargo build --release --locked --features desktop --bin rust-ssh-client
```

输出文件是：

```text
target\\release\\rust-ssh-client.exe
```

## 4. Mac/Windows：使用 connect 主控端

主控端需要系统 OpenSSH：

```bash
ssh -V
```

Mac ARM64 下载：

```text
rust-ssh-connect-macos-aarch64
```

Windows x86-64 下载：

```text
rust-ssh-connect-windows-x86_64.exe
```

打开后只填写：

```text
配置码：与 client 使用同一段配置码
SSH 用户：Windows OpenSSH 的用户名，例如 ame
```

点击“刷新设备”，选择目标设备，然后：

- 点击“配置 SSH”：自动写入系统 SSH 配置；
- 点击“连接选中设备”：配置并打开系统 Terminal/cmd SSH 窗口。

connect 会自动维护一个标记区块，不会覆盖其他 SSH 配置：

```text
Windows：%USERPROFILE%\\.ssh\\config
Mac：~/.ssh/config
```

设备会生成类似下面的 SSH 主机名：

```text
rust-ssh-DESKTOP-KH8O1JM
```

配置一次后，Terminal 可以直接执行：

```bash
ssh rust-ssh-DESKTOP-KH8O1JM
```

VS Code Remote-SSH 也可以直接选择这个主机名。connect GUI 不需要一直挂着；SSH 启动时会自动调用 connect 的内部代理模式。

配置文件位置：

```text
Windows：%APPDATA%\\rust-ssh\\connect.json
Mac：~/.config/rust-ssh/connect.json
```

## 5. 端口和安全

三端之间的网络关系是：

| 端 | 入站 | 出站 |
| --- | --- | --- |
| Ubuntu relay | 监听 `24443/tcp` | 无需主动连接 |
| Windows client | 不监听公网端口 | 连接 VPS `IP:24443`，再连接本机 `127.0.0.1:22` |
| Mac/Windows connect | 不监听公网端口 | 连接 VPS `IP:24443` |

server key 用来确认服务器身份，token 用来授权，Noise 负责加密。公网端口仍可能被扫描，所以建议使用云安全组、防火墙和来源 IP 限制。当前版本已有握手超时、连接数限制、帧长度限制、设备 ID 校验和单设备单会话限制，但还没有 IP 限速、失败封禁和每设备独立 token。

## 6. 高级 CLI 模式

GUI 不是必须的。源码构建默认 CLI：

```bash
cargo build --release --locked --bin rust-ssh
```

查看在线设备：

```bash
rust-ssh list \
  --server 203.0.113.10:24443 \
  --server-key ~/.config/rust-ssh/server-identity.pub \
  --token-file ~/.config/rust-ssh/token
```

手动运行被控 agent：

```powershell
rust-ssh.exe agent `
  --server 203.0.113.10:24443 `
  --server-key C:\ProgramData\rust-ssh\server-identity.pub `
  --token-file C:\ProgramData\rust-ssh\token.txt `
  --device-id DESKTOP-KH8O1JM `
  --target 127.0.0.1:22
```

高级模式才需要手动处理 key/token 文件。普通用户使用 GUI 配置码即可。

## 7. 源码、Release 与运行时

源码构建需要 Rust stable、Cargo 和 Git。运行 release exe 不需要 Rust。

构建 GUI：

```bash
cargo build --release --locked --features desktop --bin rust-ssh-client
cargo build --release --locked --features desktop --bin rust-ssh-connect
```

推送版本标签后，GitHub Actions 会构建并发布：

```text
rust-ssh-relay-linux-x86_64
rust-ssh-agent-windows-x86_64.exe
rust-ssh-client-windows-x86_64.exe
rust-ssh-connect-windows-x86_64.exe
rust-ssh-connect-macos-aarch64
```

编译文件不会进入 Git 源码仓库，`target/` 已被 `.gitignore` 忽略。

## 排错

- relay：`journalctl -u rust-ssh-relay -f`；
- Windows 测试 relay：`Test-NetConnection 203.0.113.10 -Port 24443`；
- Mac 测试 relay：`nc -vz 203.0.113.10 24443`；
- 配置码无效：确认复制了完整的 `rssh1:` 开头内容；
- 设备不在线：确认 client GUI 正在运行且设备 ID 唯一；
- public key mismatch：重新从当前 VPS 生成配置码；
- SSH 认证失败：中继已经打通，检查 Windows 用户名、密码、公钥和 `sshd`；
- 连接配置异常：检查 `~/.ssh/config` 或 `%USERPROFILE%\\.ssh\\config` 中的 rust-ssh 管理区块。
