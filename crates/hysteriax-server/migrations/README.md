# 数据库迁移

当前迁移链从 `0001_baseline.sql` 开始，最新版本为 schema 17。新安装使用空 PostgreSQL 数据库，兼容升级从已登记的版本应用待执行迁移。原 0001～0074 迁移链的数据库和备份需要匹配的旧镜像与主密钥。保留 SQLx 迁移记录，避免在已有数据库上手工重放基线。

## 初始化与执行

停止共享目标数据库的全部 API 实例，保留可恢复备份和当前镜像，再用目标版本运行一次维护命令。Compose 部署执行以下命令。

```sh
docker compose --profile maintenance run --pull never --rm --no-deps migrate
```

本地开发也可使用 `cargo run -p hysteriax-server -- --migrate-only`。维护入口应用 SQL，初始化管理员、默认订阅模板、凭据引用索引、用户访问检查和运行时契约。维护期间后台 worker 保持停止。普通 API 启动逐项校验嵌入的迁移版本和 SHA-384 校验和，并检查管理协议、主密钥指纹和初始化状态。

首次部署、升级顺序与停机排空说明见 [安装指南](../../../docs/installation.md#database-baseline)。

## 迁移内容

| 版本 | 改动与升级边界 |
| --- | --- |
| [0001](0001_baseline.sql) | 建立基础表、视图、函数、约束和索引，初始化审计总计、缓存与事件 epoch，以及各缓存域的版本桶。 |
| [0002](0002_node_kick_batches.sql) | 将撤权断连改为每节点单活跃任务，增加用户执行代次和分页索引。迁移会清理旧 kick 任务、撤权需求及其相关历史。 |
| [0003](0003_remove_authorization_previews.sql) | 回收授权预览表及令牌，保留授权组、成员、节点和撤权需求。 |
| [0004](0004_separate_job_cache_roots.sql) | 分离任务进度和节点实体的缓存失效版本。节点详情通过 jobs 域跟踪任务进度。 |
| [0005](0005_async_authorization.sql) | 增加用户级连接密码、授权变更游标、增量输入项和即时来源排除。 |
| [0006](0006_user_credential_directory.sql) | 将连接凭据目录汇总为每用户一条记录，分配数量按当前分页计算。 |
| [0007](0007_retire_legacy_conversions.sql) | 删除过时秘密字段、逐节点证书绑定和旧转换标记，收紧凭据约束。已填充的早期数据库须先满足该迁移的转换校验。出现转换未完成、密码缺失或孤立流量批次时，保留数据库并按匹配的早期版本处理。 |
| [0008](0008_drop_monitoring_leases.sql) | 删除由资源租约和探测令牌取代的监控租约表。 |
| [0009](0009_unify_user_credentials.sql) | 将连接密码和订阅令牌统一到 `user_credentials`，保留原秘密与订阅撤销历史。已有的无订阅用户在首次联合轮换时补齐。 |
| [0010](0010_lock_contention.sql) | 增加节点套餐到期评估索引与调度字段，回收旧全节点扫描状态及统计重试链锁表。 |
| [0011](0011_sampling_retention.sql) | 为终态采样工作、执行证据和未结算周期批次增加清理索引。 |
| [0012](0012_history_retention.sql) | 增加历史清理索引和事件发布时间，用五个独立序列为脏队列分配版本令牌，保留已有版本。 |
| [0013](0013_history_retention_throughput.sql) | 用语句级 DELETE 触发器批量更新流量汇总和缓存版本，增加活动采样领取索引。 |
| [0014](0014_execution_operation_time_cursor.sql) | 为未决执行记录增加按创建时间和操作 ID 排序的索引，支持全局及按所属工作筛选的游标分页。 |
| [0015](0015_database_bloat_retention.sql) | 裁剪热表冗余索引，调整 autovacuum 和 fillfactor，将代理探测及在线样本删除改为语句级触发器，增加墓碑时间戳及流量、监控、运维和元数据留存索引。升级期间需要预留索引构建空间。 |
| [0016](0016_scale_indexes_and_triggers.sql) | 补建 10 万用户 / 1 万节点规模下缺失的关联与外键索引，移除冗余索引，修复 `job_status_totals` 确定性加锁顺序，并将 `node_network_samples` 删除改为语句级触发器。 |
| [0017](0017_drop_users_exhausted_index.sql) | 移除引用 `usage_bytes` 的 `users_enabled_exhausted_idx` 部分索引，恢复 `users` 表高频计费更新的 HOT 优化。 |

持久表和后台清理依赖见 [数据库表用途](../../../docs/database-tables.md)，留存周期及工作线程见 [安装指南](../../../docs/installation.md#database-history-retention)。

## 测试库演练

以下工具针对已停止所有 API 的独立测试库，使用前通过 `--help` 核对来源版本和确认参数。

接受 `--binary` 的工具需要目标 schema 对应的服务端二进制，以及测试库初始化时的主密钥。

| 来源版本 | 入口 | 核查内容 |
| --- | --- | --- |
| 9 | [套餐调度与锁争用迁移](../../../scripts/migrations/migrate-lock-contention-test-db.py) | 逐表对比业务值摘要，检查迁移及运行契约。 |
| 10 | [采样留存迁移](../../../scripts/migrations/migrate-sampling-retention-test-db.py) | 检查留存索引、候选与查询计划。 |
| 11 或 12 | [历史留存迁移](../../../scripts/migrations/migrate-history-retention-test-db.py) | 检查历史行数和目标 schema，使用 `--target-schema` 选择目标版本。 |
| 14 | [数据库膨胀治理迁移](../../../scripts/migrations/migrate-database-bloat-retention-test-db.py) | 核查 schema 15 的索引、触发器、表参数与历史行计数。 |
| 15 | [规模索引与触发器迁移](../../../scripts/migrations/migrate-scale-indexes-test-db.py) | 核查 schema 16 的新增索引、冗余索引清理、触发器及核心表行计数。 |
| 16 | [移除用户超额索引迁移](../../../scripts/migrations/migrate-drop-users-exhausted-index-test-db.py) | 核查 schema 17 已移除 `users_enabled_exhausted_idx` 及 `users` 表行计数。 |

完整迁移、重复执行及不兼容记录拒绝由 [迁移验证脚本](../../../tests/integration/verify-migrations.py)和数据库模块测试覆盖。运行环境见 [部署与回归验证](../../../docs/deployment-testing.md)。

## 新增迁移

从 `0018_<description>.sql` 继续编号。已发布的文件保留原文、名称和顺序。应用对象使用相对 schema 名称以支持隔离测试。`pg_trgm` 安装到 `public`，相关索引显式引用 `public.gin_trgm_ops`。修改结构时同步更新 [数据库表用途](../../../docs/database-tables.md)，并验证空库初始化及受支持的升级路径。
