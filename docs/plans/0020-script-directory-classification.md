# 0020 统一脚本目录并按用途分类

实施日期：2026-11-05（Asia/Shanghai）。按用户要求统一 `script/` 与 `scripts/`，区分测试、一次性迁移升级、性能测试及其他长期入口。

## 范围

仅整理脚本、辅助文件和调用路径，保持 CLI 参数、业务流程、隔离要求和正式验收门槛。仍以 10 万用户、1 万节点为规模前提。本次没有运行迁移、生产发布或容量压测。

## 已实施

1. 统一 93 个原有文件到 `scripts/`，删除旧 `script/`。根目录只保留新增的脚本分类索引 README。
2. 分类为：构建 3、生成 4、运维 4、一次性迁移及配套升级验证 2、性能检查 5；测试下分客户端 25、集成 24、契约 8、共享辅助模块 13、夹具 4，另有统一 Python 测试依赖文件。
3. `script/build_and_run.sh` 改为 `scripts/build/build-macos.sh`。实际为 Python 的 `scripts/verify-subscription.sh` 改为 `scripts/tests/integration/verify-subscription.py`。既有参数和程序内容保持，执行权限与原文件一致。
4. 修正移动后 Python、Shell、Ruby 的仓库根路径定位。跨目录使用共享测试模块的 Python 入口显式定位 `scripts/tests/support/`，保留原导入名称与辅助模块接口。
5. 同步更新 GitHub Actions、Compose 数据库初始化挂载、Docker 测试夹具 COPY、Codex 本地环境命令、生成文件的来源注释、测试加载路径、当前文档及脚本之间的调用。新增 `scripts/README.md` 分类索引并接入文档索引。
6. 一次性工具与常规维护分开：特定 schema 测试迁移位于 `scripts/migrations/`，版本化 SQL 仍在服务端 migrations，普通部署维护仍走 `--migrate-only`。历史审计和计划、生产归档及不可变 SQL 历史保持原样；已废弃旧迁移工具在历史 SQL 中的文字引用不属于当前可执行入口。

## 验证

- 整理前、后 Python 单元测试均 19 项通过；文档审计单元测试 7 项通过。
- 57 个 Python 文件可解析；统一路径变更、根定位和新增的辅助导入定位后，语法树与原版一致。
- 30 个 Shell 脚本语法检查、2 个 Ruby 文件语法检查通过；Go 目录字段生成器通过 `go test` 编译检查（无测试用例）。
- 实际执行 24 个 Shell 根定位表达式及求值 71 个 Python 根定位表达式，均定位到当前仓库；61 处共享模块导入可解析到新位置。
- Swift API 生成器 `--check` 通过；管理 API 契约检查通过。
- 新路径下客户端检查通过：1956 项本地化资源与引用检查，以及本地化运行、管理显示文本、节点任务反馈三组 Swift 用例。
- Compose、GitHub Actions YAML 可解析；159 处当前完整脚本引用检查通过；原文件执行权限保持，分类索引链接有效。
- `cargo fmt --all -- --check`、`git diff --check` 通过。
- 未执行需要数据库、Docker、真实 SSH/DNS、UI 应用启动的全量集成验收，也未运行正式性能测试。没有将静态检查等同于完整 CI 或生产验收通过。
