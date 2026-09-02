# rust-ssh 三端部署：详细版

这份手册把服务器、Windows 被控端和 Mac/Windows 主控端放在一个流程里。请按顺序操作。

## 0. 先理解三端和网络

| 端 | 系统 | 程序 | 作用 |
| --- | --- | --- | --- |
| 服务器 | Ubuntu | `rust-ssh relay` | 保存密钥和 token，负责中继 |
| 被控端 | Windows x86-64 | `rust-ssh-client` | 主动连接服务器，并把 SSH 转给本机 |
| 主控端 | macOS ARM64 或 Windows x86-64 | `rust-ssh-connect` | 查看在线设备，并发起 SSH |

网络方向只有两条主动连接：

```text
Windows client ──主动 TCP/Noise──> Ubuntu 服务器:24443 <──主动 TCP/Noise── Mac/Windows connect
       │                                                               │
       └──本机 127.0.0.1:22                                           └──本机 SSH / VS Code
```

因此，AP 隔离只要不阻止电脑访问服务器，就不会阻止这套连接。服务器只需要向公网暴露一个 TCP 端口；client 和 connect 都不需要端口映射，也不监听公网端口。

本手册中的 `198.51.100.10`、`relay-server`、`windows-user` 和 `rssh-0123456789abcdef0123456789abcdef` 都是匿名示例，不是固定值。

运行 GitHub Release 中的二进制不需要 Rust。只有从源码编译时才需要 Rust 和 Cargo。

## 1. 准备服务器信息

你需要知道：

- 服务器公网 IP，例如 `198.51.100.10`；
- 登录服务器的方式，例如 `ssh relay-server`；
- 云平台安全组和 Ubuntu 防火墙的管理权限；
- Windows 上 OpenSSH 的登录用户名，例如 `windows-user`。

服务器使用 IP，不使用域名。配置码中的格式必须是 `服务器公网IP:端口`，默认端口为 `24443`。

在服务器登录终端中设置一个变量：

```bash
SERVER_IP=198.51.100.10
export SERVER_IP
```

这个变量只在当前终端有效。重新登录后需要再次执行；后面命令请继续在同一个终端执行。

## 2. Ubuntu 服务器部署 relay

### 2.1 下载 relay

登录服务器：

```bash
ssh relay-server
```

下载最新正式版 relay 和 systemd 服务文件：

```bash
sudo curl -L --fail -o /usr/local/bin/rust-ssh https://github.com/Ameeeeeeeeeeeeee/rust-ssh/releases/latest/download/rust-ssh-relay-linux-x86_64
sudo chmod 0755 /usr/local/bin/rust-ssh
sudo curl -L --fail -o /etc/systemd/system/rust-ssh-relay.service https://raw.githubusercontent.com/Ameeeeeeeeeeeeee/rust-ssh/main/examples/rust-ssh-relay.service
```

如果服务器不能直接访问 GitHub，可以在另一台电脑下载文件，再通过 `scp` 上传；上传后在服务器执行：

```bash
sudo install -m 0755 rust-ssh-relay-linux-x86_64 /usr/local/bin/rust-ssh
sudo install -m 0644 rust-ssh-relay.service /etc/systemd/system/rust-ssh-relay.service
```

### 2.2 创建服务账户和目录

systemd 会让 relay 以低权限用户 `rustssh` 运行。下面命令可以重复执行：

```bash
sudo useradd --system --no-create-home --shell /usr/sbin/nologin rustssh 2>/dev/null || true
sudo install -d -o root -g rustssh -m 0750 /etc/rust-ssh
sudo install -d -o root -g rustssh -m 2750 /etc/rust-ssh/devices
```

`devices` 目录使用 `2750`，其中的 setgid 位会让新生成的设备 token 继承 `rustssh` 组，保证 relay 能读取它们。

### 2.3 生成服务器 identity

第一次部署时执行：

```bash
sudo /usr/local/bin/rust-ssh keygen --identity-key /etc/rust-ssh/identity.key --public-key /etc/rust-ssh/identity.pub
```

