# 0014 — macOS 客户端中英文国际化

实施日期：2026-10-07

## 目标

客户端支持英文、简体中文，首次启动默认读取系统首选语言。中文语言变体使用简体中文，其他语言回退英文。设置提供“跟随系统 / English / 中文”，保存手动选择并更新界面。

## 实施步骤与结果

1. 提取视图、模型显示说明、校验提示、任务与审计说明、系统通知中的硬编码文案。使用标准 `en.lproj` 和 `zh-Hans.lproj/Localizable.strings`，共 1,197 条双语资源。
2. 新增 `Support/Localization.swift`：解析系统语言、使用 UserDefaults 保存偏好、通过 Observation 刷新界面、在线程安全的共享状态中读取偏好。模板采用 `{0}` 等占位符，单次替换避免将用户数据再次解释为模板。
3. 设置增加语言选择器，主窗口和设置窗口注入对应 Locale；显示日期使用所选语言，API 时间编码、数字输入约定和协议标识保持原有行为。
4. 本地文案统一调用 `L10n.text`；任务错误映射按显示时的语言读取，未识别的服务端错误及远端诊断原文保留。历史趋势的格式化更新时间改为显示时计算。
5. SwiftPM 声明默认英文及资源处理，Xcode 注册双语资源和本地化辅助源文件，同时补齐原有 `ManagementDisplayText.swift` 的目标注册。
6. 新增资源覆盖与占位符检查、语言识别/持久化/Observation/模板替换/运行时切换测试；现有独立 swiftc 验证脚本引入本地化依赖；既有中文 UI 测试显式指定中文，避免依赖测试机器语言。

英文文案直接人工整理；先前外部生成的翻译结果未进入项目。

## 验证

- `swift build --package-path apps/macos`
- Xcode Debug 构建（禁用签名，当前 macOS 目标）
- `bash scripts/verify-localization.sh`；可传入构建后 `.app/Contents/Resources` 验证实际打包资源
- `verify-management-display-text.sh`、`verify-subscription-client.sh`、`verify-dns-client.sh`、`verify-acme-dns-client.sh`、`verify-node-task-feedback.sh`、`verify-node-package-client.sh`
- `git diff --check`

无需部署服务端。
