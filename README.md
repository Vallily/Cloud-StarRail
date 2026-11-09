# Cloud-StarRail · 云·星穹铁道

一个基于 **Tauri + WebView2** 开发的极小体积云铁道启动器。

本质仍然是[官方网页版云·星穹铁道](https://sr.mihoyo.com/cloud/#/)，只提供独立桌面窗口、自定义标题栏、无边框全屏等体验。登录、排队与游戏串流均由官方网页处理。

## 下载

前往 [Releases](https://github.com/Vallily/Cloud-StarRail/releases/latest) 下载直接运行的 exe 或安装包。支持 Windows 10 / 11 x64；直接运行版需要 WebView2 Runtime，安装包可在缺少时联网补装。

## 开发

```powershell
npm ci
npm run dev
```

打包：`npm run build`。更多说明见 [开发文档](docs/DEVELOPMENT.md)。

本项目为非官方社区项目，与米哈游 / HoYoverse 无关联。源码采用 [MIT](LICENSE) 许可证。