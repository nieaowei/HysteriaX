# 0023 验证记录

执行日期：2026-10-20。平台：本地 macOS arm64。验证对象：订阅编辑器、文档元数据和审计工具。

| 检查 | 命令 | 结果与范围 |
| --- | --- | --- |
| 固定文档元数据 | `python3 scripts/generate-template-documentation-order.py SOURCE/docs/configuration --check` | 通过；111 页有字段标题，1,173 个页内去重条目；Schema 和完整英文文档树摘要一致。 |
| 全文档对应 | `python3 scripts/audit-template-documentation.py SOURCE/docs/configuration --write` | 通过；121 页全部入清单，未归类 0；每个记录的 Schema 指针可解析。 |
| 在线全导航 | `python3 scripts/audit-template-website.py --as-of 2026-10-20 --write` | 通过；125 页成功读取；4 个新增页面、固定版本页面缺失 0。原文内容不复制入仓库，仅保存标识和哈希。 |
| 解析器与证据回归 | `python3 tests/template-documentation-audit.py` | 7 项通过：组合标题、重复标题、示例隔离、错误/不完整源树拒绝、协议作用域、嵌套联合类型指针、在线导航与正文隔离。 |
| Swift 客户端模型 | `sh scripts/verify-subscription-client.sh` | 通过；URL、API 边界、模板身份、会话隔离、源码/表单状态、类型/引用、未知字段、默认值不落盘、隐藏字段、UInt64、撤销重做及字段分类；含新增 UoT 默认版本、TUN MAC、OpenVPN 模式、系统 TLS、USB/IP、OpenConnect 保活和四项默认值预览断言。 |
| 原生 SwiftUI 交互 | `sh scripts/verify-template-picker-ui.sh` | 通过；真实 NSHostingView/NSWindow 离屏渲染，规则集/出站/DNS/传输/动作/证书类型切换、Snell 版本、TLS 开关、OpenVPN 会话模式与通用 TLS 引擎切换、条件字段、HTTP Client 共享/内联、撤销重做、接收窗口、注入项与大列表。 |
| 原生集合回归 | `sh scripts/verify-template-collection-ui.sh` | 通过；解析器、服务器/规则删除、撤销、重做、重新添加及大集合自然行高。 |
| 双语资源 | `bash scripts/verify-localization.sh` | 通过；资源引用、两种语言的条目一致性、运行时语言切换。 |
| 生产校验器兼容 | `python3 scripts/verify-subscription-validator-compatibility.py` | 通过；Mihomo/sing-box 合计 1,046 项结果及归一化字段路径集合与冻结基线完全相同。 |
| macOS Debug 构建 | `xcodebuild -project apps/macos/HysteriaX.xcodeproj -scheme HysteriaX -configuration Debug -sdk macosx -destination 'platform=macOS' -derivedDataPath /tmp/hx-0022-build build CODE_SIGNING_ALLOWED=NO` | 通过，`BUILD SUCCEEDED`；不包含签名分发验证。 |
| 补丁格式 | `git diff --check` | 通过。 |

SwiftUI 检查直接渲染实际编辑器，操作模型并等待视图更新，检查数据保留、撤销和布局；没有把模型赋值称作真实鼠标/键盘端到端操作。当前运行中的用户应用未因构建被重启。

上述检查不证明每种代理协议的联网互通、全部操作系统兼容性、所有交叉字段语义或企业 VPN 身份验证。逐项附录中的 `schema` 表示可对应到固定 Schema，不能自动提升为运行时验证结论。

文档来源使用不可变提交 `af6e64c3b69e6132ebaee0e1a3d24e93903f6709`；`SOURCE` 应指向该提交的完整源码目录。在线快照随网站更新变化，固定版本清单的生成受 Schema 与文档树哈希约束。
