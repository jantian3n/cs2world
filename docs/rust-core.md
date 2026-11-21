# CS2World Rust 核心

Windows 原生客户端、Android VpnService、macOS AppKit 客户端和 Linux 服务端共用同一套 Rust 协议与运行时。旧 Go 内核及其 sing-box 适配器可以与 Rust 客户端或服务端配对。仓库中的 `Cargo.lock` 和互通夹具锁定了本次移植的依赖及 Go 对照版本。

## 使用发行产物

| 目标 | 文件 | 启动方式 |
| --- | --- | --- |
| Windows x64 | `dist/rust/windows/CS2World-XP.exe` | 双击启动，在「节点管理」导入分享链接，然后选择系统代理或 TUN。便携升级时保留 EXE 同目录的 `data`。XP 指界面风格。 |
| Linux x86_64、arm64 | `dist/rust/linux-{x86_64,aarch64}/cs2world-server` | 为文件设置可执行权限，用下方的 `check` 和 `run` 命令加载 JSON。 |
| Android 8.0 及以上 | `dist/rust/android/CS2World-debug.apk` | 安装本地调试签名包，粘贴分享链接或客户端 JSON，点击 Connect 并授予系统 VPN 权限。包内含 arm64-v8a 和 x86_64。 |
| macOS 通用 CLI | `dist/rust/macos/cs2world` | 用 `./cs2world run -c client.json` 启动 Mixed，也可用服务端 JSON 启动监听。 |
| macOS Intel、Apple Silicon | `dist/rust/macos/CS2World-xcframework.zip` | 将 XCFramework 加入 Swift 工程后 `import CS2World`。原生界面和预编译核心在 `CS2World-macOS-build-kit.zip` 中。将其解压到有 Xcode 工具链的 Mac，运行 `bash build-app.sh` 即可离线链接、签名并启动验收通用 .app。 |

### Linux 服务端

从 `configs/cs2world_server.json` 复制一份私有配置，替换示例 PSK，使其与客户端一致。放行配置中的 UDP 端口，默认值为 27015。前台运行和 systemd 安装共用同一个文件格式。

```sh
chmod +x ./cs2world-server
./cs2world-server check -c ./server.json
./cs2world-server run -c ./server.json
```

在目标 Linux 上运行 `sudo bash scripts/deploy-linux.sh install ./cs2world-server ./server.json` 可启用独立的 `cs2world-rust.service`。升级后可用 `sudo bash scripts/deploy-linux.sh rollback` 恢复上一组程序与配置。服务接收 SIGHUP 时会重新读取配置。

### 路由与流量

Mixed 在一个端口接受 SOCKS5 CONNECT、UDP ASSOCIATE、HTTP 绝对 URI 请求和 HTTPS CONNECT。CONNECT 隧道保留应用自身的 TLS 验证。客户端 JSON 可使用 `route.final` 选择默认出站，再以有序的 `route.rules` 补充精确域名、标签边界后缀、CIDR、私网和网络类型规则。以下片段适用于含有 `direct` 和 `cs2world-out` 两个 tag 的客户端配置。

```json
{
  "route": {
    "rules": [
      { "ip_is_private": true, "outbound": "direct" },
      { "domain_suffix": [".cn", ".local", ".lan"], "outbound": "direct" }
    ],
    "final": "cs2world-out"
  }
}
```

TUN 接收原始 IPv4/IPv6 包。TCP 和 UDP 经过 smoltcp 与 Mixed 入站，沿用同一份路由规则。HTTP Host、TLS SNI 和已配对的 DNS 问答可为 IP 流补充域名；多个域名共享同一 IP、缺少可识别主机名时使用 IP 规则。即使命中域名规则，实际连接仍使用应用请求的原始 IP。

Windows 启用 TUN 时需要管理员权限，用于创建适配器和安装路由。EXE 内嵌 Wintun 0.14.1 对应架构的发行 DLL，运行时在受保护目录展开并通过绝对路径加载。macOS 原生 utun 需要配置设备和路由的权限。Android 由系统创建 VPN 文件描述符，Rust 持有独立副本。桌面代理模式只需普通用户权限。

