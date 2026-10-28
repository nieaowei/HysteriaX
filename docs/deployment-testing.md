# 部署与回归验证

在独立数据库、Redis 和自有 Docker 容器中运行验证。不要将开发脚本指向线上数据库或直接复用线上 Compose 项目。带真实快照的测试必须先隔离外部出站，避免副本触发 SSH、DNS 或撤权。测试环境准备见 [安装指南](installation.md#local-development-and-tests)。

## 常用检查

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
ruby scripts/generate-swift-api-models.rb --check
bash scripts/verify-node-task-feedback.sh
bash scripts/verify-management-display-text.sh
bash scripts/verify-management-protocol.sh
bash scripts/verify-localization.sh
python3 -m unittest discover -s tests -p 'test_backup_restore.py'
```

Rust 数据库测试需要可丢弃 PostgreSQL 的 `TEST_DATABASE_URL`；Redis 专项另需 `TEST_REDIS_URL`。保留测试输出中的忽略项和失败，不把部分完成记为全量通过。上述是运行入口，不代表本次已经执行或通过。

## 全进程与部署专项

| 入口 | 验证范围 |
| --- | --- |
| `scripts/verify-deployment-matrix.py` | 自有 systemd Linux 容器中的 SSH 部署、探测及卸载。 |
| `scripts/verify-multi-instance-billing.py` | 多实例计费批次、崩溃／数据库断连、租约接管和串行参考。 |
| `scripts/verify-multi-instance-cache.py` | 跨实例失效、版本与 Redis 故障。 |
| `scripts/verify-multi-instance-events.py` | 发布顺序、进程故障和游标恢复。 |
| `scripts/verify-multi-instance-cutover.py` | v2 多实例协议及幂等切换。 |
| `scripts/verify-migrations.py` | 新库基线、完整迁移校验和多实例就绪。 |
| `scripts/verify-snapshot-capacity.py` | 最终 Linux 镜像、Caddy 三副本及 Redis 停止／空重启；快照需属于当前基线。 |
| `scripts/verify-backup-restore-compose.py` | 备份恢复及外部操作时间线不一致时的拒绝行为。 |

先查看各脚本的参数和环境要求；提供命令行参数解析的 Python 专项可用 `--help` 查阅。当前脚本使用 v2。原迁移链的升级、旧版 mTLS 转换和旧版恢复专项已移除；当前快照测试仅接受新基线及其后续兼容迁移。镜像、内核下载、Docker 权限和 fixture 参数以各脚本为准；不将旧脚本的历史通过当作本次重新执行。

容量记录只描述具体数据、并发和限制下的实验。本机 Docker 可完成多实例功能验收，不要求额外原生 x86_64 服务器。生产 PostgreSQL、Redis 与 API 可分机部署，按实际主机角色核算资源。详见 [当前架构](architecture.md)。

## 文档审计与发布边界

模板文档审计结果可以按需重新生成：先用 `scripts/audit-template-documentation.py` 的 `--write` 模式生成固定版本字段基线，再运行 `scripts/audit-template-website.py` 比较在线文档。前者需要匹配的上游文档源码；后者依赖前者生成的清单并访问网络。脚本参数及版本以源码为准，生成结果不属于运行时资源。

验证通过不等于执行发布。实际部署须本机构建镜像后传输，停止全部旧 API，保留可恢复备份，单独迁移并核查后启动。不要对线上执行故障注入或容量压力测试。
