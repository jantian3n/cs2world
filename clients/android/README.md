# CS2World Android 客户端

安装 `CS2World-debug.apk`，粘贴 `cs2world://` 分享链接或原生客户端 JSON，点 **Connect** 并同意系统的 VPN 请求。**Open JSON file** 可以从系统文件选择器导入配置。点 **Disconnect** 或从系统 VPN 设置断开即可释放 TUN。配置用 AndroidKeyStore 中的 AES 密钥加密后保存在应用私有目录。

链接模式使用 `127.0.0.1:17890`、私网直连和默认节点出口。JSON 模式使用给定的 Mixed 监听与路由。VPN 同时捕获 IPv4 和 IPv6，沿用底层网络的 DNS。应用为核心的传输及直连 socket 调用 `VpnService.protect`。系统同意、文件描述符交接和生命周期见 [Android VPN 开发指南](https://developer.android.com/develop/connectivity/vpn)。

## 构建和回环验证

构建需要 JDK 17 或 21、Gradle 8.11.1、Android SDK Platform 35、Build Tools 35 和 NDK。设好 `ANDROID_HOME`、`ANDROID_NDK_HOME` 与 `JAVA_HOME` 后，从仓库根目录运行。

```sh
rustup target add aarch64-linux-android x86_64-linux-android
python clients/android/package.py --with-tests --output dist/rust/android
```

`--gradle PATH` 可指定 Gradle 可执行文件。`--libs-only` 单独打包 arm64-v8a 和 x86_64 JNI 库及 C 头文件。安装包使用 Android debug 签名。

在专用模拟器上运行 JNI 和 VpnService 测试，传入已经启用 CS2World 的 Go 参考程序。驱动安装两个 APK，在本机回环启动节点和回显端点，临时批准测试应用的 VPN 权限，结束后恢复原 AppOps 模式。

```sh
python clients/android/test_device.py --serial emulator-5554 --go /path/to/sing-box
```

第二项用例从独立 UID 访问保留测试地址 `198.18.0.42`。只有测试节点会把该地址转到本机的随机回显端口。设备断言核对 TCP 内容、UDP 内容和来源，主机驱动另外校验两种协议收到的随机令牌。
