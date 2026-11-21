# CS2World (`v0.2.4`)

将代理流量伪装为 **Counter-Strike 2 社区服务器（UDP 27015）** 的纯 Rust 传输协议与全平台客户端，同时保留可选的 Go / `sing-box` 生态兼容层。

## 核心特性

- **纯 Rust 独立内核（`cs2world-core`）**：服务端与客户端静态编译、无外部内核依赖、无 GC 抖动。
- **密码学与传输**：1-RTT X25519 PFS 握手 + AES-256-GCM + 64-bit 滑动窗口防重放 + KCP/FEC + smux 多路复用。
- **真实 CS2 对局流量校准**：基于真实 CS2 对局抓包提取的经验分布画像（`configs/cs2_profile.json`），严格遵守 Valve SDR `1257B` MTU 上限、双向 64Hz 心跳，消除定长尖峰与下载期上行 ACK 放大。
- **双层主动探测防御**：
  - 响应标准 Steam `A2S_INFO` / `A2S_PLAYER` / `A2S_RULES` 查询（AppID `730`，在线人数与地图动态轮换）；
  - 收到真实 CS2 客户端 `connect IP:27015` 探测时，回复标准 512B 填充 Valve GNS `0x24` 帧（`reason_code = 1135`），游戏客户端直接弹出原生 **「服务器已满」**。

## 服务端一键部署 / 平滑升级（Linux VPS）

支持 Ubuntu / Debian / CentOS / Rocky（`amd64` / `arm64`），若已有旧版 Go 配置会自动继承原端口、PSK 与房间名：

```bash
curl -fsSL https://raw.githubusercontent.com/jantian3n/cs2world/main/scripts/cs2world-deploy-v0.2.0.sh | sudo bash -s -- upgrade
```

部署完成后随时输入 `cs2world` 打开管理面板（查看 `cs2world://` 分享链接、服务状态/日志、A2S + GNS `0x24` 双项自检、重启、卸载）。

## 项目结构与客户端

| 模块 | 目录 | 说明 |
|---|---|---|
| **协议与核心** | `crates/cs2world-proto`, `crates/cs2world-core` | 协议实现、Mixed (SOCKS5/HTTP)、路由分流与跨平台 TUN |
| **Linux / Windows 服务端** | `crates/cs2world-server` | 纯 Rust 单文件服务端（支持 `SIGHUP` 热重载） |
| **Windows 客户端** | `win32-xp/` | 纯 Rust + 原生 Win32 API（Windows XP 经典风格，内嵌核心单文件 EXE） |
| **macOS / Android 客户端** | `clients/macos/`, `clients/android/`, `crates/cs2world-ffi` | 原生 Swift / Kotlin 客户端与 C/JNI 绑定 |
| **Go / sing-box 兼容层（可选）** | `pkg/`, `cmd/sing-box-cs2world/` | 与 Rust 主线线协议 100% 互通，可按需跟随上游 `sing-box` / `v2rayN` 更新 |

## 快速构建与测试

```bash
# 运行全部 Rust 单元测试
cargo test --workspace

# 编译单文件 Windows XP 客户端与服务端
cargo build --release -p cs2world-xp -p cs2world-server

# 真实 CS2 与 CS2World 流量 A/B DPI 体检对比
python3 scripts/compare_cs2_pcap.py cs2-real.pcapng cs2world-test.pcapng
```

更多跨平台打包与验证细节见 [docs/rust-core.md](docs/rust-core.md) 与 [docs/rust-release.md](docs/rust-release.md)。
