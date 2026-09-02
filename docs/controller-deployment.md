# rust-ssh connect 部署手册

这份手册用于：

```text
Mac/Windows 主控端 connect
```

connect 不直接访问 Windows，而是通过 Ubuntu VPS relay 找到在线的 Windows client。

## 1. 使用 controller token 生成配置码

先确认 relay 已按 [client 部署手册](client-deployment.md) 配置并运行。

在 Ubuntu VPS 上执行：

```bash
sudo /usr/local/bin/rust-ssh pair-code \
  --server 203.0.113.10:24443 \
  --server-key /etc/rust-ssh/identity.pub \
  --token-file /etc/rust-ssh/controller.token
```

把 `203.0.113.10` 换成 VPS 公网 IP。命令输出一整行 `rssh1:...`，这就是 controller 配置码。

controller 配置码包含 controller token。它可以查看和连接所有设备，只交给可信的主控端，不要交给 Windows client，也不要提交到 GitHub。

当前设计中 controller token 只有一份；如果有多台 Mac/Windows 主控端，它们使用同一份 controller 配置码，也拥有相同的全部权限。

## 2. Mac/Windows 安装 connect

从 [v0.3.0 Release](https://github.com/Ameeeeeeeeeeeeee/rust-ssh/releases/tag/v0.3.0) 下载对应版本：

```text
macOS ARM64：rust-ssh-connect-macos-aarch64
Windows x86-64：rust-ssh-connect-windows-x86_64.exe
```

主控端需要系统 OpenSSH：

```bash
ssh -V
```

macOS 下载的可执行文件如不能直接打开，先执行：

```bash
chmod +x rust-ssh-connect-macos-aarch64
```

## 3. 配置 GUI

打开 `rust-ssh-connect`，填写：

```text
配置码：controller 配置码
SSH 用户：Windows 上的登录用户名，例如 ame
```

点击：

```text
刷新设备 → 选择 Windows 设备 → 配置 SSH
```

如果想马上打开终端，点击“连接选中设备”。

## 4. 使用 SSH 和 VS Code

配置完成后，connect 会自动维护用户 SSH 配置：

```text
macOS：~/.ssh/config
Windows：%USERPROFILE%\.ssh\config
```

假设设备 ID 是 `DESKTOP-KH8O1JM`，以后直接使用：

```bash
ssh rust-ssh-DESKTOP-KH8O1JM
```

VS Code Remote-SSH 中选择同名主机即可。

配置完成后 connect GUI 不需要一直打开。SSH 启动时会调用 `rust-ssh-connect` 的内部代理模式；client GUI 仍然必须保持打开。

不要随意移动 `rust-ssh-connect` 可执行文件。移动后需要重新打开 GUI，再点击一次“配置 SSH”。

## 常见问题

- 没有在线设备：确认对应 Windows client 已点击“启动”。
- 配置码无效：connect 必须使用 controller 配置码，不能使用某台 client 的设备配置码。
- `24443` 连接失败：检查 VPS 安全组、防火墙和 relay 状态。
- SSH 用户名错误：填写 Windows OpenSSH 的实际用户名，不要填写 VPS 用户名。
- server key mismatch：重新从当前 VPS 的 `identity.pub` 生成 controller 配置码。
