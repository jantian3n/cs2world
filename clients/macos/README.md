# CS2World macOS 客户端

使用预编译的通用命令行客户端时，在终端执行 `chmod +x ./cs2world`，再执行 `./cs2world run -c client.json`，即可启动客户端配置中的 Mixed 监听。

在已登录图形桌面的 Mac 上解压 `CS2World-macOS-build-kit.zip`，进入 `CS2World-macOS-build-kit` 文件夹。用 Xcode 或 Xcode Command Line Tools 的 Swift 编译器执行以下命令。

```sh
bash build-app.sh
```

脚本先核验输入文件的 SHA-256，使用包内的 XCFramework 分别链接 arm64 和 x86_64，再用 `lipo` 生成通用程序。它对应用作本地 ad-hoc 签名，重新解压 ZIP 并验证两种架构的签名和 AppKit 依赖。启动验收会打开一个 CS2World 窗口，在回环地址随机端口启动 Mixed 监听，收到 Rust 内核的停止回调后退出。完成后双击解压 `build-macos/CS2World-macOS.zip`，即可打开其中的 `CS2World.app`。最低系统版本为 macOS 12。

另一次启动验收可以独立运行。验收使用当前 Mac 的原生架构，要验证两种架构的原生启动，可分别在 Intel 和 Apple 芯片 Mac 上执行。

```sh
bash verify-app.sh build-macos/CS2World-macOS.zip
```

构建使用本机 SDK 和已编好的 Rust 静态库，可离线进行。已有签名身份时，可在构建前通过 `CS2W_SIGN_IDENTITY` 指定钥匙串中的身份。对外分发所需的 Developer ID 签名和公证由分发者处理。

## 从完整仓库重建 Rust 静态库与应用

在仓库根目录执行以下命令。默认输出位于 `target/packages/macos`。

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin
bash clients/macos/package.sh
```

## 使用

打开应用，粘贴分享链接或导入客户端 JSON，点 **Connect**。Mixed 模式监听配置中的本地端口，分享链接默认使用 `127.0.0.1:17890`。需要接管系统流量时勾选 **Route system traffic through utun**。创建 utun 和配置路由需要相应系统权限，启动失败原因显示在窗口内。关闭窗口或点击 **Disconnect** 会停止运行时并释放网络资源。
