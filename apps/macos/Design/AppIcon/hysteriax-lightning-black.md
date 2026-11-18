# HysteriaX 原稿改色图标

- 原始文件：用户提供的 `AppIcon_副本.sketch`；原文件保持不变。
- 可编辑副本：`hysteriax-lightning-black.sketch`。
- 修改范围：模板页面中 10 个闪电路径的启用填充由橙色改为纯黑 `#000000`，解除这些填充与原橙色色板的关联。其他页面、路径、坐标、底板、阴影、透明度和导出区域保持原样。
- 导出工具：Sketch 自带的 `sketchtool export slices`，未使用图像生成或重绘。
- 1024 × 1024 导出：`hysteriax-lightning-black.png`。
- 原稿切片：`../../Sources/HysteriaX/Resources/Assets.xcassets/AppIcon.appiconset`，保留原稿对应的 10 个切片及各尺寸的独立排版。
- 当前客户端图标：`../../Sources/HysteriaX/Resources/AppIcon.icon`。白色背景与黑色闪电独立分层，避免系统把扁平黑白图标再次缩小并叠加底板。
- 分层图标的 `Assets/lightning.svg` 使用 Sketch 中 `0B40E829-BD18-4587-AD36-A588C5FAE722` 路径直接导出的 SVG 路径数据，未重绘；`viewBox="100 100 824 824"` 将原稿安全区映射到系统图标画布，保持最终显示的尺寸与位置。
- `hysteriax-lightning-black-system.png` 是重新启动客户端后，从系统读取的实际应用图标，用于与 Decentralized 对照；此文件不是编辑源。

从仓库根目录导出，例如：

```sh
/Applications/Sketch.app/Contents/MacOS/sketchtool export slices \
  apps/macos/Design/AppIcon/hysteriax-lightning-black.sketch \
  --output=apps/macos/.build/sketch-black-export --overwriting=YES
```

原模板的 2× 切片名称和导出后缀都含有 `@2x`，因此导出文件的 `@2x@2x.png` 需要按资源清单对应为 `@2x.png`。无需再次缩放。
