# Changelog

## v0.5.5 — 2026-09-06

修复 VS Code Remote-SSH 在发送启动脚本时可能卡住、连接随后失败的问题。

- 修复 Noise 加密帧分段接收：密文尚未收齐时保留帧头和正文读取进度，避免把剩余密文误读为下一帧的长度并报 `invalid encrypted relay frame length`。
- 新增分段传输回归测试，覆盖 1、4、5、17、1024 字节缓冲区、多个加密帧和输入结束，验证较大数据传输仍能完整返回。

### 升级说明

server、Windows Client、macOS/Windows Connect 共用这段传输实现，请将三端都升级到 v0.5.5。协议和配置码格式没有变化，现有身份密钥、token、设备 ID 和 SSH 配置可继续使用。升级并重启程序时，已有 SSH 会话需要重新连接。
