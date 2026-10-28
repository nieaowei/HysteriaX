# 数据库迁移

当前迁移链从 `0001_baseline.sql` 开始，仅支持新建空 PostgreSQL 数据库。原 0001～0074 SQL 迁移已合并并删除，不保留历史升级链、兼容识别或迁移记录替换逻辑。旧库及其备份应使用匹配的旧服务版本；不能通过修改 `_sqlx_migrations` 接入本基线。

## 初始化与执行

基线直接定义原 0074 SQL 执行完成后的表、视图、函数、触发器、约束和索引，只保留空库需要的初始数据：审计总计、独立缓存/事件 epoch，以及 17 个缓存域各 64 个桶。不包含旧数据回填、旧表的创建再删除、流量模式切换和一次性历史清理。

SQL 基线不是完整运行时初始化。`--migrate-only` 仍按顺序完成初始管理员、凭据格式及其字段约束、订阅模板、引用索引、工作队列和运行时契约初始化，且不启动后台 worker、不访问远端节点。普通 API 启动只做兼容性检查。

```sh
cargo run -p hysteriax-server -- --migrate-only
```

连接参数和测试环境见 [安装指南](../../../docs/installation.md#database-baseline)。生产发布仍需在本地构建镜像后传输，使用独立空库；这次整理不执行部署。

## 后续迁移

- 从 `0002_<description>.sql` 继续递增；已发布的 SQL 不再修改、重命名或重排。
- SQLx 内嵌 SQL 并记录 SHA-384 checksum；启动逐项验证完整历史，重复执行维护不会重新应用已成功迁移。
- 应用对象使用相对 schema 名称，以支持隔离测试 schema。`pg_trgm` 仍安装到 `public`，索引显式引用 `public.gin_trgm_ops`。
- 新增或移除对象时同步更新 [数据库表用途](../../../docs/database-tables.md)，并验证当前基线上的后续升级。

## 验证

```sh
# TEST_DATABASE_URL 必须指向可丢弃的 PostgreSQL 数据库。
cargo test --workspace
python3 scripts/verify-migrations.py
```

本次合并在独立 PostgreSQL 18 空库中回放原 SQL 链，再将原结构与基线结构按 PostgreSQL 规范化定义比较；105 张应用表的初始数据也逐表一致，随机 epoch 按生成语义核对。当前测试覆盖原生字段、初始化数据、重复迁移、历史篡改/缺失/失败/未来版本拒绝及旧迁移链拒绝。后续业务测试直接使用当前 schema，不再构建已移除的历史版本。
