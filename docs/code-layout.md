# 目录与测试归属

文件按职责和所属组件放置。构建、生成、运维和实际迁移工具归入 `scripts/`；仓库级验证程序归入 `tests/`；组件专属测试靠近组件。Python、Shell、Ruby 等可执行文件同样按用途归类。

## 仓库布局

| 位置 | 职责 |
| --- | --- |
| `crates/hysteriax-server/src/` | Rust 服务端实现与所属模块的测试 |
| `crates/hysteriax-server/migrations/` | 版本化数据库 SQL 与迁移维护说明 |
| `crates/hysteriax-template-validator/` | 独立的固定 schema 验证器 |
| `apps/macos/Sources/HysteriaX/` | macOS 应用源码与运行时资源 |
| `apps/macos/Tests/` | 独立 Swift 回归程序和本地化资源检查 |
| `apps/macos/Tests/runners/` | 客户端回归的编译、执行与隔离环境入口 |
| `apps/macos/UITests/` | macOS UI 测试用例与 UI 夹具源码 |
| `components/hysteriax-acme/` | 节点独立 ACME 组件 |
| `openapi/` | 管理 API 契约源文件 |
| `deploy/` | 部署时使用的配置、节点脚本和许可资源 |
| `production/` | 本地生产维护说明、发布和诊断记录 |
| `docs/` | 当前开发、业务与运维指南 |
| `docs/plans/` | 按编号保存的实施记录 |

操作工具分为 `scripts/build/`、`scripts/generate/`、`scripts/ops/`、`scripts/migrations/`。具体入口见 [操作脚本索引](../scripts/README.md)。

## 测试布局

| 位置 | 内容 |
| --- | --- |
| `tests/unit/` | 运维工具、测试辅助代码和文档提取工具的 Python 单元测试 |
| `tests/integration/` | HTTP、数据库、SSH、DNS、部署、升级和多实例功能验证 |
| `tests/integration/traffic-clear/` | 固定 Hysteria 版本流量接口的独立 Go 测试模块 |
| `tests/contracts/` | OpenAPI、配置内核、固定 schema 与模板文档契约验证 |
| `tests/performance/` | 容量、并发、锁争用、查询计划和大规模清理验证 |
| `tests/support/` | 验证程序共用的客户端、隔离数据库、故障注入和夹具生成代码 |
| `tests/fixtures/` | 跨组件共享证书、配置、固定语料和预期结果 |
| `tests/fixtures/runtime/` | 测试节点 Dockerfile、网络隔离脚本和测试 CA 服务 |
| `tests/requirements.txt` | Python 验证程序的依赖清单 |

验证产品行为的可执行程序属于测试目录。例如，`tests/integration/verify-lock-migration.py` 验证升级流程，实际修改测试库的工具则位于 `scripts/migrations/migrate-lock-contention-test-db.py`。常规部署维护继续使用服务端 `--migrate-only`。

macOS 的独立 `@main` 回归程序与运行入口统一位于 [客户端测试目录](../apps/macos/Tests/README.md)。应用的 Xcode 源码组和 SwiftPM 可执行目标使用 `Sources/HysteriaX`，客户端回归由各自的 runner 选择源码并编译运行。

完整运行入口见 [仓库级测试索引](../tests/README.md)，环境要求和专项矩阵见 [部署与回归验证](deployment-testing.md)。上述目录说明不代表测试已执行，也不替代正式性能验收。

## 服务端内部组织

- 顶层领域组件和包含多个实现文件的分组使用目录与 `mod.rs` 入口。
- 内部叶子模块采用所属目录下的具名文件。例如 `api/job_retries/handlers.rs` 与 `handlers_tests.rs`，无需再为一份实现和配套测试增加目录层级。
- 多文件的大型测试套件可保留独立目录。Rust 的测试子模块可使用明确的 `#[path]` 绑定，保持私有访问关系和测试过滤器稳定。
- API 边界允许一级职责分组。令牌相关文件及使用记录文件归入 `api/admin_tokens/`，指纹处理文件归入 `api/nodes/`，跨模块回归归入 `api/tests/`。物理路径调整时，可通过父模块的明确绑定保留既有 Rust 命名空间。
- SQL、Lua、Shell 等专用资源靠近所属模块，例如 `cache_domains/cache_batch_version.sql`、`quota_rank/quota_rank.lua`。
- 内部文件使用职责名称，如 `handlers`、`event_stream`。`/api/v2`、`v2:` 游标和 `ssh-journal-v2` 属于对外契约，继续保留协议版本标记。

## 新增和移动文件

新增文件先确定所属组件、操作或验证职责，再选择现有目录。共享测试代码放入 `tests/support/`，共享静态数据放入 `tests/fixtures/`，运行时测试环境资产放入 `tests/fixtures/runtime/`。

移动入口时同步检查调用方、共享模块导入、仓库根定位、相对资源路径、执行权限、CI、Docker COPY 和当前文档。命令示例默认从仓库根目录执行；在其他工作目录调用时需遵循入口自身的参数说明。

历史实施记录、审计和发布归档保留当时的路径与执行结果。当前操作以本页及对应入口文档为准，历史记录不作为当前命令清单。
