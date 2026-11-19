# CS2World (`sing-box-cs2world` v0.0.1)

**CS2World** 是基于官方 **`sing-box v1.14.4`** 深度扩展的自定义协议内核，100% 保留官方全部协议与特性（VLESS、Reality、Hysteria2、TUN、DNS、Rule-Set、Clash API），并**原生内置了全新的 `"type": "cs2world"` 深度游戏流量拟态协议**（将跨国代理流量在字节级与统计分布级完整伪装为 **Valve Counter-Strike 2 社区私服 UDP 27015 对局流量**）。

---

## ✨ 核心技术架构 (`v0.0.1`)

1. **原生 `sing-box` 协议支持 (`"type": "cs2world"`)**
   - 无需外挂端口转发器，直接在 `sing-box` 的 `inbounds` 和 `outbounds` 中配置 `"type": "cs2world"`，同时支持 **TCP** 与 **UDP-over-TCP (UoT)** 全流量代理。
   - 客户端底层对接 `sing-box` 原生 `dialer.ListenPacket`，完美支持 `auto_detect_interface` 与 `bind_interface`（避免本地 TUN 虚拟网卡回环劫持）。
2. **1-RTT X25519 完全前向保密握手 (PFS) + AES-256-GCM**
   - 完整复刻 Valve `GameNetworkingSockets` (GNS) 进服握手生命周期：
     - `0x01 ConnectRequest (148B)`：携带客户端临时 X25519 公钥 + `±60s` 防重放时间戳 + HMAC-SHA256 零知识认证；
     - `0x03 ConnectOK (92B)`：服务端生成临时 X25519 公钥并分配 `32-bit SessionConnID`，通过 HKDF 派生出**单次会话专属的 AES-256-GCM 密钥与双向独立 Nonce Salt**，握手后立即销毁临时私钥。
     - `0x82 Data`：9 字节标准 Valve GNS 明文头作为 AEAD 的附加认证数据（AAD），防止任何中间人篡改。
3. **64-bit 滑动窗口防重放攻击 (Anti-Replay)**
   - 采用与 WireGuard / IPSec 相同的 RFC 6479 64-bit 位图滑动窗口，严格执行“先 AEAD 验签、再更新防重放窗口”，任何被截获重放的历史数据包均被静默丢弃。
4. **真假美猴王：双层主动探测防御 (`pkg/a2s/`)**
   - **Steam A2S 查询防御**：收到 `0xFFFFFFFF` 开头的 `A2S_INFO` / `A2S_PLAYER` 探测包时，直接返回合法的 **Counter-Strike 2 (AppID `730`)** 房间信息（在线人数按北京时间正弦曲线动态波动，地图每 30 分钟在 `de_mirage` / `de_inferno` / `de_dust2` / `de_ancient` 间自动轮换）。
   - **陌生 GNS 连接防御**：任何未通过 PSK 校验的探测包或真实 CS2 游戏客户端连接，服务端均返回标准 Valve GNS `0x02 ConnectionClosed` 控制包（`"Server is full (20/20). VAC authentication required."`）。
5. **AI 64-Tick 行为状态机与双峰高斯整形 (`pkg/shaper/`)**
   - **Idle（观战心跳）**：无流量时维持 `~21Hz` 的 `72±10B` 拟态同步包，消除传统代理的“静默断崖”；闲置满 10 分钟自动进入 `Sleep` 零流量休眠。
   - **Combat（64-Tick 对局交火）**：日常网页浏览请求按高斯分布（上行 `180±38B` / 下行 `240±38B`）动态填充随机噪声，抹平 TLS-in-TLS 握手指纹。
   - **FastDL（创意工坊地图下载）**：观看 4K 视频或大文件下载时，自动切换至 `1248B` Source 引擎标准资源分片通道。
6. **KCP + Reed-Solomon FEC 前向纠错 + smux 多路复用**
   - 默认开启 `10:3` 前向纠错与 64-Tick (15ms) 快速重传，在跨国晚高峰 15% UDP 丢包环境下依然保持低延迟高吞吐。

---

## 🚀 一、 服务端一键全自动部署（Linux VPS）

无需提前安装 Go 或上传任何文件，在任意 Linux VPS（Ubuntu / Debian / CentOS / Rocky，支持 `amd64` 与 `arm64`）上以 `root` 运行：

```bash
bash <(curl -fsSL https://raw.githubusercontent.com/jantian3n/cs2world/main/scripts/cs2world-deploy-v0.0.1.sh)
```

