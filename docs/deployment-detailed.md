# rust-ssh 三端部署：详细版

这份手册把三端的完整流程放在一起：

| 端 | 系统 | 程序 | 作用 |
| --- | --- | --- | --- |
| 服务器端 | Ubuntu VPS | `rust-ssh relay` | 保存身份密钥和 token，负责中继 |
| 被控端 | Windows x86-64 | `rust-ssh-client` | 主动连接 VPS，并把 SSH 转给本机 |
| 主控端 | macOS ARM64 或 Windows x86-64 | `rust-ssh-connect` | 查看在线设备，并通过 SSH 连接 |

最终网络关系是：

```text
Windows client ──主动连接──> Ubuntu VPS:24443 <──主动连接── Mac/Windows connect
       │                                           │
       └──本机 127.0.0.1:22                       └──SSH / VS Code
```

两台电脑即使处于不同 AP、开启 AP 隔离或没有公网端口，也能通过 VPS 互通。运行 Release 中的程序不需要安装 Rust；Rust 只用于源码编译。

## 1. 准备信息

需要准备：

- Ubuntu VPS 的公网 IP，例如 `203.0.113.10`；
- 能登录 VPS 的 SSH 方式，例如你的 `ssh Volc-Engine-Test`；
- Windows 被控机的设备 ID，例如 `DESKTOP-KH8O1JM`；
- Windows 上实际登录 OpenSSH 的用户名，例如 `ame`。

设备 ID 只能包含字母、数字、`.`、`_`、`-`，并且每台设备必须唯一。

`Volc-Engine-Test` 只是本机 SSH 配置里的登录别名，不能放进 rust-ssh 配置码；配置码必须使用 VPS 公网 IP 和 `24443` 端口。

## 2. Ubuntu VPS：安装并运行 relay

### 2.1 下载程序和 systemd 服务

在本机登录 VPS：

```bash
ssh Volc-Engine-Test
```

下面命令在 VPS 终端执行。先把变量替换成自己的值；登录 SSH 后需要在 VPS 终端重新设置一次：

```bash
VPS_IP=203.0.113.10
DEVICE_ID=DESKTOP-KH8O1JM
```

下载 v0.3.0 的 Linux relay 和服务文件：

```bash
sudo curl -L --fail -o /usr/local/bin/rust-ssh \
  https://github.com/Ameeeeeeeeeeeeee/rust-ssh/releases/download/v0.3.0/rust-ssh-relay-linux-x86_64
sudo chmod 0755 /usr/local/bin/rust-ssh
sudo curl -L --fail -o /etc/systemd/system/rust-ssh-relay.service \
  https://raw.githubusercontent.com/Ameeeeeeeeeeeeee/rust-ssh/v0.3.0/examples/rust-ssh-relay.service
```

如果 VPS 不能直接访问 GitHub，也可以在本机下载后，用 `scp` 上传到 VPS 当前目录，再执行：

```bash
sudo install -m 0755 rust-ssh-relay-linux-x86_64 /usr/local/bin/rust-ssh
sudo install -m 0644 rust-ssh-relay.service /etc/systemd/system/rust-ssh-relay.service
```

### 2.2 创建 relay 账户和身份密钥

```bash
sudo useradd --system --no-create-home --shell /usr/sbin/nologin rustssh 2>/dev/null || true
sudo install -d -o root -g rustssh -m 0750 /etc/rust-ssh /etc/rust-ssh/devices
```

第一次部署时生成身份密钥：

```bash
sudo /usr/local/bin/rust-ssh keygen \
  --identity-key /etc/rust-ssh/identity.key \
  --public-key /etc/rust-ssh/identity.pub
```

这两个文件的含义：

| 文件 | 用途 | 是否能给别人 |
| --- | --- | --- |
| `/etc/rust-ssh/identity.key` | relay 的 Noise 私钥 | 不能，永远只留在 VPS |
| `/etc/rust-ssh/identity.pub` | 写入配置码，用来确认 relay 身份 | 可以通过配置码间接使用 |

升级 relay 或重启 VPS 时保留这两个文件，不要再次运行 `keygen`。如果重新生成，旧配置码中的 server key 会不匹配。

### 2.3 创建 controller token 和设备 token

v0.3.0 有两种 token：

- `/etc/rust-ssh/controller.token`：只有一份，给所有可信的 connect 主控端使用；它可以查看和连接所有设备；
- `/etc/rust-ssh/devices/<DEVICE_ID>.token`：每台设备一份，只能让对应的 device ID 注册。

只在文件不存在时生成，避免让已经发出的配置码失效：

```bash
sudo test -e /etc/rust-ssh/controller.token || \
  (openssl rand -hex 32 | sudo tee /etc/rust-ssh/controller.token >/dev/null)
sudo test -e /etc/rust-ssh/devices/$DEVICE_ID.token || \
  (openssl rand -hex 32 | sudo tee /etc/rust-ssh/devices/$DEVICE_ID.token >/dev/null)
```

设置 relay 服务读取这些文件所需的权限：