文件用途如下：

| 文件 | 用途 | 能否分发 |
| --- | --- | --- |
| `/etc/rust-ssh/identity.key` | Noise 私钥，用于证明服务器身份 | 不能，只留服务器 |
| `/etc/rust-ssh/identity.pub` | 公钥，写入配置码让端点锁定服务器 | 可以随配置流程使用 |

`identity.key` 是长期身份。升级程序、重启服务、重启服务器都保留它；不要再次执行 `keygen`，否则旧配置码中的 server key 会失效。

### 2.4 生成 controller token

controller token 是主控总钥匙，服务器只生成一份：

```bash
sudo test -e /etc/rust-ssh/controller.token || (openssl rand -hex 32 | sudo tee /etc/rust-ssh/controller.token >/dev/null)
```

所有可信的 connect 可以使用同一个 controller 配置码。v0.4 暂不提供每个主控端独立权限；因此 controller token 或其配置码泄露，就等于所有设备的主控权限泄露。

给 relay 设置读取权限：

```bash
sudo chown root:rustssh /etc/rust-ssh/identity.key /etc/rust-ssh/identity.pub /etc/rust-ssh/controller.token /etc/rust-ssh/devices
sudo chmod 0640 /etc/rust-ssh/identity.key /etc/rust-ssh/controller.token
sudo chmod 0644 /etc/rust-ssh/identity.pub
```

此时还不要手动创建设备 token。v0.4 的正确顺序是先打开 client 取得它的随机设备 ID，再使用 `device add` 注册。

### 2.5 写入 relay 配置并启动

创建环境文件：

```bash
sudo tee /etc/rust-ssh/relay.env >/dev/null <<'EOF'
RUST_SSH_LISTEN=0.0.0.0:24443
RUST_SSH_IDENTITY_KEY=/etc/rust-ssh/identity.key
RUST_SSH_CONTROLLER_TOKEN_FILE=/etc/rust-ssh/controller.token
RUST_SSH_DEVICES_DIR=/etc/rust-ssh/devices
EOF
```

这个 heredoc 配置块本身需要换行；除此之外，手册中的命令都可以一行执行。

