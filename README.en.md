# Qingjie

[简体中文](README.md)

Qingjie is a small Mac menu bar app I wrote for myself. I use it to grab part of the screen, draw an arrow, or make a short GIF. I'm sharing it in case it fits your workflow too. Updates follow my own day-to-day needs.

## Download

Download the Apple silicon ZIP from [GitHub Releases](https://github.com/alcheme-labs/qing-jie/releases/latest). Unzip it and drag the app into Applications. The minimum version is macOS 13. The release was verified on Apple silicon running macOS 15.7.9. The interface follows your system language preferences and supports English and Simplified Chinese.

This build is signed ad hoc and has not been notarized by Apple. If macOS blocks the first launch, check the download source, then follow [Apple’s instructions for opening the app](https://support.apple.com/en-us/102445). When you first capture the screen, allow Qingjie in System Settings under Privacy & Security. Quit and reopen the app if prompted.

## Use

- **Capture and annotate.** Press **⌘⇧X** and drag to select a region. Mark it up with the pen, rectangle, oval, or arrow. Press **Enter** to copy or **⌘S** to save a PNG. You can also right-click the menu bar icon to save a clipboard image.
- **Record a GIF.** Choose **Record Area as GIF…** from the menu and select an area. Record for up to 12 seconds, then stop and choose where to save the file.
- **Build a GIF frame by frame.** Select an area once. Press your capture shortcut or click **Take Frame** to add a frame from the same area. Save after taking two or more frames.
- **Settings.** Change the capture shortcut or toggle launch at login from the menu bar. Launch at login is on by default.

Qingjie processes screenshots and GIFs on your Mac. It does not upload captures or keep a screenshot history.

## Build from source

You'll need macOS 13 or later, the Xcode command line tools, Rust, and Python 3.

```sh
sh scripts/cargo.sh test --locked
sh scripts/build-app.sh
```

The build creates `dist/轻截.app` and a ZIP for your Mac's architecture. Qingjie uses Rust and AppKit, macOS system capture for still images, and ScreenCaptureKit for GIF frames.

The project uses the [MIT License](LICENSE). The app bundle includes third-party license notices.
