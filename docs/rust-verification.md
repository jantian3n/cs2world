# Rust 核心交付验证

线协议对照为 Go 提交 `ea7b5e100d898c8a67a25babe2be608652fbcc89`。互通夹具使用真实 UDP 会话和 Mixed 入口，测试目标为回环 HTTP、TLS、TCP 和 UDP 服务。Windows 实测节点核对单独运行，记录中省略节点地址与密钥。

| 项目 | 实际验证 |
| --- | --- |
| 协议 | 19 项协议测试覆盖握手、AEAD 两方向固定帧、重放窗口、FEC 校验分片、KCP 重传和窗口、smux/UoT、A2S、整形器及分享链接。协议库通过 `thumbv7em-none-eabi` 的无标准库检查。 |
| Go ↔ Rust | Rust 客户端连 Go 服务端、Go 客户端连 Rust 服务端，在 Windows 上每个方向各测 FEC 关闭、10+3 和非默认的 4+1，六种组合均通过。通过有损 UDP 中继，验证 HTTP、真实 TLS CONNECT、并发 TCP 半关闭、256 KiB 内容、固定与多目标 UDP、来源地址和逆序回包，以及停服后的失败对照。 |
| 流量整形 | 配置 64 Hz 后，Go 和 Rust 客户端的加密空闲帧间隔中位数均约 46 ms。关闭整形时，两端在该测试窗口中均只发送真实报文。Go 的 RULES 行为按挑战响应核对。 |
| Mixed 与路由 | 复用 HTTP 连接切换目标、分块 POST、CONNECT 后合并的首批数据、SOCKS UDP ASSOCIATE、域名边界、CIDR、私有地址与 IPv4 映射地址均有回归覆盖。 |
| Linux | x86_64 和 aarch64 产物均为 ELF64，未发现 PT_INTERP 或 DT_NEEDED。x86_64 在 WSL2 原生运行，arm64 在 QEMU 用户态运行，每个交付架构各通过四组与 Go 的实际互通。 |
| Windows | 发布 EXE 在只有一个程序文件的目录启动，读入旧 state，通过 Go 回环和已选真实节点。实测包括 A2S、认证、系统证书校验的 HTTPS CONNECT 和 SOCKS5 UDP DNS。消息响应用 Win32 消息往返计时；系统代理写入和还原采用隔离夹具。 |
| Android | 最终调试 APK 在 Android 15 / API 35 x86_64 模拟器安装并通过 JNI 生命周期测试。独立 shell UID 2000 发出的 TCP、UDP 经过系统 VpnService、实际 TUN fd、Rust 混合入站、加密隧道和 Go 节点，两侧逐字节校验回显。arm64 JNI 库已构建并随包分发。 |
| macOS 与 FFI | 交叉构建的通用 CLI 包含 arm64 和 x86_64 两个可执行切片，最低版本为 macOS 12。逐页校验两份 ad-hoc 签名，两个切片均只依赖系统 libSystem。XCFramework 的两份静态库均检查了 Mach-O 对象及全部 12 个 C ABI 导出。Windows 的独立 MSVC C 程序实际链接 FFI 库并测试启动、统计、停止、销毁及回调排空。 |
| TUN | 共用 IP 栈通过原始 TCP 握手与载荷、IPv4/IPv6 UDP 往返、主机名提示与 DNS 关联隔离测试。Windows 和 macOS 提供仅捕获一个保留 /32 的特权设备用例。 |

Windows 最终成品连续三轮在可见窗口测量，使用独立进程向 GUI 发送 WM_NULL。3 次 256 KiB 下载各耗时约 2.54 秒；下载期间各轮消息 p99 为 0.18、0.19 和 0.21 ms，最大值为 0.85 ms。握手超时阶段各轮 p99 为 0.22、0.24 和 0.18 ms，最大值为 0.97 ms。空状态启动到窗口可交互约 186–196 ms。早期夹具有一次在 97 个样本中记录到 198 ms，无法仅凭该样本确定原因。最终夹具分别计时窗口启动和实际下载，门槛维持 p99 ≤100 ms、单次 ≤250 ms。该测量描述消息派发延迟，不能换算为屏幕帧率。

Linux x86_64 在一轮 WSL2 回环负载中，服务端空闲 RSS 为 1,396 KiB，8 条流时为 1,856 KiB。测试启用 FEC 10+3，并在双方向每 101 个数据包中连续丢弃 5 个。该次双向业务载荷合计吞吐为 7.85 MiB/s。这些数字描述该次运行环境和负载。

当前 Windows 会话没有管理员令牌，原生 Wintun 适配器与 SCM 服务的系统级启动、停止留给具备权限的 Windows 运行器。本机已检查 macOS CLI 的文件结构和签名页。CLI 的系统加载、AppKit 的链接与窗口启动、utun 和系统签名校验仍需 macOS 运行器。Android 的实际设备覆盖为 x86_64 模拟器，arm64 手机及蜂窝切网待验。仓库内的 CI 工作流已提供相应构建和主机测试入口。发布包中的 `android/device-test.json` 可用于核对 Android 设备结果和产物哈希。