脚本将自动完成：
- 若 VPS 内存 `<2GB`，自动挂载 `2GB` 临时 Swap 防止编译 OOM（编译后自动销毁）；
- 自动下载隔离的 Go 编译环境、拉取本仓库最新源码并现场编译安装 `sing-box-cs2world`（编译缓存自动清理）；
- 自动调优 Linux 内核 16MB UDP 缓冲区与 BBR、自动放行 `UFW` / `Firewalld` / `iptables` (适配 Oracle Cloud) 的 UDP 端口（默认 `27015`）；
- 自动配置 `systemd` 守护服务、导出即用型 `client.json` 并当场运行 **Valve A2S_INFO 与非法探针防伪自检**。

部署完成后，可在 VPS 任意位置输入 **`cs2world`** 打开管理菜单，或使用快捷子命令：
```bash
cs2world status     # 查看服务状态与监听信息
cs2world client     # 查看/导出客户端 client.json 配置
cs2world probe      # 执行本机 Valve A2S_INFO 与主动探测防伪自检
cs2world modify     # 免重装快捷修改端口 / 轮换 PSK / 房间名 / AI 整形开关
cs2world log        # 查看最近 80 行运行日志
cs2world uninstall  # 彻底卸载并清理
```

---

## 🛠️ 二、 从源码自行编译内核 (Build Instructions)

如果你希望在本地电脑（macOS / Linux / Windows）自行编译 `sing-box-cs2world` 客户端或服务端内核，请按以下步骤操作：

