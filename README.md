# sing-box-cs2world: 原生内置 `cs2world` (CS2 流量拟态) 协议的 sing-box 内核

`sing-box-cs2world` 基于官方 **`sing-box v1.14.4`** 构建，100% 保留官方全部协议与功能（VLESS、Reality、Hysteria2、TUN、DNS、Rule-Set、Clash API），并**原生注册了全新的 `"type": "cs2world"` Inbound 与 Outbound 协议**（同时支持 TCP 与 UDP-over-Stream）。

不再需要任何外挂端口转发：服务端只需单进程运行 `sing-box-cs2world`，客户端直接替换 v2rayN / Nekoray 的 `sing-box` 核心或直接运行即可！

## 核心能力

1. **原生协议类型 `"type": "cs2world"` (`pkg/cs2world/`)**
   - 直接在 `sing-box` 配置文件的 `inbounds` 和 `outbounds` 中原生使用 `"type": "cs2world"`，无缝接入路由分流、TUN 网卡、测速与 Clash API。
2. **Valve `GameNetworkingSockets` (GNS) 字节级封装 (`pkg/protocol/gns.go`)**
   - 9 字节明文外层头（`Flags=0x82` + `32-bit ConnID` + `32-bit Seq`）作为 AEAD AAD + **AES-256-GCM** 全加密内部帧。
3. **双层主动探测防御 (`pkg/a2s/responder.go`)**
   - 陌生扫描器发送 Steam `A2S_INFO` / `A2S_PLAYER` 查询时，返回逼真的 Counter-Strike 2（AppID `730`）服务器信息（动态在线人数 + 30 分钟自动轮换地图）。
   - 陌生客户端或主动探测器发来非 A2S UDP 包时，回复标准 Valve GNS `0x02 ConnectionClosed` (`"Server is full (20/20). VAC authentication required."`)。
4. **AI 64-Tick 行为状态机 (`pkg/shaper/state_machine.go`)**
   - 自动在 `Idle`（~21Hz 72±10B 观战心跳）、`Combat`（64-Tick 高斯分布 Padding）、`FastDL`（1248B 地图资源分片）和 `Sleep`（闲置 10 分钟零流量休眠）间切换。
5. **KCP + Reed-Solomon FEC + smux 多路复用**
   - 默认 `10:3` 前向纠错，跨国晚高峰 UDP 丢包 15% 内依然流畅。

## 快速编译与验证

```bash
cd cs2-ghost

# 编译本机版本并运行全链路测试（A2S 探测 + 陌生探针拒接 + 原生 sing-box HTTP 代理 256KB 传输）
make test-cs2world

# 一键编译 macOS + Linux amd64 + Windows amd64 三平台内核
make build-all
```

## 单进程部署指南

### 1. 海外 VPS 服务端
1. 上传 `bin/sing-box-cs2world-linux-amd64` 到 VPS（如 `/usr/local/bin/sing-box-cs2world`），放行防火墙 **UDP `27015`**。
2. 编辑 `configs/cs2world_server.json`，将 `psk` 修改为你的高强度随机密码。
3. 直接启动（或配置 systemd）：
   ```bash
   /usr/local/bin/sing-box-cs2world run -c /etc/cs2world/cs2world_server.json
   ```

### 2. 客户端使用（两种方式任选）
* **方式 A：命令行 / TUN 模式直接运行**
  修改 `configs/cs2world_client.json` 中的 `server` 为你的 VPS IP、填入相同 `psk`，运行：
  ```bash
  ./bin/sing-box-cs2world run -c configs/cs2world_client.json
  ```
* **方式 B：在 v2rayN (Windows/macOS) 中作为原生核心使用**
  1. 将 `bin/sing-box-cs2world-windows-amd64.exe` 重命名为 `sing-box.exe`，替换 v2rayN 的 `bin/sing_box/sing-box.exe`。
  2. 在 v2rayN 中选择「服务器 -> 添加自定义配置服务器」，导入修改好 IP 和 `psk` 的 `configs/cs2world_client.json`，内核选择 `sing-box` 即可原生使用！
