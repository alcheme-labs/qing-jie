# 轻截

[English](README.en.md)

我给自己写的 Mac 菜单栏截图小工具，平时用来框选截图、画个箭头，偶尔做个短 GIF。开源出来给有类似需求的人用，后续按自用需要更新。

## 下载

从 [GitHub Releases](https://github.com/alcheme-labs/qing-jie/releases/latest) 下载 Apple 芯片版 ZIP，解压后把「轻截」拖到「应用程序」。最低系统版本为 macOS 13，发布包在 Apple 芯片的 macOS 15.7.9 上验证。应用会跟随系统语言显示简体中文或英文。

发布包使用临时签名，未经 Apple 公证。如果 macOS 拦截首次打开，请核对下载来源，再按 [Apple 的打开说明](https://support.apple.com/zh-cn/102445) 操作。首次截图时，在「系统设置 → 隐私与安全性」的屏幕录制设置中允许轻截，按提示退出并重新打开。

## 使用

- **截图和标注**。按 **⌘⇧X** 拖选区域。在编辑窗口用画笔、矩形、椭圆或箭头标注，按 **Enter** 复制图片，按 **⌘S** 另存为 PNG。也可以右键菜单栏图标保存剪贴板图片。
- **录制 GIF**。从菜单栏选择「录制区域 GIF…」，框选后开始录制。停止时选择保存位置，最长 12 秒。
- **逐帧制作 GIF**。框选一次，之后每按一次截图快捷键或点一次「取一帧」，就从同一区域新增一帧。至少取两帧即可保存。
- **设置**。在菜单栏更改截图快捷键和登录时启动选项。登录时启动默认开启，可以关闭。

截图和 GIF 都在本机处理。应用不会上传画面，也不会保存截图历史。

## 从源码构建

需要 macOS 13 或更高版本、Xcode 命令行工具、Rust 和 Python 3。

```sh
sh scripts/cargo.sh test --locked
sh scripts/build-app.sh
```

构建产物是 `dist/轻截.app` 和对应本机架构的 ZIP。轻截采用 Rust 和 AppKit，区域截图使用 macOS 系统工具，GIF 取帧使用 ScreenCaptureKit。

项目采用 [MIT 许可证](LICENSE)，安装包内附第三方许可文本。
