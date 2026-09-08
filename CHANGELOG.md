# Changelog

## v0.5.6 — 2026-09-08

修复控制通道静默失效导致"已有会话仍在传输、新 SSH 连接全部失败"的问题，并补齐连接建立阶段的安全期限与秘密文件权限。

### 控制通道心跳与恢复

- Agent 控制通道新增 Ping/Pong 心跳（默认每 15 秒探测，45 秒无响应判定失效），Agent 和 Server 双端都能发现对端不可达。心跳只用于控制通道，不会进入 SSH 数据连接。
- 通过 Hello/HelloOk 中的 `features` 字段做能力协商：旧版本端不声明 `heartbeat` 就完全不发送心跳，v0.5.6 与旧版本可以互相连接；只有两端都升级到 v0.5.6 后心跳才生效。
- Agent 控制通道失效后自动重连，**已经建立的 SSH 会话不再随控制通道一起断开**；重连成功后新会话恢复。旧控制连接上尚未建立的请求会及时失败清理，不影响新连接的注册和会话。
- 控制通道的读取改由独立 reader task 完成，通过有界 channel 交付完整帧：修复了 `read_frame` 不具备取消安全性、会话结束与半帧读取交错时把正文当长度导致 `invalid control frame length` 的问题。
- 心跳与控制帧的写入都带超时，TCP 黑洞不会再让 writer 永久阻塞；Server 端所有控制帧经单一 writer task 发出，Open 与 Pong 不会交错。

### 建连阶段绝对期限

- Server 对首个 Hello、SessionAttach、Ready、Controller 首请求全部增加绝对期限；完成 Noise 握手但从不发 Hello 的连接不再无限期占用全局 128 个连接名额。
- Controller 的 Open 请求总期限（15 秒）现在覆盖排队、写控制消息和等待 Agent 响应三个阶段，不再只覆盖最后一段。
- Agent 会话数据流转交 Controller 时连接名额随流一起移交，配额统计与实际存活连接一致。
- Connect 的认证、设备列表、会话建立都增加明确超时；GUI 刷新超时后自动恢复可刷新状态，退出时取消后台刷新。
- 长时间 SSH 数据传输不受任何固定时长限制。

### 秘密文件权限与配置迁移（Windows）

- Windows GUI 配置目录从安装目录的共享 `data` 文件夹（本机 Users 组可读写）迁移到当前用户的 `%LOCALAPPDATA%\rust-ssh`；MSI 不再创建共享 data 目录。
- 升级后首次启动自动迁移旧 `client.json`、`connect.json` 和 `connect.setup`；已生成的 SSH ProxyCommand 里旧的 `--setup-code-file` 路径会被自动改写到新位置，无需重新点击"配置 SSH"。旧文件留在原地，不影响共用电脑上其他账户的配置。
- Unix/macOS 下配置目录 0700、含配对码的 JSON/setup 文件在创建时即为 0600（临时文件 + 原子替换）；`keygen` 生成的私钥默认 0600。SSH 用户名写入 SSH config 前拒绝空白与控制字符。

### 依赖与 CI

- 移除未使用的 webbrowser 直接依赖；webbrowser 经 egui-winit 升级到 1.2.2（修复 RUSTSEC-2026-0257），因此最低 Rust 版本由 1.82 提高到 1.85。
- CI 增加 Windows 与 macOS 的 desktop 全量测试、cargo-audit 依赖审计；Release 构建增加测试门禁。audit 忽略项与理由记录在 `.cargo/audit.toml`。

### 升级说明

- 三端（server、Windows Client、Mac/Windows Connect）都要升级到 v0.5.6，心跳修复才会完全生效；混用旧版本不影响连通性，但旧端控制通道失效检测行为与 v0.5.5 相同。
- 建议升级顺序：先 server，再 Colorful 上的 Client，最后本机 Connect。**重启 server 会中断所有现有 SSH 会话**（包括正在下载的会话），请避开重要传输窗口；Client/Connect 会在 server 恢复后自动重连。
- 协议版本仍是 v4，配对码、身份密钥、token、设备 ID 和 SSH 配置均可继续使用。Windows 配置目录位置变化如上，升级后不需要重新配置。

## v0.5.5 — 2026-09-06

修复 VS Code Remote-SSH 在发送启动脚本时可能卡住、连接随后失败的问题。

- 修复 Noise 加密帧分段接收：密文尚未收齐时保留帧头和正文读取进度，避免把剩余密文误读为下一帧的长度并报 `invalid encrypted relay frame length`。
- 新增分段传输回归测试，覆盖 1、4、5、17、1024 字节缓冲区、多个加密帧和输入结束，验证较大数据传输仍能完整返回。

### 升级说明

server、Windows Client、macOS/Windows Connect 共用这段传输实现，请将三端都升级到 v0.5.5。协议和配置码格式没有变化，现有身份密钥、token、设备 ID 和 SSH 配置可继续使用。升级并重启程序时，已有 SSH 会话需要重新连接。
