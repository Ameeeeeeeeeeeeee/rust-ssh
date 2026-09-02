# rust-ssh 三端部署：简洁版

这份手册只保留能跑通的步骤。三端关系如下：

```text
Windows client ──主动连接──> Ubuntu 服务器:24443 <──主动连接── Mac/Windows connect
```

- 服务器只需要对外开放一个 TCP 端口：`24443`。
- client 和 connect 都不需要对外开放端口。
- Release 下载文件可以直接运行，不需要安装 Rust。
- 示例中的 IP、用户名和设备 ID 都是虚构值，请替换成自己的值。

## 1. 先部署服务器

登录你的 Ubuntu 服务器：

```bash
ssh relay-server
```

下面命令都在服务器终端执行。把示例 IP 换成服务器公网 IP；这里使用文档保留地址 `198.51.100.10` 只是示例：

```bash
SERVER_IP=198.51.100.10
export SERVER_IP
```

下载最新正式版 relay 和 systemd 服务：

```bash
sudo curl -L --fail -o /usr/local/bin/rust-ssh https://github.com/Ameeeeeeeeeeeeee/rust-ssh/releases/latest/download/rust-ssh-relay-linux-x86_64
sudo chmod 0755 /usr/local/bin/rust-ssh
sudo curl -L --fail -o /etc/systemd/system/rust-ssh-relay.service https://raw.githubusercontent.com/Ameeeeeeeeeeeeee/rust-ssh/main/examples/rust-ssh-relay.service
```

创建服务账户、目录和服务器身份：

```bash
sudo useradd --system --no-create-home --shell /usr/sbin/nologin rustssh 2>/dev/null || true
sudo install -d -o root -g rustssh -m 0750 /etc/rust-ssh
sudo install -d -o root -g rustssh -m 2750 /etc/rust-ssh/devices
sudo /usr/local/bin/rust-ssh keygen --identity-key /etc/rust-ssh/identity.key --public-key /etc/rust-ssh/identity.pub
```

`keygen` 只在第一次部署时运行。以后升级或重启都保留 `identity.key` 和 `identity.pub`，不要重新生成。

只生成一次 controller token：

```bash
sudo test -e /etc/rust-ssh/controller.token || (openssl rand -hex 32 | sudo tee /etc/rust-ssh/controller.token >/dev/null)
```

设置权限：

```bash
sudo chown root:rustssh /etc/rust-ssh/identity.key /etc/rust-ssh/identity.pub /etc/rust-ssh/controller.token /etc/rust-ssh/devices
sudo chmod 0640 /etc/rust-ssh/identity.key /etc/rust-ssh/controller.token
sudo chmod 0644 /etc/rust-ssh/identity.pub
```

写入服务配置。这个配置块需要保留换行，直接整块复制：

```bash
sudo tee /etc/rust-ssh/relay.env >/dev/null <<'EOF'
RUST_SSH_LISTEN=0.0.0.0:24443
RUST_SSH_IDENTITY_KEY=/etc/rust-ssh/identity.key
RUST_SSH_CONTROLLER_TOKEN_FILE=/etc/rust-ssh/controller.token
RUST_SSH_DEVICES_DIR=/etc/rust-ssh/devices
EOF
```

