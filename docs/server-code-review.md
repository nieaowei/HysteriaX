# HysteriaX 服务端代码审查报告

## 一、审查范围与基线

- **审查范围**：`crates/hysteriax-server/`（约 6 万行非测试 Rust/SQL 源码，32 个模块 + 15 份数据库迁移）及 `crates/hysteriax-template-validator/`。
- **静态检查**：`cargo clippy --workspace --no-deps` 通过，**0 warning**。
- **容量假设**（遵循 [AGENTS.md](file:///Users/nekilc/Downloads/HysteriaX/AGENTS.md)）：**10 万用户、1 万节点**。

---

## 二、总体评估

服务端整体工程成熟度较高，在**分布式执行围栏与租约（Fencing & Lease）**、**破坏性流量清零与计费安全**、**幂等事务封装**、**数据库膨胀治理**及**基础安全卫生**方面设计扎实。

但在 **10 万用户 / 1 万节点** 的目标规模下，仍存在若干**会导致 OOM、连接池耗尽、节点自残隔离（Quarantine）或吞吐严重瓶颈**的严重问题，集中在五个方面：
1. **全表加载 / N+1 查询与内存放大**（失败脱敏扫全表 `credential_versions`、mTLS 订阅 N+1、`reconcile_node` 全量加载 `node_assignments`、模板渲染 O(N²) 与全量重校验）。
2. **关键热路径缺少缓存与索引**（`/hy2/auth` 5 次串行 DB 查询、`node_assignments` 缺少 `(node_id, user_id)` 索引、`credentials(owner_user_id)` 无索引、`cache::middleware` 持有 DB 事务等待 Redis 与 Single-flight）。
3. **执行正确性与停机安全缺陷**（`wait_for_startup` 对只读轮询使用写日志的 `execute_checked` 导致超时自隔离、`main.rs` 停机直接 `abort()` `jobs`/`mtls`、`ssh/journal.sh` FIFO 死锁）。
4. **1 万节点 / 10 万用户规模下的串行/限速瓶颈**（Job 并行度硬上限 4、mTLS 轮换与吊销每秒仅处理 1 项/1 页、Cloudflare 每次建记录全量翻页拉取 Zone）。
5. **安全纵深加固项**（全局 30 MiB Body 限制 + YAML 锚点膨胀 DoS、`proxy_probe_url` SSRF、`SshNode` `Debug` 泄露凭据、密钥未做 HKDF 域分离等）。

---

## 三、问题清单（按严重程度分级）

### Critical（严重：高概率引发 OOM、雪崩、死锁或节点误隔离）

#### C1. 任务失败脱敏 `safe_error()` 每次扫描并解密整张 `credential_versions` 表和节点全部用户凭据
- **位置**：[jobs/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/jobs/mod.rs#L1200-L1225)
- **问题**：每次任务失败时，`safe_error()` 都会：
  1. 查询并解密该节点关联的所有有效 `user_credentials`（单节点可达 10 万条）；
  2. 执行无 `WHERE`、无 `LIMIT` 的 `SELECT payload_enc FROM credential_versions`，解密数据库内**全部历史与当前凭据版本**（10 万用户 mTLS 证书/私钥 + 1 万节点探针/CA/SSH/DNS 密钥，轻松超 20 万行），递归提取全部字符串做替换。
- **影响**：任一网络抖动导致多任务同时失败时，立即触发数十万次 AEAD 解密与数百 MB 内存峰值，极易导致 OOM 与数据库连接池枯竭。
- **建议**：仅加载当前任务实际接触的敏感上下文（`node_token`、`traffic_stats_secret`、该节点通过 `credential_reference_edges` 实际引用的凭据及 `desired_config_enc`），严禁全局扫描 `credential_versions` 和 `user_credentials`。

#### C2. `wait_for_startup()` 对只读状态轮询使用带日志的 `execute_checked()`，超时取消会导致节点自隔离
- **位置**：[deployment/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/deployment/mod.rs#L955-L979)、[deployment/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/deployment/mod.rs#L1223-L1262)、[execution/remote.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/execution/remote.rs#L94-L130)
- **问题**：`wait_for_startup` 在 12s `timeout_at` 内每秒轮询一次 `systemctl show`，但调用的是写操作专用的 `session.execute_checked()`（每次执行 3 次 SSH `head.sh`/`journal.sh`/`inspect.sh` + 2 次 DB 事务 + 插入 `execution_operations`）。一旦 12s 超时在 `dispatch_prepared`（`state='dispatching'`）与 `lease.verified()` 之间取消 Future，会留下悬空的 `dispatching` 记录，导致 1 秒后的下一次轮询以及后续 `rollback_managed_node()` 全部被拒绝，最终永久将 `node:{id}` 置为 `blocked=true`（隔离）。同理，`free_remote_tcp_port()`（[deployment/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/deployment/mod.rs#L1650)）、漂移校验（`L592`）与诊断读取（`L1036`, `L1477`）也误用了写日志执行。
- **建议**：将所有只读远端检查切换为 `session.execute_read_only()` / `execute_read_only_checked()`，仅保留当前租约围栏校验而不写 `execution_operations` 与远端 journal。

#### C3. `node_assignments` 缺少 `(node_id, user_id)` 索引（在迁移 `0005` 被隐式删除）
- **位置**：[0005_async_authorization.sql](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/migrations/0005_async_authorization.sql#L7)、[0001_baseline.sql](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/migrations/0001_baseline.sql#L2663-L2668)
- **问题**：迁移 `0005` 删除 `credential_hash` 列时隐式删掉了 `UNIQUE (node_id, credential_hash)`，但未补建以 `node_id` 为前导列的索引。当前仅存主键 `(user_id, node_id)`。
- **影响**：在千万级 `node_assignments` 行下，所有 `WHERE node_id = $1`（节点限额踢人分页、节点详情/更新/删除、mTLS 签发、`nodes` 级联删除）均退化为全表/全索引扫描。
- **建议**：补充迁移创建 `CREATE INDEX node_assignments_node_user_idx ON node_assignments (node_id, user_id);`。

#### C4. `cache::middleware` 在查 Redis 和等待 Single-flight 期间持续占用 `REPEATABLE READ` 数据库连接
- **位置**：[cache/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/cache/mod.rs#L409-L445)、[cache/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/cache/mod.rs#L196-L223)、[read_snapshot/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/read_snapshot/mod.rs#L82-L114)
- **问题**：每个可缓存的 `GET` 请求一进入中间件就先从连接池签出连接并开启 `REPEATABLE READ, READ ONLY` 事务，随后在持有该 DB 连接的状态下执行 Redis I/O（最长 150ms）以及等待 Single-flight `OnceCell`（最长 20s）。
- **影响**：默认连接池仅 `10` 条连接（[runtime_config/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/runtime_config/mod.rs#L3-L4)），10 个并发读请求命中冷 Key 即可把连接池全部占满（9 条连接空等 Leader），直接饿死写请求与后台任务，同时钉住 PostgreSQL `xmin` 阻碍 `VACUUM`。
- **建议**：先用短查询读取缓存版本号，**释放 DB 连接后**再查 Redis / 参与 Single-flight；仅在真正 Cache Miss 进入 `compute` 闭包时开启 `read_snapshot` 事务。同时去掉 `scope_with` 中冗余的 `SELECT current_setting(...)` 往返。

#### C5. 订阅托管 mTLS 生成存在严重 N+1 查询爆炸
- **位置**：[api/subscriptions/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/api/subscriptions/mod.rs#L609-L627)、[credentials/identities.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/credentials/identities.rs#L298-L333)
- **问题**：`load_subscription_nodes_snapshot` 遍历用户的每个托管 mTLS 节点，逐个调用 `identities::for_node()`，每次触发 **3 次串行 SQL** + 重复解密同一份用户证书与私钥。
- **影响**：用户分配 1,000～10,000 个 mTLS 节点时，单次订阅未命中即产生 3,000～30,000 次串行 SQL 查询，必然触发 20s 超时并打满连接池。
- **建议**：通过单条批量 SQL（按 `(user_id, node_ids[], ca_ids[])`）一次性查出有效身份映射，并按唯一 `(credential_id, credential_version)` 去重后批量加载解密。

#### C6. `/hy2/auth/{node_id}/{node_token}` 热路径每次连接触发 5 次无缓存串行 DB 查询
- **位置**：[api/subscriptions/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/api/subscriptions/mod.rs#L683-L766)
- **问题**：Hysteria2 节点每次用户建连都会回调 `hy2_auth`，当前串行执行 5 条 SQL：`nodes` 查询 → `deployment_probe_tokens` 5 表 Join → `monitoring_probe_tokens` 5 表 Join → `node_limits::restricted` → `node_assignments/users/user_credentials` 3 表 Join + `hysteriax_authorized()`。
- **影响**：10 万用户规模下发生重连风暴时，5 次串行 DB 往返叠加默认 10 连接池会迅速触发级联 `503 database_pool_busy`。
- **建议**：通过前缀短路探针 Token 查询（普通用户连接直接跳过两次 5 表探针 Join），并为节点状态/限流标志引入基于 `cache_domains` 版本或短 TTL 的内存缓存。

#### C7. 订阅模板渲染存在 O(N²) 算法放大、全量重校验及异步线程 CPU 阻塞
- **位置**：[templates/render.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/api/subscriptions/templates/render.rs#L152-L198)、[templates/catalog.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/api/subscriptions/templates/catalog.rs#L197-L199)（及 `L252-L276`, `L325-L368`）
- **问题**：
  1. `render.rs:152` 在循环内使用 `!selected.iter().any(|s| s.id == n.id)` 线性去重，1 万节点 × 多分组达 $10^8$ 次比较；
  2. 注入 1 万个节点后，每次下发仍对完整展开的文档执行 `platform_diagnostics`（按规则数重复遍历整棵 JSON 树）和 `catalog::validate`/`references`（每访问一个值就递归深拷贝 schema 子树）；
  3. 上述重 CPU 计算与 `serde_yaml::to_string` 全部直接运行在 Tokio 异步工作线程上。
- **影响**：单次大订阅渲染即可阻塞 Tokio Worker 数秒至数十秒，拖垮健康检查与 `hy2_auth`。
- **建议**：将下发渲染与预览诊断路径拆分；去重改用 `HashSet<&str>`；对模板结构在发布期校验、下发期跳过对服务端生成代理段的重复 schema/diagnostics 扫描；将渲染与 YAML 序列化卸载至有界并发的 `spawn_blocking`。

#### C8. `reconcile_node()` 与节点更新/删除在单事务内全量锁定并加载所有关联用户及凭据
- **位置**：[credentials/authorization.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/credentials/authorization.rs#L231-L310)、[api/nodes/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/api/nodes/mod.rs#L838-L846)（及 `L1056-L1064`）
- **问题**：
  1. `reconcile_node()` 在部署完成 `SERIALIZABLE` 事务内取出节点所有 `user_id`（可达 10 万），再执行 `SELECT user_id, node_id FROM node_assignments WHERE user_id = ANY($1)`，把 10 万用户在全部 1 万节点上的数百万条分配记录拉入 Rust 内存。
  2. 节点更新/删除对全量关联 `users` 加 `FOR NO KEY UPDATE`，再以 10 万元素 `ANY($1)` 查 `credentials WHERE owner_user_id = ANY($1) FOR UPDATE`，而 `credentials.owner_user_id` **无索引**，引发全表扫描与大面积锁冲突。
- **建议**：
  - 补建 `CREATE INDEX credentials_owner_user_idx ON credentials (owner_user_id, id) WHERE owner_user_id IS NOT NULL;`；
  - 将 `reconcile_node()` 改为 SQL 集合更新或分页后台任务，脱离部署提交事务；节点变更通过 `node_authorization_versions` 异步收敛，避免一次性锁定 10 万用户行。

#### C9. 进程停机时直接 `abort()` `jobs` 与 `mtls` Worker，引发节点大面积误隔离
- **位置**：[main.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/main.rs#L140-L155)、[jobs/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/jobs/mod.rs#L20-L56)
- **问题**：收到 SIGTERM 时，`main.rs` 会优雅排空采样 Worker（120s），但对 `jobs` 和 `mtls` 直接调用 `worker.abort()`。任何处于 `dispatch_prepared` 到 `verified` 之间的在途 SSH/DNS 任务会被中途截断，重启恢复逻辑看到悬空的 `state='dispatching'` 后会将对应节点/DNS Zone 永久标记 `blocked=true`（隔离）。
- **建议**：让 `jobs::run` 订阅 `state.shutdown`，收到停机信号后停止认领新任务并等待在途 `JoinSet` 排空（保留 `heartbeat` 存活），同时给 `mtls`、`node_limits`、`sampling_expiry` 等 Worker 提供优雅排空窗口后再兜底 `abort()`。

---

### High（高危：严重性能瓶颈、数据/可靠性缺陷或安全隐患）

#### H1. `GET /overview` 全量加载 1 万节点（含原始 `online_connections` JSONB）与 10 万用户，且 `issues` 无上限
- **位置**：[overview/summary.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/overview/summary.rs#L116-L124)、[overview/summary_nodes.sql](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/overview/summary_nodes.sql#L1-L53)、[overview/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/overview/mod.rs#L165-L174)（及 `L387-L458`）
- **问题**：每次打开概览页都会拉取全部 1 万节点的完整行 + 2 万次 `LATERAL LIMIT 1` + 全部 `online_connections` JSONB 在 Rust 中构建 `BTreeSet`；无 `LIMIT`、无索引全表扫描 10 万 `users`（缺 `expires_at`/`usage_bytes >= quota_bytes` 局部索引）；`issues` 数组无分页上限；`summary_failures.sql` 在 `job_retry_state_dirty` 非空时回退为全量 24h 递归 CTE。
- **建议**：KPI 计数与在线去重下推 SQL 聚合；为即将到期/超额启用用户建立部分索引；对 `issues` 实施 Top-N 截断或分页。

#### H2. `overview::record_online_on` 在持有 `nodes` 行锁时全量拉取该节点的 `node_assignments`
- **位置**：[overview/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/overview/mod.rs#L62-L76)
- **问题**：每次流量采样都会在 `FOR NO KEY UPDATE` 锁住 `nodes` 行后，执行 `SELECT user_id FROM node_assignments WHERE node_id=$1` 拉取全部最多 10 万用户到 Rust `BTreeSet`，仅用于过滤 `/online` 返回的少量在线用户。
- **建议**：先解析 `/online` JSON 提取在线 `user_id` 列表，再查 `WHERE node_id=$1 AND user_id = ANY($2::text[])`。

#### H3. `analytics/probe_history.sql` 在交互查询中对全窗口原始 `proxy_probe_samples` 计算精确 `percentile_cont`
- **位置**：[analytics/probe_history.sql](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/analytics/probe_history.sql#L22-L29)
- **问题**：1 万节点每 60s 探测一次，30 天累积约 **4.32 亿行**。查询 7d/30d 延迟 p50/p95 时直接对原始表排序计算 `percentile_cont`，必然触发磁盘临时文件溢写与 20s 超时。
- **建议**：在 `probe_minute_totals` 与小时汇总表中存储固定对数桶直方图（或 t-digest），基于汇总表计算近似分位数。

#### H4. `background::ensure_cycle` 每 10–60 秒在单个 `SERIALIZABLE` 事务内锁全表 1 万节点并批量插入 2 万行
- **位置**：[background/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/background/mod.rs#L32-L78)
- **问题**：每周期对所有符合条件的 `nodes` 执行 `FOR KEY SHARE` 并一次性写入 1 万行 `background_work` + 1 万行 `online_samples`，在 `SERIALIZABLE` 下引发剧烈 SIREAD 锁冲突与重试放大。
- **建议**：改用 `READ COMMITTED` + `sampling_cycles(kind, period_start)` 唯一约束防重，并分批写入 `background_work` / `online_samples`。

#### H5. 后台并发与限速天花板过低，无法支撑 1 万节点 / 10 万用户运维
- **位置**：
  - [jobs/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/jobs/mod.rs#L14-L22)（`min(DEFAULT_MAX_PARALLEL_NODES = 4)`）
  - [node_limits/maintenance.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/node_limits/maintenance.rs#L169-L181)（`processed` 未计入 `dispatch` 导致每 128 用户睡 1 秒，10 万用户需 13+ 分钟）
  - [traffic/access.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/traffic/access.rs#L96-L98)（循环逐条调 `admit_on` 而未用已有的 `admit_many_on`；`dispatch_pass` 每秒仅认领 1 页）
  - [credentials/identity_maintenance.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/credentials/identity_maintenance.rs#L36-L79)、[credentials/rotations/users.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/credentials/rotations/users.rs#L20-L62)（每秒最多签发 1 个用户，10 万用户轮换需 **27.8 小时**）
- **建议**：移除 `jobs` 硬编码 `.min(4)` 上限；修正 `node_limits` 与 `traffic::access` 的 `processed`/突发排空逻辑并使用 `admit_many_on`；mTLS 签发/轮换循环改为队列非空时持续有界并发消费（`JoinSet`）。

#### H6. Cloudflare DNS 在建记录、自动分配主机名与凭据校验时全量分页拉取 Zone，且 `commit_zone` 逐条写库
- **位置**：[dns/worker/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/dns/worker/mod.rs#L114)、[dns/binding.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/dns/binding.rs#L103-L108)、[dns/rotation.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/dns/rotation.rs#L256)、[dns/worker/refresh.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/dns/worker/refresh.rs#L246-L276)
- **问题**：1 万节点的 Zone 有 100+ 页记录。每次 `record-create` 或 HTTP 请求自动分配主机名都调用全量 `provider.records(zone)`，批量建 1 万节点会产生百万级 Cloudflare API 请求直接触发限流；`commit_zone` 在单事务内逐条执行 2–4 次 SQL（2–4 万次往返）。
- **建议**：在 `dns/provider.rs` 增加按 `?name=`、`?comment=` 与 `?per_page=1` 过滤查询；`commit_zone` 改用 `unnest`/`jsonb_to_recordset` 批量 upsert。

#### H7. 租约续期与远端完成确认将瞬时 DB 错误视为致命丢失，易触发误隔离
- **位置**：[execution/supervision.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/execution/supervision.rs#L71-L86)、[execution/remote.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/execution/remote.rs#L121-L130)
- **问题**：`supervision::renew()` 仅重试 `55P03`，遇到 `PoolTimedOut`/`Io`/`57P01` 等瞬时 DB 错误会立刻判定 `Supervised::Lost` 并直接取消正在执行的 SSH/DNS Future；`remote.rs` 在远端调用成功后仅尝试一次 `lease.verified()`，无瞬时错误重试，导致已成功的远端操作进入隔离态。
- **建议**：区分明确的租约丢失（`Ok(false)`）与瞬时 DB 错误，在租约有效期内做短退避重试；对 `lease.verified()` 复用类似 `jobs/persistence.rs::retry_persistence` 的重试逻辑。

#### H8. `ssh/journal.sh` 在远端命令输出超过 `cap + 64KiB` 时发生 FIFO 死锁
- **位置**：[ssh/journal.sh](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/ssh/journal.sh#L91-L107)
- **问题**：父 Shell 通过 `exec 6<>"$op/stdout.fifo"` 同时持有了 FIFO 读端与写端，直到命令结束后才关闭 `fd 6/7`。当 `head -c $((cap+1))` 读满退出后，由于父进程仍持有读端，远端命令写满管道缓冲区后不会收到 `SIGPIPE` 而是永久阻塞直至 60s SSH 超时，将可控的 `OutputLimitExceeded` 变成不确定的超时隔离。
- **建议**：在 `head` 后接 `cat >/dev/null` 排空剩余输出，或在 `head` 打开 FIFO 后关闭父进程的读端。

#### H9. 全局 30 MiB Body 限制 + YAML 锚点合并膨胀 DoS
- **位置**：[api/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/api/mod.rs#L301)、[templates/native_source.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/api/subscriptions/templates/native_source.rs#L5-L10)
- **问题**：`DefaultBodyLimit::max(30 MiB)` 挂载在全部管理路由上（而非仅 `/nodes/{id}/resources`），所有写接口都会先反序列化再重序列化完整 JSON 计算幂等摘要；`native_source::yaml` 直接对用户输入调用 `serde_yaml::from_str` + `value.apply_merge()`，缺乏别名/节点数上限，易受 Billion Laughs 内存膨胀攻击。
- **建议**：管理路由默认 Body 上限收紧至 1–2 MiB，仅对资源上传路由放宽至 30 MiB；对模板源接口增加输入大小与展开节点数限制。

#### H10. SSE 事件流无并发连接上限，且每个空闲连接每秒轮询 3 次 DB
- **位置**：[api/event_stream/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/api/event_stream/mod.rs#L155-L249)
- **问题**：无全局/单 Token 并发连接限制；空闲时每 1 秒唤醒一次开启 `REPEATABLE READ` 事务并执行 3 条 SQL。几十个空闲前端标签页即可产生上百 TPS 打垮默认 10 连接池。
- **建议**：增加全局与单 Actor 并发连接 `Semaphore` 限制；将空闲兜底唤醒间隔拉长至 30–60s，优先依赖 `EventHub` 共享推送。

#### H11. 多处管理查询端点无上限加载 & 用户停用/删除在 `SERIALIZABLE` 事务内逐条入队 1 万个踢人任务
- **位置**：
  - [api/client_summary.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/api/client_summary.rs#L7-L28)（无 `LIMIT` + O(N×M) `retain` 去重）
  - [api/users/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/api/users/mod.rs#L491-L535)（每页 200 用户加载全量 `node_assignments`，单响应可达数百万行）
  - [api/users/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/api/users/mod.rs#L244-L253)（及 `L296-L305`, `L345`：`patch_on`/`delete_on` 锁全量节点并循环逐条 `enqueue_job`，未复用 `rotation.rs` 的批量 `kick_requests::enqueue_pairs`）
  - [api/credentials/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/api/credentials/mod.rs#L107-L113)（及 `L342-L434`：`references` 11 条无上限查询、`reference_count` 在 Rust 计数、`batch_values_on` N+1）
  - [cache_domains/cache_batch_version.sql](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/cache_domains/cache_batch_version.sql#L1-L11)（每次查缓存 Key 都会对整批 1 万条明细做 `jsonb_agg` + `md5`）
- **建议**：上述端点全部补齐分页/聚合下推；`users::patch_on`/`delete_on` 改为 `kick_requests::enqueue_pairs`；`cache_batch_version.sql` 改用已有的 `batch_cache_versions.generation`。

#### H12. 高基数外键缺少索引、`job_status_totals`/`audit_totals` 单行锁与死锁风险、`node_network_samples` 遗漏语句级删除触发器
- **位置**：
  - 外键缺索引：[0001_baseline.sql](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/migrations/0001_baseline.sql#L2798)（`traffic_baselines(user_id)`、`nodes(mtls_activation_id)`、`mtls_deployment_snapshots(activation_id)`、`mtls_node_operations(base_activation_id/rotation_id/node_id)`、`mtls_cleanup_nodes(node_id)`、`mtls_rotation_nodes(node_id)`）
  - 触发器死锁/热点：[0001_baseline.sql](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/migrations/0001_baseline.sql#L44-L74)（`hysteriax_job_totals_changed` 按 `OLD` 再 `NEW` 顺序锁 `job_status_totals`，`queued<->running` 并发易触发 AB-BA 死锁 `40P01`；`audit_totals` 单行热点）
  - 遗漏语句级触发器：[0001_baseline.sql](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/migrations/0001_baseline.sql#L3551)（`node_network_samples` 仍为行级 `AFTER DELETE` 触发器，未随迁移 `0013`/`0015` 升级为语句级）
- **建议**：补齐上述非局部 B-tree 索引；`job_status_totals` 按 `ORDER BY status` 确定性加锁（并考虑分桶计数）；将 `node_network_samples` 删除触发器升级为语句级 `REFERENCING OLD TABLE`。

#### H13. `quota_rank` 重建在 `self.gate` 锁内发送 4 万参数 Redis Lua 脚本，且受多实例时钟偏差震荡
- **位置**：[quota_rank/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/quota_rank/mod.rs#L171-L239)、[quota_rank/quota_rank.lua](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/quota_rank/quota_rank.lua#L28-L32)（及 `L65-L88`）
- **问题**：缓存失效时持有 `self.gate` 互斥锁阻塞所有并发读请求，并向单线程 Redis 发送 4 万 ARGV（约 7 万条命令）的原子 Lua 脚本；且使用本地 `Utc::now()` 而非 DB `clock_timestamp()`，多实例时钟哪怕偏差数毫秒也会触发 `tonumber(reset) > now` 反复全量重建。
- **建议**：统一使用 DB `clock_timestamp()`；拆分为分批写入影子 Key + `RENAME` 原子切换，避免单条巨型 Lua 脚本卡死 Redis 与阻塞读路径。

#### H14. `proxy_probe_url` 未禁止私网/回环/云元数据地址（SSRF 风险）
- **位置**：[api/nodes/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/api/nodes/mod.rs#L1401-L1427)、[proxy_monitor/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/proxy_monitor/mod.rs#L404-L422)
- **问题**：仅校验了 `http` 协议与无凭据/查询串，未拦截 `127.0.0.1`、`::1`、`localhost`、`169.254.169.254` 或 RFC1918 内网段。
- **建议**：默认拒绝非公网 IP 字面量与 `localhost`，并在探测前校验解析结果（除非通过显式服务端配置放行）。

#### H15. 订阅缓存 Key 计算在每次命中时仍做重 DB/JSON 开销，且存在 TOCTOU 二次渲染
- **位置**：[api/subscriptions/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/api/subscriptions/mod.rs#L440-L535)
- **问题**：即便 100% 命中缓存，每次拉取订阅仍查询并反序列化全部节点的 `Package` JSON；且 `boundary <= Utc::now()` 的检查位于 `get_until` 之后，边界时刻会触发完整二次加载与渲染。
- **建议**：将节点级套餐限流位图全局缓存（按节点配置版本 + 时间窗口），并将 `boundary` 检查前移至 `get_until` 之前。

---

### Medium（中危：架构债务、边界 Bug 与纵深安全）

1. **高容量时序表未分区且采用随机 UUID 主键**（[0001_baseline.sql](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/migrations/0001_baseline.sql#L1792-L1943)）：`traffic_records`、`traffic_sample_items`、`node_network_samples`、`online_samples`、`proxy_probe_samples` 在 10 万用户 / 1 万节点下日增亿级行，建议按 `sampled_at` 做时间范围分区（`DROP TABLE` 替代 `DELETE`）并使用时间有序 ID（UUIDv7/`bigint`）。
2. **`after_release` 每次归还连接均执行额外 SQL 往返 & 默认连接池（10）小于默认后台并发（16）**（[db/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/db/mod.rs#L52-L68)、[runtime_config/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/runtime_config/mod.rs#L3-L12)）：建议仅对可能残留事务状态的连接执行 `ROLLBACK`，并调高默认连接池上限。
3. **本地 `Cache<T>` 在持有 `std::sync::Mutex` 期间三次序列化 JSON、可能超限 `max_bytes`、`routes` 统计 Map 无界、Redis 熔断缺少半开单探针**（[cache/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/cache/mod.rs#L205-L255)、[redis_cache/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/redis_cache/mod.rs#L31-L97)）。
4. **搜索过滤使用不可走 `pg_trgm` 索引的 `strpos(lower(...))` + 深分页 `OFFSET`**（[api/users/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/api/users/mod.rs#L81)、[api/nodes/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/api/nodes/mod.rs#L132)、[api/authorization_groups/reads.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/api/authorization_groups/reads.rs#L61)）：建议统一改为 `LIKE lower($pattern) ESCAPE '\'` 配合 `pg_trgm` GIN 索引。
5. **多处只读接口拆分为多次独立的 `read_snapshot::begin` 事务，破坏快照一致性**（[api/users/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/api/users/mod.rs#L354-L440)、[node_limits/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/node_limits/mod.rs#L277-L299)、[api/dns/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/api/dns/mod.rs#L333-L347)、[api/resources/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/api/resources/mod.rs#L199-L212)）。
6. **`overview::package_warning` 告警 Key 拼写不匹配**（[overview/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/overview/mod.rs#L117) 匹配 `"expiry_warning"`，而 [node_limits/evaluation.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/node_limits/evaluation.rs#L38) 写入 `"expiring"`），导致前端直接暴露原始 `"expiring"` 字符串。
7. **`background::run_sampling` 仅在 1 秒定时器触发时补充槽位，而非任务完成即补充**（[background/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/background/mod.rs#L337-L378)），将单实例速率人为压低至 16 节点/秒。
8. **`proxy_monitor` 存在本地临时端口 TOCTOU 竞态、每次探测新建 `reqwest::Client`、以及在 Tokio 线程上直接执行阻塞 `std::fs` I/O**（[proxy_monitor/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/proxy_monitor/mod.rs#L313-L317) 及 `L451-L466`）。
9. **安全卫生细节**：
   - `SshNode` 派生了 `#[derive(Debug)]`，包含明文 SSH 密码/私钥与口令（[ssh/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/ssh/mod.rs#L16-L25)）。
   - SSH 回环 HTTP 客户端未过滤 `\r`/`\n`（CRLF 注入防御），且 `decode_chunked` 中 `size + 2` 未做防溢出检查（[ssh/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/ssh/mod.rs#L625-L631) 及 `L782`）。
   - `SecretBox` 将同一 32 字节主密钥直接用于 AEAD、HMAC 与 SHA-256 指纹且未 `zeroize`（[security/secrets.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/security/secrets.rs#L8-L26)）。
   - 订阅选择 HTML 页包含原始订阅 Token，缺少 `X-Frame-Options: DENY` 与 `Content-Security-Policy`（[api/subscriptions/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/api/subscriptions/mod.rs#L375-L401)）。
   - 脱敏规则在 `templates/store/mod.rs` 与 `security/redaction.rs` 重复定义且不完整（未覆盖 URL 内嵌凭据及 `api_key`/`client_key`/`passphrase`/`cookie`）。
10. **订阅代理名 8 字符 UUID 前缀碰撞风险**（[api/subscriptions/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/api/subscriptions/mod.rs#L830)）：1 万同名节点生日碰撞概率约 1.16%，碰撞会直接导致整份订阅 `422 template_invalid`。
11. **`GET /api/v2/subscription-templates/fields` 每次请求深拷贝并遍历修改 1.7 万行 Schema JSON**（[templates/catalog.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/api/subscriptions/templates/catalog.rs#L54-L79)），建议 `LazyLock` 预计算缓存。
12. **`dns/probe/mod.rs` 阻塞读 `/etc/resolv.conf` 且 `lookup_host` 无超时**（[dns/probe/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/dns/probe/mod.rs#L71) 及 `L189`）；**`rotations::status_on` 做 `mtls_rotation_nodes × node_assignments` 笛卡尔级 Join**（[credentials/rotations/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/credentials/rotations/mod.rs#L509-L516)）；**`overview::history_at` 拉取全量 30 天 `data_gaps` 在 Rust 中 O(gaps × buckets) 匹配**（[overview/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/overview/mod.rs#L758-L776)）。
13. **巨型单文件与生产路径 `.unwrap()` / 静默吞错**：`deployment/mod.rs`（1,993 行，单函数 866 行）、`api/nodes/mod.rs`（1,555 行）、`jobs/mod.rs`（1,280 行）；多处 `.unwrap()`（`deployment/mod.rs:803`、`dns/binding.rs:112`、`credentials/worker/persistence.rs:50`、`execution/finalization.rs:165`）及静默忽略 `activation::uncertain` 与 `execution::supervise` 的 `RecoveryRequired` 结果。

---

### Low（低危：代码清洁度与边缘场景）

- **冗余索引与未校验约束**：`monitoring_probe_tokens_expiry_idx` 与 `monitoring_probe_tokens_history_idx` 完全重复；`traffic_sample_batches_capture_deadline` 被 `traffic_sample_batches_capture_expiry_idx` 严格前缀覆盖；`reference_edge_*` 约束仍处于 `NOT VALID`；`cache_*_pending` 事务临时表可改为 `UNLOGGED` 减少 WAL 写放大；`after_connect` 3 次 `set_config` 可合并为 1 次往返。
- **边界 Panic / 死循环风险**：`runtime_config::database_timeouts()` 使用懒加载 `assert!`（[runtime_config/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/runtime_config/mod.rs#L80-L95)）；`node_limits` 对 DB 时区使用 `.expect("validated timezone")`（[node_limits/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/node_limits/mod.rs#L89)）；`traffic::finish_captured_for_removal` 的 25ms 自旋等待缺少超时与 shutdown 检查（[traffic/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/traffic/mod.rs#L112-L121)）；`EventHub::subscribe` 后台轮询任务未监听 shutdown 且使用 `.lock().unwrap()`（[events/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/events/mod.rs#L42-L87)）。
- **其他**：`GET /openapi.yaml` 每次请求重算 SHA-256 且暴露在公网路由（[api/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/api/mod.rs#L451-L480)）；`node_limits::update_usage_on` 混用 DB `clock_timestamp()` 与本地 `Utc::now()`（[node_limits/mod.rs](file:///Users/nekilc/Downloads/HysteriaX/crates/hysteriax-server/src/node_limits/mod.rs#L339-L353)）；`/overview` 缓存 TTL 设为 3s 但 Key 中拼了秒级 `quota_second` 实际退化为 1s。

---

## 四、工程亮点与良好实践

1. **严谨的分布式围栏与提交终结机制**（`execution/`、`ssh/journal/`）：基于数据库时间的 `(owner, generation, epoch)` 围栏、`lease_set` 排序加锁防死锁、`finalization.rs` 提交期暂停续租并二次校验最终行、以及具备 `flock`/前驱链/禁符号链接的 SSH Journal v2 协议，工程水准很高。
2. **破坏性流量清零与计费防丢设计**（`traffic/`、`main.rs`）：先持久化 `clear_requested_at` 意图再清零、带瞬时错误重试的 `seal_clear`、崩溃恢复跳过 SSH 直接续跑 500 用户计费分页、SQL 层 `saturating` 溢出防护，以及停机 120s 排空采样 Worker。
3. **一致的跨模块锁顺序与数据膨胀治理**（`migrations/0011`–`0015`、`background/history_retention/`）：全链路遵循 `node -> batch -> sorted users -> baseline` 锁序；分片 Keyset 清理 + `SKIP LOCKED` + 采样避让（`sampling_headroom_at`）+ 延迟排序缓存版本刷新触发器（`DEFERRABLE INITIALLY DEFERRED`）。
4. **扎实的幂等与基础安全边界**（`api/idempotency/`、`credentials/ca.rs`、`error/mod.rs`、`hysteriax-template-validator/`）：`SERIALIZABLE` 幂等回执加密存储、构建期编译固定 JSON Schema、日志路径脱敏、常量时间 Token 比较、禁用重定向防 DNS Token 泄露、以及 `Sqlx` 零额外往返诊断层。

---

## 五、建议修复优先级路线图

- **P0（立即修复，防止 OOM、雪崩与节点误隔离）**：
  1. 修复 `jobs::safe_error()` 全表扫描解密 `credential_versions` / `user_credentials`（C1）。
  2. 将 `wait_for_startup()` 等只读 SSH 检查改为 `execute_read_only()`（C2），并修复 `ssh/journal.sh` FIFO 死锁（H8）。
  3. 补建 `node_assignments(node_id, user_id)`、`credentials(owner_user_id)` 及关键外键索引（C3、C8、H12）。
  4. 改造 `cache::middleware` 避免持 DB 事务等 Redis/Single-flight（C4），并优化 `/hy2/auth` 与 mTLS 订阅 N+1（C5、C6）。
  5. 为 `jobs`/`mtls` 补齐优雅停机排空，并给 `supervision::renew` / `lease.verified()` 加上瞬时 DB 错误重试（C9、H7）。
- **P1（面向 10w 用户 / 1w 节点的容量与稳定性升级）**：
  1. 重构模板渲染去重/校验/卸载 `spawn_blocking`（C7）与 `reconcile_node`/节点更新的锁扇出（C8）。
  2. 优化 `GET /overview`、`record_online_on`、`probe_history.sql`、`ensure_cycle` 与 `quota_rank` 重建（H1–H4、H13）。
  3. 解除后台吞吐天花板（`jobs` 并发、`node_limits`/`traffic::access`/mTLS 轮换限速、Cloudflare 过滤查询与批量 upsert）（H5、H6）。
  4. 修复 `job_status_totals` 锁序、`node_network_samples` 语句级删除触发器、以及高基数管理接口分页（H10–H12）。
- **P2（安全加固与可维护性）**：
  - 收紧全局 Body 上限与 YAML 展开上限（H9）、拦截 `proxy_probe_url` 内网 SSRF（H14）、修复 `SshNode` `Debug` 与 `SecretBox` HKDF 域分离、统一脱敏规则、修复 `package_warning` 拼写 Bug、拆分 `deployment/mod.rs` 等巨型文件并清理生产 `.unwrap()`。
