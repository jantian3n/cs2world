# CS2World-XP Windows 客户端

在 Windows x64 上运行 `CS2World-XP.exe`。程序使用内嵌 Rust 核心，节点和选项保存在 EXE 同目录的 `data/state.json`。升级便携版时保留原 `data` 目录。标题栏关闭按钮隐藏主窗口，托盘菜单的「退出并还原系统代理」会停止内嵌核心。

在「节点管理」中导入 `cs2world://` 分享链接，或从剪贴板批量导入多行和 Base64 订阅。在「连接」页选择节点与代理方式。系统代理使用本地 Mixed 端口，TUN 虚拟网卡需要相应的 Windows 权限。界面可分别显示本地监听就绪、节点认证成功及 A2S 查询结果。

在仓库根目录运行以下命令构建单文件发行版。

```powershell
./scripts/package-windows.ps1
```

默认输出为 `dist/rust/windows/CS2World-XP.exe`。以下命令使用本地 Go 参考程序，检查程序单独启动、旧节点文件、TCP 和 UDP、托盘清理，以及握手超时和 256 KiB 下载期间的窗口响应。

```powershell
cargo test -p cs2world-xp --locked
python -B win32-xp/tests/smoke.py --exe dist/rust/windows/CS2World-XP.exe --go dist-v2rayn-win/sing-box.exe
```

如需核对自己的活动节点，可显式传入私密 state 文件。核对程序在临时 Mixed 端口进行 A2S 查询、认证、HTTPS CONNECT 和 UDP DNS 检查，全程保持用户现有系统代理设置。程序只输出测试结果及耗时。

```powershell
python -B win32-xp/tests/smoke.py --exe dist/rust/windows/CS2World-XP.exe --real-state C:\private\state.json
```