```bash
sudo chown root:rustssh \
  /etc/rust-ssh/identity.key \
  /etc/rust-ssh/identity.pub \
  /etc/rust-ssh/controller.token \
  /etc/rust-ssh/devices/$DEVICE_ID.token
sudo chmod 0640 \
  /etc/rust-ssh/identity.key \
  /etc/rust-ssh/controller.token \
  /etc/rust-ssh/devices/$DEVICE_ID.token
sudo chmod 0644 /etc/rust-ssh/identity.pub
```

### 2.4 写 relay 配置并启动服务

```bash
sudo tee /etc/rust-ssh/relay.env >/dev/null <<'EOF'
RUST_SSH_LISTEN=0.0.0.0:24443
RUST_SSH_IDENTITY_KEY=/etc/rust-ssh/identity.key
RUST_SSH_CONTROLLER_TOKEN_FILE=/etc/rust-ssh/controller.token
RUST_SSH_DEVICES_DIR=/etc/rust-ssh/devices
EOF

sudo systemctl daemon-reload
sudo systemctl enable --now rust-ssh-relay
```

检查状态和日志：

```bash
sudo systemctl status rust-ssh-relay --no-pager
sudo ss -lntp | grep ':24443'
sudo journalctl -u rust-ssh-relay -n 50 --no-pager
```

云安全组和 VPS 防火墙只需要放行：

```text
TCP 24443
```

不要放行 Windows 的 `22` 端口。`24443` 是 rust-ssh 的端口，不能和已有 RustDesk 服务端口复用；如果确实要换端口，必须同时改 `RUST_SSH_LISTEN`，并重新生成两类配置码。

### 2.5 生成两种配置码

设备配置码给 Windows client：

```bash
sudo /usr/local/bin/rust-ssh pair-code \
  --server "$VPS_IP:24443" \
  --server-key /etc/rust-ssh/identity.pub \
  --token-file /etc/rust-ssh/devices/$DEVICE_ID.token
```

主控配置码给 Mac/Windows connect：

```bash
sudo /usr/local/bin/rust-ssh pair-code \
  --server "$VPS_IP:24443" \
  --server-key /etc/rust-ssh/identity.pub \
  --token-file /etc/rust-ssh/controller.token
```

两个命令都会输出一整行 `rssh1:...`：

- 设备配置码只能给对应的 Windows client；
- 主控配置码只能给可信的 connect；
- 配置码包含 token，不要发到 GitHub、issue、公共群聊或截图中。

## 3. Windows：配置被控端 client

### 3.1 确认 Windows OpenSSH Server

在 Windows 管理员 PowerShell 中执行：

```powershell
Get-Service sshd
Test-NetConnection 127.0.0.1 -Port 22
```

如果找不到 `sshd`，安装并启动：

```powershell
Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0
Start-Service sshd
Set-Service -Name sshd -StartupType Automatic
```

这里设置自动启动的是 Windows 自带的 `sshd`，不是 rust-ssh client。按当前设计，client 不会自动开机启动。

### 3.2 下载并配置 client