### 1. 编译环境要求
- **Go 版本**：`Go >= 1.24`（推荐 `Go 1.25+`，可从 [go.dev/dl](https://go.dev/dl/) 下载）
- **Git** 与 **Make**（可选，也可直接使用下方原生 `go build` 命令）

### 2. 克隆仓库并拉取依赖
```bash
git clone https://github.com/jantian3n/cs2world.git
cd cs2world
go mod download
```

### 3. 使用 `Makefile` 一键编译（推荐）
```bash
# 1) 编译当前机器系统的原生内核 (-> bin/sing-box-cs2world)
make build-cs2world

# 2) 编译并运行完整密码学单元测试 + 端到端全链路测试
make test-cs2world

# 3) 按需交叉编译指定平台内核：
make build-darwin-arm64       # macOS Apple Silicon (M1/M2/M3/M4) -> bin/sing-box-cs2world-darwin-arm64
make build-darwin-amd64       # macOS Intel (x86_64)              -> bin/sing-box-cs2world-darwin-amd64
make build-cs2world-linux     # Linux x86_64 (VPS 服务端)         -> bin/sing-box-cs2world-linux-amd64
make build-cs2world-linux-arm64 # Linux ARM64 (Oracle ARM/软路由) -> bin/sing-box-cs2world-linux-arm64
make build-cs2world-win       # Windows x86_64 (v2rayN 核心)      -> bin/sing-box-cs2world-windows-amd64.exe

# 4) 一次性编译全部 5 大平台二进制
make build-all
```

### 4. 不使用 `make` 的原生 `go build` 命令（Windows PowerShell / 任意终端通用）
所有平台均使用纯 Go 静态编译（`CGO_ENABLED=0`），内置 `with_gvisor,with_quic,with_utls,with_clash_api` 完整特性标签：

```bash
# macOS Apple Silicon (M 芯片)
CGO_ENABLED=0 GOOS=darwin GOARCH=arm64 go build -tags "with_gvisor,with_quic,with_utls,with_clash_api" -trimpath -ldflags="-s -w -X 'github.com/sagernet/sing-box/constant.Version=1.14.4-cs2world-0.0.1'" -o bin/sing-box-cs2world-darwin-arm64 ./cmd/sing-box-cs2world

# Windows x86_64 (.exe)
CGO_ENABLED=0 GOOS=windows GOARCH=amd64 go build -tags "with_gvisor,with_quic,with_utls,with_clash_api" -trimpath -ldflags="-s -w -X 'github.com/sagernet/sing-box/constant.Version=1.14.4-cs2world-0.0.1'" -o bin/sing-box-cs2world-windows-amd64.exe ./cmd/sing-box-cs2world

# Linux x86_64
CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -tags "with_gvisor,with_quic,with_utls,with_clash_api" -trimpath -ldflags="-s -w -X 'github.com/sagernet/sing-box/constant.Version=1.14.4-cs2world-0.0.1'" -o bin/sing-box-cs2world-linux-amd64 ./cmd/sing-box-cs2world
```

编译完成后可验证版本信息：
```bash
./bin/sing-box-cs2world version
# 输出:
# sing-box version 1.14.4-cs2world-0.0.1 (cs2world v0.0.1)
# Protocols: all official sing-box protocols + cs2world (X25519 PFS + 64-bit Anti-Replay + Valve GNS + A2S Defense + AI 64-Tick Shaper)
```

---

## 💻 三、 客户端使用指南（macOS / Windows / Linux）

在 VPS 上运行 `cs2world client` 获取自动生成的 `client.json` 配置后，任选以下方式连接：

### 方式 1：在 v2rayN (macOS / Windows) 中作为原生内核使用（推荐 GUI 方式）

1. **替换 v2rayN 的 `sing-box` 内核文件**：
   - **macOS (Apple Silicon / Intel)**：
     ```bash
     mkdir -p "$HOME/Library/Application Support/v2rayN/bin/sing_box"
     cp -f ./bin/sing-box-cs2world "$HOME/Library/Application Support/v2rayN/bin/sing_box/sing-box"
     chmod +x "$HOME/Library/Application Support/v2rayN/bin/sing_box/sing-box"
     ```
   - **Windows**：
     将编译或从 Releases 下载的 `sing-box-cs2world-windows-amd64.exe` 重命名为 `sing-box.exe`，覆盖放入 v2rayN 目录下的 `bin\sing_box\sing-box.exe`。
2. **导入自定义配置**：
   - 将 VPS 上的 `client.json` 保存到本地；
   - 打开 v2rayN -> **`服务器 (Servers)`** -> **`添加自定义配置服务器 (Add custom config server)`**；
   - **配置文件**：选择保存的 `client.json`；
   - **Core 类型**：选择 **`sing_box`**；
   - **本地监听端口 (Socks Port)**：填 **`7890`**（与 `client.json` 中 `mixed-in` 的 `listen_port` 保持一致）；
   - 设为活动服务器并开启系统代理或 TUN 模式即可！

### 方式 2：命令行直接运行（无需任何第三方客户端）

```bash
# 1. 校验配置文件语法
./bin/sing-box-cs2world check -c client.json

# 2. 启动客户端（默认在 127.0.0.1:7890 提供 HTTP + SOCKS5 混合代理）
./bin/sing-box-cs2world run -c client.json
```

新开一个终端测试代理连通性与出口 IP：
```bash
curl --noproxy "*" -x http://127.0.0.1:7890 https://api.ipify.org
curl --noproxy "*" -x http://127.0.0.1:7890 -I https://www.google.com
```

### 方式 3：从本地电脑远程验证 VPS 的 CS2 `A2S_INFO` 伪装响应

在本地电脑（需先关闭其它代理软件的 TUN 模式，或绑定物理网卡）运行以下 Python 命令，即可模拟 Steam 客户端向你的 VPS 发送真实 `A2S_INFO` 服务器查询：

```bash
python3 -c 'import socket; s=socket.socket(2,2); s.settimeout(3); s.sendto(b"\xff\xff\xff\xffTSource Engine Query\x00",("你的VPS_IP",27015)); print(s.recvfrom(2048)[0])'
```
返回包含 `Counter-Strike 2`、`de_mirage`、`1.40.4.2` 的响应即代表伪装与 UDP 端口完全正常。

---

## 📂 四、 仓库目录结构

```text
.
├── cmd/
│   ├── sing-box-cs2world/main.go   # 原生集成 cs2world 协议的完整 sing-box v1.14.4 内核入口
│   └── cs2-ghost/main.go           # 独立轻量级端口转发器入口（可选）
├── pkg/
│   ├── cs2world/                   # sing-box 原生 Inbound / Outbound / Options 协议注册适配层
│   ├── protocol/                   # Valve GNS 报文封装、1-RTT X25519 PFS 握手、AES-256-GCM 与 64-bit 防重放窗口
│   ├── a2s/                        # Valve Steam A2S_INFO / A2S_PLAYER / Challenge 主动探测欺骗引擎
│   ├── shaper/                     # AI 64-Tick 行为状态机 (Idle/Combat/FastDL/Sleep) 与双峰高斯包长采样器
│   └── transport/                  # 透明 net.PacketConn 多会话管理与探针分流层
├── configs/
│   ├── cs2world_server.json        # sing-box-cs2world 服务端配置模板
│   └── cs2world_client.json        # sing-box-cs2world 客户端配置模板
├── scripts/
│   ├── cs2world-deploy-v0.0.1.sh   # Linux VPS 全自动源码编译/部署/自检/管理一键脚本
│   └── test_singbox_cs2world.py    # 端到端自动化验证套件 (A2S + 陌生探针拒接 + 256KB 代理完整性校验)
└── Makefile                        # 多平台一键交叉编译与测试脚本
```
