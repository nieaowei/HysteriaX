# 数据库迁移

当前迁移链从 `0001_baseline.sql` 开始，默认在新建空 PostgreSQL 数据库初始化。原 0001～0074 SQL 已合并并删除，程序不提供历史升级链、兼容识别或自动替换迁移记录的逻辑。未经核验的旧库及备份仍需匹配的旧服务版本，不能直接清空迁移记录或重跑基线。

2026-10-28 经用户明确授权，当前生产库已完成一次元数据归一化：核对逻辑结构，备份元数据并演练恢复，停止全部 API 后，将迁移记录登记为基线 1，同步运行时 schema 版本。未执行基线 SQL、未修改业务结构和数据，之后使用单基线镜像及正常启动校验。过程见 [生产基线说明](../../../docs/installation.md#database-baseline)，这不是任意旧库的自动升级入口。

## 初始化与执行

基线直接定义原 0074 SQL 执行完成后的表、视图、函数、触发器、约束和索引，只保留空库需要的初始数据：审计总计、独立缓存/事件 epoch，以及 17 个缓存域各 64 个桶。不包含旧数据回填、旧表的创建再删除、流量模式切换和一次性历史清理。

SQL 基线不是完整运行时初始化。`--migrate-only` 仍完成初始管理员、订阅模板、引用索引、用户访问检查和运行时契约初始化，且不启动后台 worker、不访问远端节点。普通 API 启动只做兼容性检查；维护入口不再执行旧凭据转换、密码回填或旧流量任务接管。

```sh
cargo run -p hysteriax-server -- --migrate-only
```

连接参数和测试环境见 [安装指南](../../../docs/installation.md#database-baseline)。生产发布仍需在本地构建镜像后传输。新安装在空库初始化；已完成当前迁移链的数据库直接启动对应镜像；存在后续迁移时，应先停止全部 API 并执行维护初始化，无须重跑基线 SQL。

## 后续迁移

- 从 `0011_<description>.sql` 继续递增；已发布的 SQL 不再修改、重命名或重排。
- SQLx 内嵌 SQL 并记录 SHA-384 checksum；启动逐项验证完整历史，重复执行维护不会重新应用已成功迁移。
- 应用对象使用相对 schema 名称，以支持隔离测试 schema。`pg_trgm` 仍安装到 `public`，索引显式引用 `public.gin_trgm_ops`。
- 新增或移除对象时同步更新 [数据库表用途](../../../docs/database-tables.md)，并验证当前基线上的后续升级。

## 0002 节点级批量撤权

`0002_node_kick_batches.sql` 将每个节点限制为一个活跃 kick 任务，并增加用户执行代次和分页索引。按用户要求，迁移会直接删除旧撤权需求、kick 任务及相关执行/事件/重试链历史，不转换旧载荷；新执行器不支持旧格式。2026-10-29 已按流程停止 API 并通过 `--migrate-only` 在生产应用 0002，未执行基线 SQL。迁移后 `runtime_contract.schema_version=2`，旧撤权需求与 kick 任务历史已清空；本机发布目录保存了配置和迁移前数据库快照。

## 0003 移除授权预览

`0003_remove_authorization_previews.sql` 删除 `authorization_group_previews` 及所有旧令牌、索引和约束，不删除授权组、成员、节点、连接凭据或撤权需求。已发布的 0001/0002 保持不变，新库按完整迁移链初始化后不再包含预览表。测试环境和部署均使用独立 SQL 迁移，经以下维护命令执行：

```sh
# 停止全部 API 后，使用新镜像执行维护初始化，再启动新版本 API。
./target/debug/hysteriax-server --migrate-only
```

创建、修改、删除和用户所属组提交均不接受 `preview_token`；旧客户端不兼容，需要同步更新。2026-10-29 已按用户要求不备份完成生产迁移；`runtime_contract.schema_version=3`，预览表不存在，授权业务关系数量不变，维护与启动验证通过。

## 0004 分离任务进度和节点实体缓存

`0004_separate_job_cache_roots.sql` 仅替换缓存失效触发函数：任务更新继续推进 jobs 缓存域与任务版本；节点详情本来就依赖 jobs 域，因此不再额外推进节点实体版本。避免任务进度和授权写入在同一节点实体计数上产生无关的 SERIALIZABLE 冲突。保留其他缓存失效、权限数据及既有版本计数；协议仍为 2，维护初始化后 schema 为 4。

测试环境单独迁移工具仍为 `--migrate-only`，新空库按 1～4 完整执行，已完成 1～3 的测试或生产库停止 API 后执行同一工具升级，无须修改历史迁移记录或重跑基线。升级测试保留现有用户/节点/任务并验证缓存刷新及并发授权提交。

## 验证

```sh
# TEST_DATABASE_URL 必须指向可丢弃的 PostgreSQL 数据库。
cargo test --workspace
python3 scripts/verify-migrations.py
```

本次合并在独立 PostgreSQL 18 空库中回放原 SQL 链，再将原结构与基线结构按 PostgreSQL 规范化定义比较；105 张应用表的初始数据也逐表一致，随机 epoch 按生成语义核对。当前测试覆盖原生字段、初始化数据、重复迁移、历史篡改/缺失/失败/未来版本拒绝及旧迁移链拒绝。后续业务测试直接使用当前 schema，不再构建已移除的历史版本。

`0005_async_authorization.sql` introduced user-level connection password fields,
cursor-based authorization changes, indexed delta items, and immediate source
exclusions. Production completed the associated password conversion before the
legacy conversion code was removed. Legacy test databases are retired; the
current server does not backfill old assignment passwords or provide a bridge
upgrade tool. New installations initialize an empty database with the full
current migration chain.

## 0006 用户级连接凭据目录

`0006_user_credential_directory.sql` 将 `credential_directory` 中的 `user_credential` 从每个用户—节点一条改为每个持有连接密码的用户一条，稳定 ID 为 `user:{user_id}`，名称为用户名称。没有节点分配的用户也可见。接口只为当前分页用户统计已分配节点数，通过 `reference_count` 返回，不返回密码、摘要或全部节点列表。仍在用户页执行原有密码轮换。

迁移只替换目录视图，不修改密码、授权关系或已发布迁移。停止全部 API 后使用 `--migrate-only`。0006 本身只影响目录视图；当前新空库按 1～10 初始化。

## 0007 清理历史数据转换

删除服务内的旧凭据转换、旧 mTLS 重置、TLS 证书对合并、用户连接密码回填和旧流量任务接管逻辑。历史 SQL 文件与 checksum 保持不变；0007 对空库完成最终结构的收尾，删除旧字段、已清空的逐节点证书绑定字段和 `credential_migrations` 表，并收紧 SSH 引用和凭据类型约束。

已完成维护初始化的 schema 5/6 数据库可直接停止 API 后执行当前服务 `--migrate-only`。存在未完成历史转换、缺失用户密码或孤立流量批次时，0007 事务失败，不能伪造标记绕过。

生产已应用 0007，旧测试数据库已退役，独立旧库升级工具及依赖旧版本的验证脚本已删除。新环境直接初始化空库；已升级到 schema 7 的库可重复执行当前 `--migrate-only`。不再支持未完成历史转换的旧库升级。

0007 已发布，其 SQL 原文和校验和保持不变。文件中的旧脚本路径仅是当时的错误提示，已不再是有效操作指引，不能据此运行旧工具或伪造转换标记。当前初始化、重复维护、密钥及迁移历史拒绝、恢复隔离由现有 Rust 测试和 `scripts/verify-migrations.py` 覆盖。

当前仍保留凭据引用索引维护、用户访问检查重建、默认模板播种、恢复时的 epoch 隔离与只读运行校验。旧 mTLS 配置回放拒绝和不确定远程操作屏障也继续保留，避免重新执行已失效或结果未知的任务。模板仅支持 schema 2。

## 0008 删除旧监控租约表

`0008_drop_monitoring_leases.sql` 删除 `monitoring_leases`。该表的每节点监控 owner 与到期时间在 2026-10-22 的代理采样重构后已由 `background_work` 的执行租约（`resource_leases`）和 `monitoring_probe_tokens` 取代；此后没有 Rust 或内嵌 SQL、数据库函数、视图、触发器及外键再引用它，线上剩余 4 行是迁移前写入且早已过期的租约。

迁移只有一条 `DROP TABLE`，主键、外键与索引随之删除，不修改其他表、数据或已发布迁移。停止全部 API 后，schema 7 数据库执行当前服务 `--migrate-only` 升级到 schema 8；新空库按 1～8 初始化。schema 8 不能直接切回只识别 1～7 的旧镜像，回退时使用对应数据库快照。

## 0009 统一用户连接与订阅凭据

`0009_unify_user_credentials.sql` 创建 `user_credentials`，原样迁入用户连接密码和全部订阅 Token（包括撤销时间），替换目录视图，再删除旧订阅表和用户密码字段。每用户每种类型最多一份有效凭据，两类秘密保持独立；此迁移不主动轮换任何现有值。已有用户没有订阅时保持未签发，首次联合轮换时补齐。存在同用户多份有效订阅等违反新约束的数据时，迁移事务失败，须先核查数据，不能绕过约束。

2026-11-04 已在生产通过 `--migrate-only` 完成 0008、0009，schema 7 升至 9，原秘密和迁移校验和保持。一次性测试升级脚本在完成验证和部署后移除；保留已发布 SQL 及数据保值测试，不提供运行时旧模型回退。新空库按 1～10 初始化，当前数据库的重复维护仍使用原主密钥执行 `--migrate-only`，不会重新迁入或轮换秘密，也不启动后台工作进程。客户端与服务端须配套升级；schema 9 无旧字段或独立订阅轮换接口。

## 0010 锁争用修复

增加节点套餐到期评估字段和索引，配置修改自动使评估计划失效；既有行先进入待评估状态，由有界维护批次初始化下一次评估时间。仅移除旧全节点扫描维护记录和租约，保留业务配额、凭据及已 admission 的限制/恢复任务。统计刷新改为事务 advisory 锁后移除旧重试链 guard 表。

2026-11-05 已停止生产 API、保存数据库快照，再以新服务 `--migrate-only` 将 schema 9 升至 10。历史 0001～0009 校验和、原主密钥及业务数据摘要保持，生产运行 schema 10。新空库依次执行 1～10；旧镜像不支持 schema 10，不能直接切回 schema 9 镜像。

另提供一次性测试环境工具，迁移前后逐表比较业务值摘要，DDL、迁移记录和运行时版本在同一事务更新，失败整体回滚、重复执行安全：

```sh
# TEST_DATABASE_URL 仅指向已停止所有 API 的测试库。
python3 scripts/migrate-lock-contention-test-db.py \
  --expected-database hysteriax_test \
  --confirm-test-environment --confirm-api-stopped \
  --report /tmp/hysteriax-lock-migration.json
```

该工具不轮换秘密、不启动 worker，也不处理旧历史升级链。