`Service::snapshot` 提供业务字节数、速率、TCP 流和 UDP 关联数，以及含握手、填充和冗余分片的外层 UDP 计数。`subscribe` 和 FFI 回调供原生界面接收状态及日志。探针会分别返回 A2S 房间信息和往返时间。

## 构建和验证

| 工作项 | 仓库根目录下的命令 |
| --- | --- |
| Windows 工作区测试 | `cargo test --workspace --all-features --locked` |
| 协议层无标准库目标 | `rustup target add thumbv7em-none-eabi`，再运行 `cargo check -p cs2world-proto --locked --target thumbv7em-none-eabi` |
| Go ↔ Rust 回归 | `python -B tests/interop/run.py`。需要 Go、Rust、Python、Git 及锁定提交的 Git 对象。 |
| Windows 单文件 | `./scripts/package-windows.ps1`。加 `-IncludeService` 同时生成独立服务程序。 |
| Windows GUI 及互通 | `python -B tests/interop/run.py --windows-gui dist/rust/windows/CS2World-XP.exe` |
| Linux 静态构建 | `bash scripts/package-linux.sh x86_64 aarch64`。脚本检查 ELF 架构、动态解释器和共享库依赖。 |
| Android APK、JNI、测试包 | 安装 Android SDK 35、Build Tools 35、NDK、JDK 17 或以上及 Gradle 8.11.1，运行 `python clients/android/package.py --with-tests --output dist/rust/android`。 |
| macOS 通用客户端 | 在 Mac 上安装 Xcode 和两个 Rust Darwin target，再运行 `bash clients/macos/package.sh`。可通过 `CS2W_SIGN_IDENTITY` 指定代码签名身份。 |
| macOS 命令行客户端交叉构建 | 安装两个 Rust Darwin target，准备 Zig 0.14.1 的 lib 目录和 LLVM lipo，按 `python clients/macos/package-cli.py --help` 指定路径。脚本核对动态依赖、最低版本并逐页验证 ad-hoc 签名。 |
| macOS Rust SDK 交叉构建 | `python clients/macos/package-xcframework.py --help` 提供 Rust 和 LLVM 工具参数。脚本检查两架构 Mach-O 对象和 C ABI 符号。 |
| 桌面原生 TUN 回环 | 在有权限的 Windows 或 Mac 上运行 `cargo test -p cs2world-core --features tun --locked native_device_routes_and_forwards_one_test_address -- --ignored --nocapture`。测试临时捕获一个文档保留的 /32 地址，验证 TCP/UDP 和停止后的路由清理。 |
| 可选 Windows 服务 | `cargo build --release --locked -p cs2world-server --features windows-service`。SCM 服务名为 `CS2WorldCore`，程序参数为 `service -c 配置文件绝对路径`，支持 STOP、SHUTDOWN 和 PARAMCHANGE。 |

Go ↔ Rust 回归覆盖两种方向、FEC 关闭及 10+3 和 4+1 两种冗余配置、丢包、流半关闭、多目的 UDP、A2S 和空闲整形器。Go 基线接收 `tick_rate_hz: 64`，在空闲时约每 46 ms 检查一次填充包发送条件；测试对照实际行为。

按上表将各平台产物放在 `dist/rust` 对应子目录。对已授权的 Android 测试模拟器执行 `python clients/android/test_device.py --serial <设备序列号> --go <Go参考程序路径> --artifacts dist/rust/android --report dist/rust/android/device-test.json`，即可将结果绑定到本次 APK。所有产物就绪后运行 `python scripts/package-release.py`，即可生成跨平台 ZIP、逐文件 SHA-256 清单和 ZIP 的独立校验文件。各平台的运行说明随压缩包分发。

[C ABI 声明](../crates/cs2world-ffi/include/cs2world.h) · [架构设计](superpowers/specs/2026-11-21-cs2world-core-design.md) · [线协议字节表](superpowers/specs/2026-11-21-cs2world-wire.md) · [平台 CI](../.github/workflows/rust-core.yml)
