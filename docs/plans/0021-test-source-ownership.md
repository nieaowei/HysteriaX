# 0021 按归属整理顶层测试源码

实施日期：2026-11-05（Asia/Shanghai）。按用户确认的方案，组件测试靠近组件，脚本单元测试归入脚本分类，仓库顶层 tests 只保留跨组件集成与共享数据。

## 范围

仅移动文件及更新引用与根路径定位，保留断言、夹具、执行参数和业务行为。项目设计前提仍为 10 万用户、1 万节点。本次没有部署或数据迁移。

## 已实施

1. 移动 31 个文件：23 份独立 Swift 回归源码及 1 份本地化资源检查移到 `apps/macos/Tests/`；4 份 Python 单元测试移到 `scripts/tests/unit/`；独立 Go 模块的 3 个文件移到 `tests/integration/traffic-clear/`。
2. 文档审计测试采用 `test_template_documentation_audit.py` 命名，与原有 19 项脚本单元测试一同发现，合计 26 项，原断言不变。
3. `tests/fixtures/` 的共享证书、配置及固定 schema 评估数据保持原位置；现有 `apps/macos/UITests/` 保持原位置。顶层 tests 只保留 README、fixtures 和 integration。
4. 更新 24 个原有引用文件，涵盖客户端验证脚本、CI 单元测试命令及当前文档。修正移动后的 Python 仓库根定位，保留所有移动文件的原权限。
5. 新增仓库级、macOS 回归源码和脚本单元测试索引，更新架构与脚本索引。应用的 Xcode 源码组及 SwiftPM 可执行目标仍明确使用 `Sources/HysteriaX`，独立 @main 回归源码没有并入应用目标。

## 验证

- 23 份 Swift 源码与 3 个 Go 模块文件的 SHA256 在移动前后完全一致。
- 5 份 Python 文件在统一根定位后语法树与原版一致，根目录求值均正确，执行权限保持。
- 整理前 19 项脚本单元测试和 7 项文档审计测试分别通过；整理后统一发现 26 项通过。
- Go 流量清零测试在整理前、后均通过，后者使用 `-count=1` 实际重新执行。
- 新路径下五个客户端入口通过：本地化、管理显示文本、节点任务反馈、ACME DNS 客户端及订阅客户端；最后一项包含 URL、模板和字段布局三份独立 Swift 回归程序。
- 当前文件中的旧路径检查通过；62 份 Python、30 份 Shell 语法检查通过；CI YAML 与文档本地链接检查通过。
- Swift API 生成器 `--check`、`cargo fmt --all -- --check`、`git diff --check` 通过。
- 未执行完整数据库、Docker、真实节点和 UI 测试，也未执行完整 macOS 应用构建；本轮调整没有改动应用构建配置或业务实现。