安装并启动持久化服务：

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now rust-ssh-relay
```

检查服务：

```bash
sudo systemctl status rust-ssh-relay --no-pager
sudo ss -lntp | grep ':24443'
sudo journalctl -u rust-ssh-relay -n 50 --no-pager
```

如果服务因为“没有 device token 目录”启动失败，确认 `/etc/rust-ssh/devices` 存在且权限为 `root:rustssh`，然后执行 `sudo systemctl restart rust-ssh-relay`。

### 2.6 配置云安全组和 Ubuntu 防火墙

只放行服务器的 `TCP 24443`：

```bash
sudo ufw allow 24443/tcp
```

如果你使用云安全组，也要添加一条入方向 TCP `24443` 规则。不要开放 Windows 的 `22` 端口；Windows 的 `22` 只允许 client 在本机访问。

如果 `24443` 已经被别的程序占用，rust-ssh 不能复用它。换成另一个端口后，需要同时修改 `RUST_SSH_LISTEN` 和所有配置码中的服务器地址；RustDesk 使用的端口也不要重复占用。

## 3. Windows 被控端部署 client

### 3.1 下载并首次打开

从[最新 Release](https://github.com/Ameeeeeeeeeeeeee/rust-ssh/releases/latest) 下载并安装 MSI：

```text
rust-ssh-client-windows-x86_64.msi
```

Release 不提供单独的 Windows `.exe`。MSI 会把 client 安装到 Windows 程序目录并创建开始菜单入口；以后安装更高版本 MSI 可以覆盖升级，不会自动清除 `%APPDATA%\rust-ssh\client.json`。

第一次打开时，client GUI 会显示类似下面的设备 ID：

```text
rssh-0123456789abcdef0123456789abcdef
```

点击“复制”，把这串 ID 交给服务器管理员。这个 ID：

- 由 client 首次运行时随机生成；
- 保存在 `%APPDATA%\rust-ssh\client.json`；
- 不使用 Windows 的 `COMPUTERNAME` 或主机名；
- 不会因修改 Windows 主机名而变化；
- 不是秘密，单独知道它不能通过认证。

随机 ID 有 128 位随机空间，正常情况下不会重复。服务器的 `device add` 还会拒绝同一个 ID 的重复注册；relay 也会拒绝同一个 ID 同时运行两个 client。

当前设计不让用户在 UI 中直接编辑 ID，因为随意改 ID 会造成“本机 ID、服务器 token 和配置码”三者不一致。需要更换身份时，按第 7.2 节操作。

### 3.2 确认 Windows OpenSSH Server

在 Windows 管理员 PowerShell 中执行：

```powershell
Get-Service sshd
Test-NetConnection 127.0.0.1 -Port 22
```

如果没有 `sshd`，安装并启动：

```powershell
Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0
Start-Service sshd
Set-Service -Name sshd -StartupType Automatic
```

这里设置自动启动的是 Windows 自带的 OpenSSH Server，不是 rust-ssh client。按当前设计，client 不自动开机启动；需要使用时手动打开即可。

### 3.3 在服务器登记设备

把 client UI 显示的完整 ID 填入服务器终端变量。示例：

```bash
DEVICE_ID=rssh-0123456789abcdef0123456789abcdef
```

执行注册命令：

```bash
sudo /usr/local/bin/rust-ssh device add --device-id "$DEVICE_ID" --server "$SERVER_IP:24443" --server-key /etc/rust-ssh/identity.pub --devices-dir /etc/rust-ssh/devices
```

这个命令会：

1. 检查 ID 是否是 v0.4 client 生成的格式；
2. 在服务器写入 `/etc/rust-ssh/devices/<设备ID>.token`；
3. 生成只属于这台设备的 token；
4. 输出绑定了该设备 ID 的 `rssh1:...` 配置码。

如果提示 `device is already registered`，说明服务器已经有同名设备。不要随便删除旧 token；先确认是不是同一台设备。如果确实要重新注册，见第 7.2 节。

复制命令输出的整行 `rssh1:...` 配置码，暂时不要发到公共聊天或提交 GitHub。它包含设备 token，只交给对应的 Windows client。

relay 会自动读取新的 token，通常等待约 2 秒即可，不需要重启服务。服务器可以用下面的命令查看当前保存的设备和 token 文件：

```bash
sudo /usr/local/bin/rust-ssh inventory --controller-token-file /etc/rust-ssh/controller.token --devices-dir /etc/rust-ssh/devices
```

默认不会打印 token 内容；只有在服务器本地可信终端排查时才追加 `--show-tokens`。这个命令不会显示曾经使用过配置码的 connect 数量，因为多个 connect 共享同一个 controller token，server 不保存 connect 的单独登记。

### 3.4 在 client 中完成配置

回到 Windows client GUI：

| 字段 | 填写内容 |
| --- | --- |
| 配置码 | 服务器 `device add` 输出的整行 `rssh1:...` |
| 设备 ID | UI 已生成并显示的 ID，不需要修改 |
| 本地 SSH | 默认 `127.0.0.1:22` |

点击“保存”→“启动”。状态的含义是：

- 黄色“正在连接”：正在建立 Noise/relay 连接；
- 绿色“已连接服务器”：relay 已认证通过，正在等待 SSH；
- 黄色“连接中断，正在重试”：网络或服务器暂时不可用，client 会自动重连；
- 红色“连接失败”：配置或本地参数有误，需要检查提示。

client 不自动开机启动。关闭窗口只会隐藏到 Windows 右下角托盘，client 仍会运行；右键托盘图标选择“关闭”才会真正停止。托盘图标颜色与上述连接状态一致。

client 只会把服务器中继过来的连接转发到本机 loopback 地址。即使配置了其他局域网，client 也不会把它们作为目标暴露给主控端。

## 4. Mac / Windows 主控端部署 connect

### 4.1 生成 controller 配置码

在服务器执行：

```bash
sudo /usr/local/bin/rust-ssh pair-code --server "$SERVER_IP:24443" --server-key /etc/rust-ssh/identity.pub --token-file /etc/rust-ssh/controller.token
```

输出的整行 `rssh1:...` 是 controller 配置码。它可以列出并连接服务器上所有在线 client，因此不要发给任何被控设备，也不要放进源码仓库。

controller 只有一个 token 文件，但可以有多个 connect 实例使用同一份 controller 配置码。v0.4 没有 controller 子账号和细粒度 ACL；需要不同权限时应另行设计权限层。

### 4.2 下载并打开 connect

从[最新 Release](https://github.com/Ameeeeeeeeeeeeee/rust-ssh/releases/latest) 下载：

```text
macOS Apple Silicon：rust-ssh-connect-macos-aarch64
Windows x86-64：rust-ssh-connect-windows-x86_64.msi
```

Release 文件已经包含运行所需的 Rust 代码；主控端不需要安装 Rust。macOS 下载后如没有执行权限，执行：

```bash
chmod +x rust-ssh-connect-macos-aarch64
```

Windows 双击 MSI 安装，然后从开始菜单打开 rust-ssh connect。MSI 安装路径会被 GUI 自动写入 SSH 配置；不要移动安装目录中的程序文件，否则需要重新点击“配置 SSH”。

主控端还需要系统 OpenSSH。检查：

```bash
ssh -V
```

### 4.3 使用 connect GUI

打开 connect，填写：

| GUI 字段 | 填写内容 |
| --- | --- |
| 配置码 | 服务器生成的 controller 配置码 |
| SSH 用户 | Windows OpenSSH 的登录用户名，例如 `windows-user` |

点击“刷新设备”。Windows client 正在运行且已通过认证时，列表会出现它的设备 ID。选择设备后可以：

- 点击“配置 SSH”：只写入 SSH 配置，不立即打开终端；
- 点击“连接选中设备”：配置并打开系统 Terminal 或命令窗口。

connect 的配置文件位置：

```text
macOS：~/.config/rust-ssh/connect.json
Windows：%APPDATA%\rust-ssh\connect.json
```

配置 SSH 后，connect 会在系统 SSH 配置中写入一个受管理区块：

```text
macOS：~/.ssh/config
Windows：%USERPROFILE%\.ssh\config
```

示例设备 ID 下，默认 Host 昵称就是设备 ID，直接使用：

```bash
ssh rssh-0123456789abcdef0123456789abcdef
```

在设备列表上右键可以选择“设置 Host 昵称”。昵称只允许字母、数字、点、下划线和短横线；保存后再次点击“配置 SSH”，以后就可以直接执行 `ssh 你设置的昵称`。SSH 配置中那条较长的 `ProxyCommand` 是内部实现，不需要手写，正常使用时只输入 `ssh Host昵称`。

也可以在 VS Code Remote-SSH 中选择同名主机。第一次 SSH 登录时，输入 Windows OpenSSH 用户的密码，或使用该 Windows 用户已经配置好的 SSH 公钥。

配置完成后，connect GUI 可以关闭，因为 SSH 会通过 SSH `ProxyCommand` 自动调用 connect 的内部代理模式。但是 Windows client 必须继续运行；如果移动或卸载 connect，需要重新安装/打开 connect 并再次点击“配置 SSH”。

## 5. 配置文件和密钥分别放在哪里

### 5.1 服务器

| 路径 | 内容 | 保密要求 |
| --- | --- | --- |
| `/etc/rust-ssh/identity.key` | 服务器 Noise 私钥 | 只留服务器 |
| `/etc/rust-ssh/identity.pub` | 服务器 Noise 公钥 | 可随配置码分发 |
| `/etc/rust-ssh/controller.token` | 主控总 token | 只留服务器，不给 client |
| `/etc/rust-ssh/devices/<device_id>.token` | 对应设备 token | 只给对应 client |
| `/etc/rust-ssh/relay.env` | relay 启动变量 | 不含 token 内容，可保留在服务器 |

### 5.2 Windows client

```text
%APPDATA%\rust-ssh\client.json
```

其中保存设备 ID、设备配置码和本地 SSH 目标。设备配置码包含设备 token，应当按秘密材料保护。

### 5.3 Mac / Windows connect

```text
macOS：~/.config/rust-ssh/connect.json
Windows：%APPDATA%\rust-ssh\connect.json
```

其中保存 controller 配置码。它的权限等同于 controller token，不要复制给 client。

## 6. 添加第二台或更多 Windows client

每台设备都重复同一套顺序，不要复用设备 token：

1. 打开新 Windows client，复制 UI 显示的新设备 ID；
2. 服务器执行 `rust-ssh device add --device-id <新ID> ...`；
3. 等待约 2 秒，让 relay 自动热加载 token；
4. 把输出的设备配置码粘贴到对应 client；
5. 在 connect 点击“刷新设备”。

服务器上的 token 文件是一台设备一个：

```text
/etc/rust-ssh/devices/rssh-设备A.token
/etc/rust-ssh/devices/rssh-设备B.token
```

不同设备的 token 不相同。某一台设备 token 泄露时，攻击者只能尝试以那一个设备 ID 注册，不能使用它列出或连接其他设备。

## 7. 修改主机名、替换设备身份和升级

### 7.1 修改 Windows 计算机名

可以随意修改 Windows 计算机名。rust-ssh v0.4 不读取 `COMPUTERNAME`，设备 ID 与主机名无关，所以修改主机名不会让设备掉线，也不需要重新注册。

### 7.2 更换一台设备的 rust-ssh 身份

通常不需要更换。只有在你想让同一台电脑作为“全新设备”登记，或怀疑旧设备配置码泄露时才这样做：

1. 关闭 client；
2. 在 Windows PowerShell 执行备份（不要直接删除）：

```powershell
Move-Item "$env:APPDATA\rust-ssh\client.json" "$env:APPDATA\rust-ssh\client.json.bak"
```

3. 重新打开 client，它会生成新的随机设备 ID；
4. 用新 ID 执行一次服务器 `device add`；
5. 把新配置码粘贴到 client；relay 会自动热加载新的设备 token；
6. 确认新设备在线后，再在服务器移走旧 token；relay 会自动热加载变更。

移走旧 token 的可恢复写法：

```bash
sudo mv "/etc/rust-ssh/devices/$OLD_DEVICE_ID.token" "/etc/rust-ssh/devices/$OLD_DEVICE_ID.token.disabled"
```

`*.token.disabled` 不会被 relay 当作设备 token 读取；需要恢复时再改回 `.token` 文件名，等待约 2 秒即可生效。

### 7.3 从 v0.3 升级到当前版本

服务器升级时保留：

- `identity.key` 和 `identity.pub`；
- `controller.token`；
- 已有的 `/etc/rust-ssh/devices/` 目录。

替换 `/usr/local/bin/rust-ssh` 后执行：

```bash
sudo systemctl restart rust-ssh-relay
```

旧版 client 配置没有 v0.4 配置版本标记。v0.4 client 首次打开时会生成新的随机设备 ID，并清空旧设备配置码；这是为了避免继续使用依赖主机名的旧身份。需要按第 3 节重新 `device add`。旧 token 文件可以暂时保留，确认旧 client 不再使用后再移走。Windows client/connect 使用 MSI；安装新 MSI 会覆盖升级程序，但保留 `%APPDATA%\rust-ssh` 配置。

### 7.4 替换 controller token

controller token 泄露时，应在服务器生成新 token，并为所有 connect 重新生成配置码。先备份旧文件，再执行：

```bash
sudo mv /etc/rust-ssh/controller.token /etc/rust-ssh/controller.token.old
openssl rand -hex 32 | sudo tee /etc/rust-ssh/controller.token >/dev/null
sudo chown root:rustssh /etc/rust-ssh/controller.token
sudo chmod 0640 /etc/rust-ssh/controller.token
```

然后按第 4.1 节重新生成 controller 配置码，等待约 2 秒让 relay 热加载。旧 controller 配置码会失效；已经建立的 SSH 会话不会被强制断开。

## 8. server 和 client 各自暴露什么

| 端 | 对外监听 | 主动连接 | 需要给对方的内容 |
| --- | --- | --- | --- |
| 服务器 relay | `0.0.0.0:24443` 一个 TCP 端口 | 不需要主动连 client | 给 client/connect：服务器 IP、端口和 `identity.pub`（通过配置码） |
| Windows client | 不监听公网端口 | 连接服务器 `24443` | 给服务器管理员：UI 显示的设备 ID；从服务器接收自己的设备配置码 |
| Mac/Windows connect | 不监听公网端口 | 连接服务器 `24443` | 从服务器接收 controller 配置码；不需要给 client 任何 token |

server 和 client 之间不是直接暴露 SSH 端口。服务器只中继加密流量；真正的 SSH 服务仍是 Windows 本机的 `127.0.0.1:22`。

## 9. 安全边界

- 不需要 X.509 证书，也不需要域名；Noise server key pinning 用于防止把配置码指向错误的服务器。
- 公网端口可以被扫描，这是所有公网服务都无法完全避免的；扫描者没有正确 token 不能完成角色认证。
- server 会检查协议版本、设备 ID、controller/device token，并限制握手时间、总连接数、帧大小和单设备并发会话。
- device token 是单设备权限；controller token 是全设备权限。绝不能把 controller 配置码分发给 client。
- 云安全组和 Ubuntu 防火墙只开放 `24443/tcp`，保持 Ubuntu、Windows OpenSSH 和 rust-ssh Release 更新。
- 当前版本不包含 IP 限速、失败封禁、细粒度 ACL、审计、BitLocker、证书体系和自动下载新版本；token 文件变更会自动热加载，但只影响新的认证。

## 10. 常见问题

### relay 启动失败

查看：

```bash
sudo systemctl status rust-ssh-relay --no-pager
sudo journalctl -u rust-ssh-relay -n 100 --no-pager
```

重点检查：`identity.key` 是否存在、controller token 是否至少 32 个非空白字符、`devices` 目录是否存在、服务用户是否能读取这些路径，以及 `24443` 是否已被占用。

### `device is not configured`

说明服务器没有加载这个设备 ID 对应的 token。检查 ID 是否完整复制，确认文件名为 `/etc/rust-ssh/devices/<完整设备ID>.token`，然后执行 `sudo systemctl restart rust-ssh-relay`。

### `device pairing code` 不匹配

设备配置码只能粘贴回生成它的那台 client。不要手动修改 UI 中的 ID，也不要把 controller 配置码粘贴到 client。

### connect 看不到设备

确认 client 已点击“启动”且窗口仍开着；确认服务器 `24443/tcp` 在云安全组和防火墙中放行；确认 connect 使用的是 controller 配置码，而不是某台设备的配置码。

### `public key mismatch`

不要重新运行 `keygen`。服务器身份变化后，旧配置码会失效；使用当前服务器的 `/etc/rust-ssh/identity.pub` 重新生成配置码，并确保 IP 和端口正确。

### SSH 已连通但登录失败

这通常说明 rust-ssh 中继已经正常。检查 Windows 的 OpenSSH Server、SSH 用户名、Windows 密码或该用户的 SSH 公钥配置；rust-ssh 不替代 Windows SSH 的用户认证。

## 11. 源码构建

普通部署不需要 Rust。开发者从源码构建时，在对应系统安装 Rust stable 和 Cargo，然后执行：

```bash
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets --all-features -- -D warnings
```

构建服务器 relay：

```bash
cargo build --release --locked --bin rust-ssh
```

构建 Windows client 或 connect：

```bash
cargo build --release --locked --features desktop --bin rust-ssh-client
cargo build --release --locked --features desktop --bin rust-ssh-connect
```

桌面前端使用 Rust/egui 编译为原生程序，不需要 Node.js、Tauri 或额外运行时。