从 [v0.3.0 Release](https://github.com/Ameeeeeeeeeeeeee/rust-ssh/releases/tag/v0.3.0) 下载：

```text
rust-ssh-client-windows-x86_64.exe
```

打开程序，填写：

| GUI 字段 | 填写内容 |
| --- | --- |
| 配置码 | 第 2.5 节生成的“设备配置码”整行内容 |
| 设备 ID | 必须与服务器的 `$DEVICE_ID.token` 文件名去掉 `.token` 后完全一致 |
| 本地 SSH | `127.0.0.1:22` |

例如：

```text
设备 ID：DESKTOP-KH8O1JM
服务器文件：/etc/rust-ssh/devices/DESKTOP-KH8O1JM.token
本地 SSH：127.0.0.1:22
```

点击“保存”→“启动”。看到正在连接服务器后，让 client 窗口保持打开；关闭窗口，client 就会停止。

client 的 GUI 配置保存在：

```text
%APPDATA%\rust-ssh\client.json
```

client 只主动连接 VPS，不监听公网端口，也不会因为 Windows 的 `sshd` 自动启动而自动启动。

## 4. Mac / Windows：配置主控端 connect

### 4.1 下载程序

从 [v0.3.0 Release](https://github.com/Ameeeeeeeeeeeeee/rust-ssh/releases/tag/v0.3.0) 下载对应系统的文件：

```text
macOS Apple Silicon：rust-ssh-connect-macos-aarch64
Windows x86-64：rust-ssh-connect-windows-x86_64.exe
```

主控端还需要系统 OpenSSH：

```bash
ssh -V
```

macOS 如果提示没有执行权限：

```bash
chmod +x rust-ssh-connect-macos-aarch64
```

### 4.2 配置 GUI

打开 connect，填写：

| GUI 字段 | 填写内容 |
| --- | --- |
| 配置码 | 第 2.5 节生成的“主控配置码”整行内容 |
| SSH 用户 | Windows 上实际使用的 OpenSSH 登录用户名，例如 `ame` |

点击“刷新设备”。如果 Windows client 已经启动，列表里会出现它的设备 ID。选择设备后点击“配置 SSH”，或直接点击“连接选中设备”。

connect 的配置保存在：

```text
macOS：~/.config/rust-ssh/connect.json
Windows：%APPDATA%\rust-ssh\connect.json
```

### 4.3 使用 Terminal、PowerShell 或 VS Code

配置 SSH 后，connect 会在用户 SSH 配置中写入一个受管理区块：

```text
macOS：~/.ssh/config
Windows：%USERPROFILE%\.ssh\config
```

设备 ID 为 `DESKTOP-KH8O1JM` 时，直接执行：

```bash
ssh rust-ssh-DESKTOP-KH8O1JM
```

也可以在 VS Code Remote-SSH 中选择 `rust-ssh-DESKTOP-KH8O1JM`。第一次连接时输入 Windows 账户的 SSH 密码或使用 Windows 上已配置的 SSH 密钥。

配置完成后，connect GUI 不需要一直打开；SSH 会调用 connect 的内部 proxy 模式。但是：

- Windows client 必须一直运行；
- 不要移动或删除 `rust-ssh-connect` 可执行文件，因为 SSH 配置会引用它的路径；
- 如果移动了 connect，重新打开 GUI，并对设备点击一次“配置 SSH”。

多台 Mac/Windows 主控端可以使用同一份主控配置码，它们共享 controller token 的全部权限。不要把这份配置码发给任何被控设备。

## 5. 添加、替换和删除设备

### 添加设备

在 VPS 上设置新设备 ID，例如 `LAPTOP-ABC123`：

```bash
NEW_DEVICE_ID=LAPTOP-ABC123
sudo test -e /etc/rust-ssh/devices/$NEW_DEVICE_ID.token || \
  (openssl rand -hex 32 | sudo tee /etc/rust-ssh/devices/$NEW_DEVICE_ID.token >/dev/null)
sudo chown root:rustssh /etc/rust-ssh/devices/$NEW_DEVICE_ID.token
sudo chmod 0640 /etc/rust-ssh/devices/$NEW_DEVICE_ID.token
sudo systemctl restart rust-ssh-relay
```

生成这个设备自己的配置码：

```bash
sudo /usr/local/bin/rust-ssh pair-code \
  --server "$VPS_IP:24443" \
  --server-key /etc/rust-ssh/identity.pub \
  --token-file /etc/rust-ssh/devices/$NEW_DEVICE_ID.token
```

把新配置码交给对应的 Windows client。所有 connect 仍然使用同一份 controller 配置码。

### 删除或替换设备 token

删除并重启 relay 后，该设备不能再次注册：

```bash
sudo rm /etc/rust-ssh/devices/DESKTOP-KH8O1JM.token
sudo systemctl restart rust-ssh-relay
```

如果只想替换 token，删除旧文件、生成同名新文件、设置权限，再重启 relay。旧设备配置码会失效。

替换 `/etc/rust-ssh/controller.token` 也需要重启 relay，并重新给所有主控端生成配置码；controller token 泄露时应立即这样做。

## 6. 认证和安全边界

- relay 只对外监听一个 TCP 端口：默认 `24443`；client 和 connect 不监听公网端口。
- Noise server key 用来确认连接的是正确的 VPS；不需要域名，也不需要 X.509 证书。
- 每个设备 token 只绑定一个 `device_id`；某台设备 token 泄露，不能用来列出或连接其他设备。
- controller token 是主控密钥，泄露后可以列出并连接所有在线设备；它不能分发给 client。
- relay 会拒绝未知设备 ID、错误 token，并限制握手时间、连接数、帧大小和单设备并发会话。
- 公网端口仍然可能被扫描；云安全组和 VPS 防火墙只开放 `24443/tcp`，并保持系统更新。

当前版本不包含 IP 限速、失败封禁、ACL、审计、token 热吊销和 BitLocker 等功能。

## 7. 常见问题

### relay 启动失败

查看：

```bash
sudo systemctl status rust-ssh-relay --no-pager
sudo journalctl -u rust-ssh-relay -n 100 --no-pager
```

重点检查四个路径是否存在、权限是否正确，以及 `24443` 是否已经被其他程序占用。

### connect 中没有设备

确认 Windows client 窗口仍然打开并显示正在运行；确认云安全组和 VPS 防火墙放行 `24443/tcp`；确认 connect 使用的是 controller 配置码，而不是某台设备配置码。

### `device is not configured`

确认设备 ID 与 `/etc/rust-ssh/devices/<device_id>.token` 的文件名完全一致，并在新增或修改 token 后重启 relay。

### `public key mismatch`

不要重新生成 identity key。使用当前 VPS 上的 `/etc/rust-ssh/identity.pub` 重新生成配置码。

### SSH 已连通但认证失败

这说明 rust-ssh 中继大概率已经正常。检查 Windows 的 OpenSSH Server、Windows 用户名、密码或 SSH 公钥配置；rust-ssh 不替代 Windows SSH 的用户认证。
