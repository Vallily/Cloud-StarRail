# Cloud-StarRail · 云·星穹铁道

一个基于 **Tauri + WebView2** 开发的极小体积云铁道启动器。

本质仍然是[官方网页版云·星穹铁道](https://sr.mihoyo.com/cloud/#/)，只提供独立桌面窗口、自定义标题栏、无边框全屏等体验。登录、排队与游戏串流均由官方网页处理。

## 下载

[直接运行 exe](https://github.com/Vallily/Cloud-StarRail/releases/latest/download/Cloud-StarRail.exe) · [下载安装包](https://github.com/Vallily/Cloud-StarRail/releases/latest/download/Cloud-StarRail-Setup.exe) · [所有版本](https://github.com/Vallily/Cloud-StarRail/releases)

支持 Windows 10 / 11 x64；直接运行版需要 WebView2 Runtime，安装包可在缺少时联网补装。

## 开发

```powershell
npm ci
npm run dev
```

打包：`npm run build`。

本项目为非官方社区项目，与米哈游 / HoYoverse 无关联。源码采用 [MIT](LICENSE) 许可证。

支持窗口／无边框全屏，以及截图和硬件加速录屏。默认 F12 截图，Ctrl+F12 开始／结束录屏，也可在设置中修改。设置自动保存，媒体默认保存到程序所在目录的 `cloud-starrail-screencapture` 文件夹；可自行更改目录。已有用户设置在升级时保留。

当前版本不提供正式的 Steam 叠加支持。GPU 截图优化计划在后续小版本提供。
