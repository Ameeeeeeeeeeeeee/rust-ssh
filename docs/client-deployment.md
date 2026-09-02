# rust-ssh client 部署手册

这份手册用于：

```text
Ubuntu VPS relay + Windows 被控端 client
```

最终效果是：Windows client 主动连接 VPS，VPS 不需要访问 Windows 的公网地址。

## 你需要准备

把下面两个值换成自己的值：

```text
VPS_IP       = VPS 公网 IP，例如 203.0.113.10
DEVICE_ID    = Windows 设备名，例如 DESKTOP-KH8O1JM
```

`Volc-Engine-Test` 是你登录 VPS 的 SSH 别名；生成配置码时必须使用 `VPS_IP:24443`，不能使用这个别名。

## 1. VPS 配置

登录 VPS：

```bash
ssh Volc-Engine-Test
```

从 [v0.3.0 Release](https://github.com/Ameeeeeeeeeeeeee/rust-ssh/releases/tag/v0.3.0) 下载
`rust-ssh-relay-linux-x86_64`，并把仓库里的
[`examples/rust-ssh-relay.service`](../examples/rust-ssh-relay.service) 文件也放到 VPS 当前目录，然后安装 relay：

```bash
sudo install -m 0755 rust-ssh-relay-linux-x86_64 /usr/local/bin/rust-ssh
sudo useradd --system --no-create-home --shell /usr/sbin/nologin rustssh 2>/dev/null || true
sudo install -d -o root -g rustssh -m 0750 /etc/rust-ssh /etc/rust-ssh/devices
```

如果 `/etc/rust-ssh/identity.key` 和 `/etc/rust-ssh/identity.pub` 已经存在，保留它们，不要重新生成。第一次部署才执行：

```bash
sudo /usr/local/bin/rust-ssh keygen \
  --identity-key /etc/rust-ssh/identity.key \
  --public-key /etc/rust-ssh/identity.pub
```

生成两类 token。每个命令只执行一次；文件已存在时不要覆盖：

```bash
openssl rand -hex 32 | sudo tee /etc/rust-ssh/controller.token >/dev/null
openssl rand -hex 32 | sudo tee /etc/rust-ssh/devices/DESKTOP-KH8O1JM.token >/dev/null
```

把上面第二条命令里的 `DESKTOP-KH8O1JM` 换成你的 `DEVICE_ID`。

权限设置：

```bash
sudo chown root:rustssh /etc/rust-ssh/identity.key /etc/rust-ssh/identity.pub
sudo chown root:rustssh /etc/rust-ssh/controller.token
sudo chown root:rustssh /etc/rust-ssh/devices/DESKTOP-KH8O1JM.token
sudo chmod 0640 /etc/rust-ssh/identity.key /etc/rust-ssh/controller.token
sudo chmod 0644 /etc/rust-ssh/identity.pub
sudo chmod 0640 /etc/rust-ssh/devices/DESKTOP-KH8O1JM.token
```

创建 relay 配置：

```bash
sudo tee /etc/rust-ssh/relay.env >/dev/null <<'EOF'
RUST_SSH_LISTEN=0.0.0.0:24443
RUST_SSH_IDENTITY_KEY=/etc/rust-ssh/identity.key
RUST_SSH_CONTROLLER_TOKEN_FILE=/etc/rust-ssh/controller.token
RUST_SSH_DEVICES_DIR=/etc/rust-ssh/devices
EOF
```

安装 systemd 服务并启动：

```bash
sudo install -m 0644 examples/rust-ssh-relay.service \
  /etc/systemd/system/rust-ssh-relay.service
sudo systemctl daemon-reload
sudo systemctl enable --now rust-ssh-relay
```

确认 relay 正常：

```bash
sudo systemctl status rust-ssh-relay
sudo ss -lntp | grep ':24443'
```

云安全组和 VPS 防火墙只需要放行：

```text
TCP 24443
```

不要把 Windows 的 22 端口开放到公网；`24443` 也不要和 RustDesk 端口冲突。

## 2. 为这台 client 生成配置码

使用这台设备自己的 token：

```bash
sudo /usr/local/bin/rust-ssh pair-code \
  --server 203.0.113.10:24443 \
  --server-key /etc/rust-ssh/identity.pub \
  --token-file /etc/rust-ssh/devices/DESKTOP-KH8O1JM.token
```

把 `203.0.113.10` 和 `DESKTOP-KH8O1JM` 换成自己的值。

命令输出一整行 `rssh1:...`。这段配置码只交给对应的 Windows client，不要交给 connect。

## 3. Windows client

Windows 需要 OpenSSH Server：

```powershell
Get-Service sshd
Test-NetConnection 127.0.0.1 -Port 22
```

如果没有安装：

```powershell
Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0
Start-Service sshd
Set-Service -Name sshd -StartupType Automatic
```

这里自动启动的是 Windows 的 `sshd`，不是 rust-ssh client。rust-ssh client 仍然按当前设计不做开机自启。

打开 `rust-ssh-client-windows-x86_64.exe`，填写：

```text
配置码：上一步生成的设备配置码
设备 ID：DESKTOP-KH8O1JM
本地 SSH：127.0.0.1:22
```

点击“保存”→“启动”。`设备 ID` 必须和服务器文件名完全一致：

```text
设备 ID：DESKTOP-KH8O1JM
文件名：DESKTOP-KH8O1JM.token
```

client 窗口需要保持打开；关闭窗口就停止。配置保存在：

```text
%APPDATA%\rust-ssh\client.json
```

## 4. 以后添加其他 Windows 设备

在 VPS 上为每台设备创建自己的文件：

```bash
openssl rand -hex 32 | sudo tee /etc/rust-ssh/devices/LAPTOP-ABC123.token >/dev/null
sudo chown root:rustssh /etc/rust-ssh/devices/LAPTOP-ABC123.token
sudo chmod 0640 /etc/rust-ssh/devices/LAPTOP-ABC123.token
```

然后为它生成配置码，复制给那台设备，并重启 relay：

```bash
sudo systemctl restart rust-ssh-relay
```

删除对应 `.token` 文件并重启 relay，就会禁止该设备重新注册。

## 常见问题

- 设备不在线：确认 client 窗口开着、VPS 的 `24443` 可达。
- `device is not configured`：确认设备 ID 和 `.token` 文件名完全一致，并已重启 relay。
- `public key mismatch`：配置码使用了错误的 `identity.pub`，重新生成配置码。
- SSH 认证失败：relay 已经连通，检查 Windows 用户名、密码和 `sshd`。