启动 relay：

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now rust-ssh-relay
sudo systemctl status rust-ssh-relay --no-pager
```

在云安全组和 Ubuntu 防火墙中只放行 `TCP 24443`。如果这个端口已经被其他程序占用，就换一个端口，同时修改服务配置并重新生成配置码；不要和 RustDesk 端口复用。

## 2. 部署 Windows client

从[最新 Release](https://github.com/Ameeeeeeeeeeeeee/rust-ssh/releases/latest) 下载并安装 MSI：

```text
rust-ssh-client-windows-x86_64.msi
```

MSI 会打开安装向导，可以选择安装目录，并创建开始菜单入口；Release 不提供单独的 Windows `.exe`。双击开始菜单里的 rust-ssh client 即可。client 自己的配置会保存在所选安装目录下的 `data` 文件夹中。

第一次打开 client 时，界面会显示一串类似下面的设备 ID：

```text
rssh-0123456789abcdef0123456789abcdef
```

点击“复制”，把这串 ID 发给服务器管理员。它是随机生成并保存在本机的，不使用 Windows 计算机名；修改计算机名不会影响连接。client 配置保存在 `<client安装目录>\data\client.json`。

确认 Windows 自带 OpenSSH Server 正常：

```powershell
Get-Service sshd
Test-NetConnection 127.0.0.1 -Port 22
```

没有 `sshd` 时，在管理员 PowerShell 执行：

```powershell
Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0
Start-Service sshd
Set-Service -Name sshd -StartupType Automatic
```

## 3. 在服务器登记这台 client

服务器管理员把刚才得到的 ID 填入变量：

```bash
DEVICE_ID=rssh-0123456789abcdef0123456789abcdef
```

执行：

```bash
sudo /usr/local/bin/rust-ssh device add --device-id "$DEVICE_ID" --server "$SERVER_IP:24443" --server-key /etc/rust-ssh/identity.pub --devices-dir /etc/rust-ssh/devices
```

命令会完成两件事：

1. 在服务器创建 `/etc/rust-ssh/devices/<设备ID>.token`；
2. 输出只属于这台设备的 `rssh1:...` 配置码。

复制输出的整行配置码，粘贴回这台 Windows client 的“配置码”框。点击“保存”→“启动”。状态显示绿色“已连接服务器”后，client 就在等待 SSH 连接；它不会自动开机启动。关闭窗口只会隐藏到右下角托盘，右键托盘图标选择“关闭”才会停止。

relay 会自动读取新的 token，通常等待约 2 秒即可，不需要重启服务。可以在服务器查看注册情况：

```bash
sudo /usr/local/bin/rust-ssh inventory --controller-token-file /etc/rust-ssh/controller.token --devices-dir /etc/rust-ssh/devices
```

## 4. 部署 Mac / Windows connect

在服务器生成主控配置码：

```bash
sudo /usr/local/bin/rust-ssh pair-code --server "$SERVER_IP:24443" --server-key /etc/rust-ssh/identity.pub --token-file /etc/rust-ssh/controller.token
```

这份配置码拥有查看和连接所有已登记设备的权限，只交给可信的主控端。不要粘贴给 client。

从[最新 Release](https://github.com/Ameeeeeeeeeeeeee/rust-ssh/releases/latest) 下载：

```text
macOS Apple Silicon：rust-ssh-connect-macos-aarch64
Windows x86-64：rust-ssh-connect-windows-x86_64.msi
```

macOS 首次运行前执行：

```bash
chmod +x rust-ssh-connect-macos-aarch64
```

Windows 双击 MSI，按向导选择安装目录，然后从开始菜单打开 rust-ssh connect。connect 自己的配置和配置码会保存在所选安装目录下的 `data` 文件夹中。

打开 connect，粘贴主控配置码，填写 Windows 的 OpenSSH 用户名，例如 `windows-user`。依次点击：

```text
刷新设备 → 选择设备 → 配置 SSH
```

之后可以在 Terminal 或 VS Code Remote-SSH 中直接使用 Host 昵称。默认昵称就是设备 ID，示例命令是：

```bash
ssh rssh-0123456789abcdef0123456789abcdef
```

如果想改昵称，在设备列表中右键设备，选择“设置 Host 昵称”，保存后重新点击“配置 SSH”。以后直接执行 `ssh 你设置的昵称` 即可；SSH 配置中的 ProxyCommand 是内部实现，不需要手写。

connect 只负责生成 SSH 配置和启动内部代理，配置完成后可以关闭；Windows client 必须保持托盘运行并显示绿色已连接状态。

## 5. 只要记住这四点

- 服务器暴露 `24443/tcp`，client/connect 都主动连服务器。
- `identity.key` 只留服务器；`identity.pub` 会进入配置码，用来锁定服务器身份。
- 每台 client 有自己的设备 token；controller token 只有一份，是主控总钥匙，不能给 client。
- Windows 设备 ID 由 client 随机生成并持久保存，不依赖、也不跟随计算机名变化。

更完整的权限、升级、加设备和排错说明见[三端部署详细版](deployment-detailed.md)。
