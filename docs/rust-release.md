# CS2World 0.1.0

## Windows

运行 `windows/CS2World-XP.exe`，在「节点管理」粘贴 `cs2world://` 链接，选择节点后连接。从旧便携版升级时，将原来的 `data` 目录放在 EXE 旁边即可继续使用已保存的节点。系统代理模式可用普通用户权限运行。TUN 模式使用内嵌的 Wintun 驱动组件，需要管理员权限。

可选的 `windows-service/cs2world-server.exe` 可由 Windows 服务控制管理器以 `CS2WorldCore` 服务名运行，命令行为 `service -c 配置绝对路径`。它也支持前台 `run -c` 和配置检查 `check -c`。

## Linux

选择 `linux-x86_64` 或 `linux-aarch64`。复制 `configs/cs2world_server.json` 为自己的配置，用随机 PSK 替换示例值。服务端与客户端的 PSK、FEC 参数应匹配。在服务端放行所配置的 UDP 端口。

```sh
chmod +x ./linux-x86_64/cs2world-server
./linux-x86_64/cs2world-server check -c ./server.json
./linux-x86_64/cs2world-server run -c ./server.json
```

在 arm64 机器上将目录名换为 `linux-aarch64`。在使用 systemd 的主机上，可用 `sudo bash linux/deploy-linux.sh install ./linux-x86_64/cs2world-server ./server.json` 安装。

## Android

安装 `android/CS2World-debug.apk`，粘贴分享链接或导入 JSON，点击 **Connect** 并授予 VPN 权限。该包使用本地调试签名，支持 Android 8.0 及以上的 arm64-v8a 和 x86_64 设备。SDK 集成包和独立的设备测试 APK 同放在 `android` 目录。设备测试报告记录了实测 APK 的哈希。

## macOS

在终端中运行下面两条命令即可启动 Mixed 客户端。先将示例中的服务器地址、端口和 PSK 改成自己的配置。

```sh
chmod +x ./macos/cs2world
./macos/cs2world run -c ./configs/cs2world_client.json
```

`macos/CS2World-xcframework.zip` 包含 Intel 和 Apple Silicon 静态库、C 头文件与 Swift 模块映射。原生 AppKit 界面使用相同的 C ABI。Mac 构建包中的说明列出了从预编译核心生成通用 .app 的命令。

每个交付文件的摘要见 `SHA256SUMS.txt`。兼容性覆盖、平台实测条件及待验项目见 [验证记录](VERIFICATION.md)。
