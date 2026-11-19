# CS2World Windows v2rayN 适配版使用指南

本目录 (`dist-v2rayn-win`) 包含针对 **Windows 版 v2rayN** 修复并重新编译的 `sing-box` 内核（`sing-box.exe`）与配套配置模板、安装脚本及本地端到端测试工具。

---

## 🔧 相比上游原版 (`jantian3n/cs2world` Release v0.0.1) 的修复内容

1. **修复 v2rayN 启动内核即报错退出的问题 (`cmd/sing-box-cs2world/main.go`)**
   - v2rayN 拉起 `sing-box` 时固定使用命令：`sing-box.exe run -c config.json --disable-color`。
   - 上游原版 CLI 使用 `flag.NewFlagSet` 且未定义 `--disable-color`，导致在 v2rayN 中直接报错 `flag provided but not defined: -disable-color` 并秒退。
   - 本适配版已支持 `--disable-color`、`--config` / `-c`（可重复合并）、`--config-directory` / `-C`、`--directory` / `-D`、任意顺序参数以及标准 `version` / `version -n` 输出格式，完整兼容 v2rayN。
2. **包含上游最新 TUN 防回环提交并强化 Windows UDP / DNS / 网络切换处理**
   - 基于包含 `3a1f498`（`dialer.ListenPacket` 绕过本地 TUN）的最新代码，并额外修复：
     - 禁用 Windows UDP Socket 的 `SIO_UDP_CONNRESET` 并忽略瞬时 `WSAECONNRESET (10054)` / `WSAEMSGSIZE (10040)` 错误，防止远端 ICMP 不可达或本地探测包意外中断 KCP 隧道（`pkg/transport/udp_windows.go`）。
     - `outbound` 域名解析接入 `sing-box` 内置 `adapter.DNSRouter`，解析出 IP 后再交给 `kcp-go` 与 `dialer.ListenPacket`；同时实现 `adapter.InterfaceUpdateListener`，在开启/关闭 TUN 或切换网络接口时自动重置隧道（`pkg/cs2world/outbound.go`）。
3. **补全 v2rayN 常用编译特性标签**
   - 使用 `with_gvisor,with_quic,with_dhcp,with_wireguard,with_utls,with_acme,with_clash_api,with_tailscale` 编译，确保替换内核后 v2rayN 的 TUN（gVisor）、Clash API 流量统计、WireGuard/QUIC/uTLS 等其他节点功能全部正常。

---

## 📦 安装内核到 v2rayN

### 方式一：使用一键安装脚本（推荐）

在 PowerShell 中运行（可只传 `-V2rayNDir` 交互检测，也可以顺便传入服务器参数直接生成配置）：

```powershell
# 仅安装内核（自动备份原有 sing-box.exe 为 sing-box.exe.bak）
.\install-to-v2rayn.ps1 -V2rayNDir "你的v2rayN目录"

# 推荐：安装为 sing-box-client.exe（v2rayN 会优先调用它，且更新内核时不会被覆盖）
.\install-to-v2rayn.ps1 -V2rayNDir "你的v2rayN目录" -UseClientName

# 一步到位：安装内核 + 自动生成并校验可导入的配置文件
.\install-to-v2rayn.ps1 -V2rayNDir "你的v2rayN目录" -UseClientName -Server "你的VPS_IP" -Port 27015 -PSK "你的PSK"
```

### 方式二：手动替换

1. 在 v2rayN 中先断开代理并退出 v2rayN。
2. 打开 v2rayN 目录下的 `bin\sing_box\`（若你是 Microsoft Store / MSIX 打包版或只读目录，可能在 `%LOCALAPPDATA%\v2rayN\bin\sing_box\`）。
3. 将原 `sing-box.exe` 备份为 `sing-box.exe.bak`。
4. 将本目录下的 `sing-box.exe` 复制到 `bin\sing_box\sing-box.exe`（或者重命名为 `sing-box-client.exe` 防止 v2rayN 自动更新覆盖）。

---

## 🚀 在 v2rayN 中导入测试节点

### 方法 A：v2rayN 7.x「添加自定义出站」（推荐，保留分流/DNS/统计/测速）

适合希望继续使用 v2rayN 自带绕过大陆、FakeIP、DNS、TUN 及实时流量统计的用户：

1. 用文本编辑器打开 `v2rayn-outbound-cs2world.json`（或脚本生成的 `my-cs2world-outbound.json`），填入你的 `server`、`server_port`、`psk`。
2. 打开 v2rayN -> **`服务器 (Servers)`** -> **`添加自定义配置服务器`** -> 类型选 **`Outbound`**（或 **`添加自定义出站`**）。
3. 选择该 JSON 文件，Core 类型保持 **`sing_box`**，保存。
4. 选中该节点设为活动，按需开启系统代理或 TUN 模式即可。

### 方法 B：v2rayN 6.x / 7.x「添加自定义配置服务器」（通用完整配置）

适合直接使用 VPS 上 `cs2world client` 导出的完整 `client.json`：

1. 编辑 `v2rayn-custom-config-client.json`（或直接使用 VPS 导出的 `client.json` / 脚本生成的 `my-cs2world-client.json`），确认 `server`、`server_port`、`psk` 正确。
2. 打开 v2rayN -> **`服务器 (Servers)`** -> **`添加自定义配置服务器 (Add custom config server)`**。
3. **配置文件**：选择该 `.json` 文件。
4. **Core 类型**：选择 **`sing_box`**。
5. **本地监听端口 (Socks Port)**：填写 **`7890`**（必须与配置中 `mixed-in` 的 `listen_port` 一致）。
6. 设为活动服务器并开启系统代理或 TUN 模式。

---

## 🛡️ Windows TUN 模式注意事项

1. 在 Windows 上开启 v2rayN 的 **TUN 模式** 时，请**以管理员身份运行 v2rayN**。
2. 若使用方法 B 的完整自定义配置，请确保配置中的 `route` 包含 `"auto_detect_interface": true`（本目录模板已默认开启），否则 `cs2world` 的 UDP 流量可能会被 TUN 虚拟网卡回环劫持。
3. 本内核已内置 `with_gvisor` 与 `sing-tun` 支持，并实现了接口变化监听（`InterfaceUpdateListener`），切换网络或开关 TUN 时会自动重建加密会话。

---

## 🧪 无 VPS 本地回环自检

在安装到 v2rayN 前，可先在本机直接运行完整端到端自检（包含 `version`、`--disable-color`、`A2S_INFO`、非法探针拒接、256KB HTTP 代理传输及 Clash API）：

```powershell
python .\test-local-v2rayn.py
```

若 7 项测试全部输出 `[PASS]` 与 `ALL 7 WINDOWS v2rayN E2E TESTS PASSED!`，即可放心导入 v2rayN 测试。
