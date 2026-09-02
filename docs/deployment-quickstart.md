# rust-ssh 三端部署：简洁版

这份手册把三端放在一起：

```text
Ubuntu VPS：relay 服务器
Windows：client 被控端
Mac / Windows：connect 主控端
```

运行已下载的程序不需要安装 Rust。只需要 Rust 的是“从源码编译”这种开发方式。

## 0. 先准备两个值

先记下两个值，稍后登录 VPS 后再粘贴到终端：

```text
VPS 公网 IP：203.0.113.10
设备 ID：DESKTOP-KH8O1JM
```

`Volc-Engine-Test` 只是你登录 VPS 的 SSH 别名；配置码里必须填写 VPS 公网 IP。

## 1. 配置 Ubuntu VPS relay

在本机执行：

```bash
ssh Volc-Engine-Test
```

登录 VPS 后，在 VPS 终端再设置一次下面两个变量：

```bash
VPS_IP=203.0.113.10
DEVICE_ID=DESKTOP-KH8O1JM
```

从 [v0.3.0 Release](https://github.com/Ameeeeeeeeeeeeee/rust-ssh/releases/tag/v0.3.0) 下载 relay，或在 VPS 上直接执行：

```bash
sudo curl -L --fail -o /usr/local/bin/rust-ssh \
  https://github.com/Ameeeeeeeeeeeeee/rust-ssh/releases/download/v0.3.0/rust-ssh-relay-linux-x86_64
sudo chmod 0755 /usr/local/bin/rust-ssh
sudo curl -L --fail -o /etc/systemd/system/rust-ssh-relay.service \
  https://raw.githubusercontent.com/Ameeeeeeeeeeeeee/rust-ssh/v0.3.0/examples/rust-ssh-relay.service
```

创建目录和 token：

```bash
sudo useradd --system --no-create-home --shell /usr/sbin/nologin rustssh 2>/dev/null || true
sudo install -d -o root -g rustssh -m 0750 /etc/rust-ssh /etc/rust-ssh/devices

# identity 文件只在第一次部署时生成；已有文件不要重生成
sudo /usr/local/bin/rust-ssh keygen \
  --identity-key /etc/rust-ssh/identity.key \
  --public-key /etc/rust-ssh/identity.pub

# 每条只在文件不存在时生成，避免让旧配置码失效
sudo test -e /etc/rust-ssh/controller.token || (openssl rand -hex 32 | sudo tee /etc/rust-ssh/controller.token >/dev/null)
sudo test -e /etc/rust-ssh/devices/$DEVICE_ID.token || (openssl rand -hex 32 | sudo tee /etc/rust-ssh/devices/$DEVICE_ID.token >/dev/null)
```

设置配置和权限：

```bash
sudo chown root:rustssh /etc/rust-ssh/identity.key /etc/rust-ssh/identity.pub \
  /etc/rust-ssh/controller.token /etc/rust-ssh/devices/$DEVICE_ID.token
sudo chmod 0640 /etc/rust-ssh/identity.key /etc/rust-ssh/controller.token \
  /etc/rust-ssh/devices/$DEVICE_ID.token
sudo chmod 0644 /etc/rust-ssh/identity.pub

sudo tee /etc/rust-ssh/relay.env >/dev/null <<'EOF'
RUST_SSH_LISTEN=0.0.0.0:24443
RUST_SSH_IDENTITY_KEY=/etc/rust-ssh/identity.key
RUST_SSH_CONTROLLER_TOKEN_FILE=/etc/rust-ssh/controller.token
RUST_SSH_DEVICES_DIR=/etc/rust-ssh/devices
EOF

sudo systemctl daemon-reload
sudo systemctl enable --now rust-ssh-relay
sudo systemctl status rust-ssh-relay --no-pager
```

云安全组和 VPS 防火墙只放行 `TCP 24443`。不要开放 Windows 的 `22` 端口；`24443` 也不要和 RustDesk 端口冲突。

## 2. 配置 Windows client

在 VPS 上为这台设备生成配置码：

```bash
sudo /usr/local/bin/rust-ssh pair-code \
  --server "$VPS_IP:24443" \
  --server-key /etc/rust-ssh/identity.pub \
  --token-file /etc/rust-ssh/devices/$DEVICE_ID.token
```

复制输出的整行 `rssh1:...`，只交给这台 Windows 设备。

在 Windows 管理员 PowerShell 中确认 OpenSSH Server：

```powershell
Get-Service sshd
Test-NetConnection 127.0.0.1 -Port 22
```

如果没有 `sshd`：

```powershell
Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0
Start-Service sshd
Set-Service -Name sshd -StartupType Automatic
```

下载并打开 `rust-ssh-client-windows-x86_64.exe`，填写：

```text
配置码：刚才生成的设备配置码
设备 ID：必须和服务器的设备 token 文件名一致
本地 SSH：127.0.0.1:22
```

点击“保存”→“启动”，窗口保持打开。client 当前不自动开机启动；关闭窗口就会停止。

## 3. 配置 Mac / Windows connect

在 VPS 上生成主控配置码：

```bash
sudo /usr/local/bin/rust-ssh pair-code \
  --server "$VPS_IP:24443" \
  --server-key /etc/rust-ssh/identity.pub \
  --token-file /etc/rust-ssh/controller.token
```

这段配置码可以查看和连接所有设备，只交给可信的主控端，绝不要交给 client。

从 [v0.3.0 Release](https://github.com/Ameeeeeeeeeeeeee/rust-ssh/releases/tag/v0.3.0) 下载：

```text
Mac Apple Silicon：rust-ssh-connect-macos-aarch64
Windows x86-64：rust-ssh-connect-windows-x86_64.exe
```

Mac 运行前如有需要：

```bash
chmod +x rust-ssh-connect-macos-aarch64
```

打开 connect，填写 controller 配置码和 Windows SSH 用户名，点击：

```text
刷新设备 → 选择设备 → 配置 SSH
```

以后可以直接：

```bash
ssh rust-ssh-DESKTOP-KH8O1JM
```

也可以在 VS Code Remote-SSH 中选择同名主机。connect 配置一次后可以关闭，但 client 必须保持运行；不要移动 connect 可执行文件。

## 4. 记住这三件事

- VPS 只暴露一个 `24443` 端口；client 和 connect 都只主动连接 VPS。
- `controller.token` 只有一份，所有主控端共用；它是全权限主控密钥，不能给 client。
- 每台 Windows 设备各有一个 `<DEVICE_ID>.token`，泄露只影响那一台。

完整解释、增加设备和排错见[详细部署手册](deployment-detailed.md)。
