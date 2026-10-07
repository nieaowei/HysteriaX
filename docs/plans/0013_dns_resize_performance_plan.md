# 0013 — DNS 记录页缩放优化

日期：2026-10-07（Asia/Shanghai）
状态：已实施并验证
基线：00a8f1d，DNS 源文件来自 HEAD，其余源文件与修复版本相同。

## 定位与实施

最近的 deb32a8 减少了节点、用户、任务和凭据详情的重复布局测量。其后的 65c5346 为 DNS 页增加了随 GeometryReader 宽度更新的表格列宽，以及详情中的嵌套 Grid。此次优化只修改 DNS 页，保留已有未提交的 NodesView 修改。

1. 表格移除 GeometryReader，让原生 Table 使用可伸缩的域名、目标列；筛选、排序结果在构造表格时取得一次，避免尺寸变化触发几何闭包中的重复计算。
2. 标题使用现有 DetailHeaderLayout；详情卡片使用 OverviewColumnsLayout，宽度不足 576 点时改为单列，保持同一内容树。
3. 元数据使用等宽两列布局；标签和值使用现有 OverviewPairLayout，标签宽度固定为 56 点，值保持可选择、可换行。移除嵌套 Grid 和文字基线对齐探测。
4. 详情 ScrollView 的内容明确使用视口宽度减去左右内边距，减少对整棵详情树理想宽度的探测。
5. 扩展现有缩放基准：支持只运行 DNS 场景、指定记录数量，并通过 NSWindow.setContentSize 改变真实窗口尺寸。事件循环的显示回调移到计时区间之外，分开记录同步布局和离屏绘制；因此本轮数值不能直接与 0012 的旧基准比较。

单独替换卡片布局和 Grid 的早期试验未显示稳定改善，最终结果来自上述组合修改。

## 对照结果

Swift 6、-O、whole-module-optimization，离线静态夹具，DNS 记录已选中、下方详情展开。预热后记录 48 帧，窗口宽度 720–1138 点、高度 700–898 点。基线与最终版本按顺序运行，不启用 PNG 导出。记录数量只改变表格数据，详情使用相同的 AAAA 记录。

| 记录数 | 修改前平均布局 ms | 修改后平均布局 ms | 修改前 P95 ms | 修改后 P95 ms |
| --- | ---: | ---: | ---: | ---: |
| 1 | 11.62 | 9.68 | 20.17 | 19.18 |
| 200 | 19.20 | 12.48 | 24.07 | 17.83 |

平均同步布局耗时分别下降约 17% 和 35%。200 条记录时，布局加离屏绘制的平均耗时从 88.87 ms 降至 82.05 ms。离屏绘制包含软件 bitmap 渲染，不能据此换算真实窗口拖动 FPS；多次运行仍有波动，结果用于说明优化方向与此次对照，不保证所有数据规模下同等降幅。

原始结果：0013_dns_resize_before.json、0013_dns_resize_after.json。

## 验证

- scripts/verify-main-split-resize.sh：单记录和 200 条记录的 DNS 缩放对照通过。
- scripts/verify-detail-header-layout.sh：横排/竖排对齐、条件操作和内容身份检查通过。
- scripts/verify-vertical-split.sh：初始等分、选择切换、拖动以及宽高变化后位置保持检查通过。
- python3 scripts/verify-dns-ui.py：最终完整 UI 流程通过，结果为 apps/macos/.build/dns-ui-1791355367.xcresult；检查了 DNS 页面截图、编辑、连接与区域选择、节点域名分配和 ACME 配置。
- script/build_and_run.sh --verify：最终 Xcode Debug 构建及应用启动通过。
- git diff --check：通过。

验证期间曾有一次 UI 测试在后续节点配置步骤失败，重跑完整流程通过；未将失败运行记为通过。原生窗口尺寸基准用于缩放验证，尝试过但未可靠命中边缘的 UI 拖拽检查未保留。
