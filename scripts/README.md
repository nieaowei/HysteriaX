# 构建与运维脚本

从仓库根目录调用本目录下的工具。目录按实际操作职责分类，现有参数和默认行为保持不变。

新增文件的归属规则见 [目录与测试归属](../docs/code-layout.md)。本目录保留执行实际操作的工具，验证这些工具或产品行为的程序归入测试目录。

| 目录 | 用途 | 常用入口 |
| --- | --- | --- |
| [build/](build/) | 本地构建、启动 macOS 应用和打包发布 | `build-macos.sh`、`build-node-acme.sh`、`release-macos.sh` |
| [generate/](generate/) | API 模型、订阅字段及文档元数据生成、依赖许可更新 | `generate-swift-api-models.rb`、`generate-subscription-catalog.go` |
| [ops/](ops/) | 初始化、服务启动、备份恢复及数据库角色配置 | `init-secrets.sh`、`run.sh`、`backup-restore.py`、`provision-postgres.sh` |
| [migrations/](migrations/) | 针对特定历史版本的一次性迁移工具 | `migrate-lock-contention-test-db.py` |

```sh
scripts/build/build-macos.sh
scripts/build/build-macos.sh --verify
ruby scripts/generate/generate-swift-api-models.rb --check
```

`migrations/` 中的工具只针对说明中明确的历史 schema 和停止状态测试库。常规数据库维护仍使用服务端 `--migrate-only`，版本化 SQL 保留在 `crates/hysteriax-server/migrations/`。该迁移工具的配套回归入口是 `tests/integration/verify-lock-migration.py`。

仓库级验证、单元测试、性能测试和环境辅助代码统一见 [测试索引](../tests/README.md)，Python 测试依赖位于 `tests/requirements.txt`。macOS 专属回归源码及运行入口见 [客户端测试索引](../apps/macos/Tests/README.md)。构建与生产发布方式见 [发布指南](../docs/release.md)。

## 临时数据库连接池诊断构建

`python3 scripts/build/prepare-pool-diagnostic-build.py --output <不存在的目录>` 从已提交且干净的 Git 输入生成临时构建上下文。工具验证锁定的 sqlx-core 0.9.0 crate SHA256，再应用同目录的 `sqlx-pool-timing.patch`；只在输出上下文覆盖依赖、锁文件、Docker COPY 和诊断测试注册，常规 Cargo/Docker 构建保持原依赖。

诊断记录包含同一 acquire 的 permit 等待、存活检查、建连、初始化、回退、丢弃，以及归还侧的调度、清理、检查、应用持有时长和池计数。只为超过原慢获取阈值的获取、归还超过一秒或应用持有超过两秒的连接增加日志，不记录 SQL、绑定值、URL 或凭据。计时包含等待期间的调度延迟，不能单凭阶段名将其解释为纯网络或数据库执行时间。该模式用于受控归因，不替代常规生产构建和容量验收。

诊断专属回归源码位于 `crates/hysteriax-server/src/db/pool_phase_diagnostics_tests.rs`，只由准备工具在冻结上下文注册。在隔离 `TEST_DATABASE_URL` 下从该上下文执行 `cargo test -p hysteriax-server pool_phase_`，验证真实池的等待、初始化、回调、归还和超时行为。

诊断测试使用独立 `CARGO_TARGET_DIR`，或先对冻结上下文执行 `cargo clean -p sqlx-core`；不要在多个补丁版本之间直接相信共享目标目录中的旧产物。生成的诊断 Dockerfile 会在构建前清理 sqlx-core 的 release 产物，确保重新编译已核验的补丁源码。

`ops/compare-production-pool.py` 是当前生产拓扑的有界 32/48/64 交叉对照入口，锁定既有已验证诊断镜像、六节点和 16 并发配置，不是通用部署工具。在 API 宿主机使用 `--root`、`--output` 指定现有部署与不存在的证据目录；完成在线快照后，控制端须下载并核验 `backup.json` 所记 SHA256，再把该摘要写入证据目录的 `backup-local-verified` 才开始切换。默认每档预热至少 180 秒、观察三个完整分钟；按正反顺序各执行一次，逐次排空 API，最终恢复原镜像与实际配置。可先用 `--prepare-only` 保存准备结果，再用 `--resume-prepared` 在原 API 与配置仍保持时继续；已开始的对照不能续跑。创建证据目录的 `STOP` 文件可结束观察并进入恢复。具体限制、结果和人工恢复时所需的原配置见 [0035](../docs/plans/0035-pool-size-comparison.md)。
