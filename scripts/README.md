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
