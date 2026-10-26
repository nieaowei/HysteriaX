# 2026-10-26 生产数据库事务异常分析

## 结论与置信度

主要触发路径已获得针对性复现：令牌使用时间更新的 50ms 外层超时，会撞上 SQLx 0.9.0 在事务启动被取消时的连接状态失配。已在本地 PostgreSQL 18 上稳定复现与生产相同的错误组合：

1. `BEGIN` 已发往数据库，等待响应时 Future 被取消。
2. 数据库保持事务开启；SQLx 的事务深度仍为 0，`is_in_transaction()` 返回 false。
3. 连接回到池中，被另一个操作复用，沿用原来的事务及隔离级别。
4. 已执行查询后再次使用不同隔离级别的 `BEGIN`，产生 `25001`；事务进入失败状态。
5. 同一连接继续被借出，持续产生 `25P02`，直到实际执行 ROLLBACK 或连接关闭。
6. 残留 SERIALIZABLE 事务也能使后续操作遭遇 `40001` 读写依赖冲突。

“机制可导致全部核心现象”已经实测；“生产每一条错误都由该机制引发”尚不能证明。生产日志缺少 backend PID、事务开始/提交阶段和取消来源，无法将每条 40001 归到某个事务。

本次仅分析、只读查询生产；未重启服务、修改配置或更新镜像。

## 生产证据

日志窗口：2026-10-26 **17:41:07—18:24:24，Asia/Shanghai**（原日志 UTC 09:41:07—10:24:24）。共 1,788 行。以下按匹配日志行计数，结构化 Debug 错误也计入：

| 现象 | 行数 |
| --- | ---: |
| `there is already a transaction in progress` | 482 |
| `there is no transaction in progress` | 69 |
| `could not serialize ...` | 154 |
| `current transaction is aborted ...` | 493 |
| `SET TRANSACTION ISOLATION LEVEL must be called before any query` | 5 |

关键时间线：

- 17:41:08 服务开始监听。
- 17:41:09 首次出现重复 BEGIN 提示。
- 17:41:10 首次出现序列化读写依赖冲突。
- 17:46:35.382，proxy sampling cycle 报隔离级别设置时机错误。
- 17:46:35.480 起，probe renewal、identity rotation、job maintenance 等多个模块连续报事务已中止。
- 后续 17:51:57、17:54:26、18:12:25、18:17:08 再次出现隔离级别错误。

生产二进制的编译路径字符串确认 `sqlx-core-0.9.0`、`sqlx-postgres-0.9.0`。

数据库只读检查：

- 数据库版本：PostgreSQL 18.6 / PolarDB 18.6.5.0。
- `default_transaction_isolation = read committed`，不是实例默认 SERIALIZABLE。
- 18:28 左右的活动快照出现 `idle in transaction (aborted)`；另有事务持续约一分钟、续租和节点锁等待。
- 数据库统计 `deadlocks = 0`；这不能用来统计 SSI 序列化失败。
- 活跃管理令牌在 **18:28:31** 的 `last_used_at` 仍为 **16:19:38**，相隔 2 小时 8 分钟；期间管理接口持续被读取。
- 实际运行环境：池上限 **32**，获取连接/锁/语句/空闲事务超时均为 **60 秒**，后台每队列并行度 **4**，执行租约 **180 秒**。`production/AGENTS.md` 的池上限 16 与当前值存在差异。

活动快照只代表取样时刻。短暂的 idle in transaction 本身不能直接证明泄漏；失败事务状态、重复 BEGIN 和本地复现应结合判断。

## 代码中的触发与扩散路径

### 管理令牌使用时间更新：针对性复现的主要触发路径

`crates/hysteriax-server/src/api/mod.rs:371` 对 `token_usage::touch` 整体施加 **50ms** 的 Tokio timeout。

`crates/hysteriax-server/src/api/token_usage.rs:4` 实际执行 `db::write`，包含 BEGIN、SET LOCAL、SELECT FOR UPDATE、可能的 UPDATE，以及 COMMIT。`SET LOCAL lock_timeout='1ms'` 仅限制数据库锁等待，不能保证全部网络往返在 50ms 内结束。

令牌超过一分钟未使用就再次尝试更新；更新持续失败会使后续每次请求都走这个取消路径。生产 last_used_at 长时间未推进与此吻合。后续用原始 touch 源码和实测生产延迟复现，证实该路径可以连续留下残留事务；生产每一次取消的具体阶段仍需额外日志追踪。

### 后台执行监督：另一类取消入口

`crates/hysteriax-server/src/execution.rs:473` 的 supervise_scope 使用 `tokio::select!`。续租返回 false、数据库错误或所有权失效时，work Future 被丢弃。

如果 work 正在等待 BEGIN/COMMIT 的响应，就可能进入同一驱动取消窗口。日志中 43 条 renewal lost（含 false 和数据库错误）说明取消入口确实活跃；其中一部分也可能是连接异常导致的后果。

### 驱动与连接池

本机依赖源码 `sqlx-postgres-0.9.0/src/transaction.rs`：

- begin 先发送 BEGIN，并 await `wait_until_ready()`。
- 成功返回后才将 `transaction_depth` 加一。
- 取消时的 Rollback guard 调用 start_rollback；后者仅在 depth > 0 时排队 ROLLBACK。
- 因而 BEGIN 等待阶段取消，depth = 0 时可以漏掉 ROLLBACK。

`sqlx-postgres-0.9.0/src/connection/mod.rs` 的 ping 采用协议 Sync；它能确认连接还可通信，却不会自动回滚已失败事务。

`sqlx-core-0.9.0/src/pool/connection.rs` 默认回池检查使用该 ping。项目 `db::pool_options_for_maintenance` 未增加针对实际事务状态的清理策略。

上游曾记录相同取消问题与重复 BEGIN/COMMIT 表现：[SQLx issue #2805](https://github.com/transact-rs/sqlx/issues/2805)。本次判断以本地 0.9.0 实测为主，不依赖旧 issue 推断版本是否修复。

## 为什么已有重试仍会持续报错

`crates/hysteriax-server/src/db.rs:98` 和 `:145` 已对 `40001`、`40P01` 提供最多 **6 次尝试**，并带退避。不能将问题概括为“没有实现事务重试”。

- SERIALIZABLE 下本来就可能产生合法的 40001，需要从新事务完整重试。
- 连接上的残留事务会让下一次 BEGIN 无法建立预期的新事务/隔离级别，破坏重试的前提。
- 25P02 表示前一个错误已使该事务失败；继续在同一事务里重发 SQL 不会恢复。
- 生产的一部分 40001 也可能来自正常业务热点竞争；当前日志不足以量化两者占比。
- 超时、连接污染、续租失败、取消工作可能形成反馈，扩大采样和维护任务的错误面。

## 本地隔离复现

临时复现工程：`/tmp/hx-db-investigation/repro/`；输出：`/tmp/hx-db-investigation/repro-output.txt`。使用 SQLx **=0.9.0**，连接本地 hx-uitest-pg PostgreSQL 18；未使用生产库运行实验。

核心步骤：连接建立后，将 `conn.begin_with("BEGIN ISOLATION LEVEL SERIALIZABLE")` Future poll 到 Pending，立即 drop；随后 ping、读取隔离级别并检查驱动状态。

实测输出摘要：

```text
initial isolation=read committed, driver_in_transaction=false
cancelled begin while pending=true
after cancelled begin isolation=serializable, driver_in_transaction=false
begin attempt=1: 25001 / SET TRANSACTION ISOLATION LEVEL must be called before any query
query attempt=1: 25P02
query attempt=2: 25P02
query attempt=3: 25P02
after explicit rollback query_succeeded=true

pool begin pending=true
reused connection isolation=serializable, driver_in_transaction=false
pooled next begin error=25001
pool reacquire attempt=1 error=25P02
pool reacquire attempt=2 error=25P02
pool reacquire attempt=3 error=25P02
```

另用两个残留 SERIALIZABLE 连接建立相同快照、更新不同记录，稳定得到：

```text
40001: could not serialize access due to read/write dependencies among transactions
Reason code: Canceled on identification as a pivot, during write.
routine: OnConflict_CheckForSerializationFailure
```

因此无需 PolarDB、线上业务表或高负载，也能复现全部核心错误类别。复现证明机制成立，不能替代生产逐事务追踪。

## 建议处理顺序

1. **修复取消时的连接生命周期。** 覆盖 BEGIN、COMMIT、ROLLBACK 等待窗口。取消或状态不确定时，应确保连接实际回滚，或直接淘汰连接；仅依赖 is_in_transaction() 不够，因为该值在复现中已经错误。可评估驱动修补或应用层持有连接的取消守卫。
2. **调整令牌使用时间更新。** 避免用 50ms timeout 直接取消完整事务；可使用去重的后台更新，并在数据库侧限制等待。仅把 50ms 调大不能修复其他取消入口。
3. **补诊断字段。** 对 40001/25P02/25001 记录 SQLSTATE、detail、routine、事务操作名、尝试次数、backend PID、取消阶段及关联任务/请求；不得记录令牌或完整敏感绑定值。
4. **再处理业务竞争。** 在连接状态可靠后，定位剩余 40001 的具体事务/共享行，缩短事务、统一锁顺序，按业务正确性选择隔离级别；保留有界重试。
5. **发布后回收旧连接并验证。** 按仓库要求本地构建容器镜像后传输服务器；观察至少 10–15 分钟，检查重复 BEGIN、25001、25P02 是否消失、last_used_at 是否推进、续租及采样是否恢复。单纯重启只能临时清理旧连接。

以上为分析与建议，尚未实施生产修复。


## 继续排查：生产延迟与原始令牌更新路径（19:23–19:27）

### 从生产 API 服务器实测数据库往返

在美国服务器临时运行已有 postgres:18-bookworm 镜像中的 psql，走相同 Docker 网络和 DATABASE_URL；只执行只读事务、SET LOCAL、SELECT、ROLLBACK，容器退出即删除。未修改生产应用或数据库配置。

13 次独立 SQL 往返的结果：最小 **25.590ms**，最大 **26.846ms**，中位数 **25.841ms**。只读实验的每次 BEGIN 也约 **25.8ms**。

SQLx 0.9.0 的 PoolOptions 默认 `test_before_acquire = true`；借出空闲连接前先执行 ping。这意味着：

```text
借连接时 ping ≈ 26ms
BEGIN 等待响应 ≈ 26ms
两者合计 ≈ 52ms > token_usage 外层 timeout 50ms
```

取消因此可以精确落在 BEGIN 已发送、响应未被驱动处理的窗口，不需要整个事务先发生慢 SQL 或锁等待。

### 使用原始 touch 源码做延迟对照

临时工程 `/tmp/hx-db-investigation/latency-repro/` 通过 Rust `#[path]` 直接引用仓库 `api/token_usage.rs`，使用同等的 READ COMMITTED 事务封装（没有执行租约 scope），连接本地 PostgreSQL。TCP 代理仅在数据库返回方向增加 26ms 延迟。为强制检查同一连接的复用，实验池上限为 1；生产池上限 32 并不能消除该取消窗口。

实验只写本地独立 schema；完成后删除 schema。三组各 3 次：

| 对照条件 | timeout | 令牌更新时间 | 数据库连接状态 | 驱动事务状态 |
| --- | --- | --- | --- | --- |
| 默认借出前 ping，50ms 预算 | 3/3 超时 | 3/3 未更新 | **3/3 idle in transaction** | **false** |
| 关闭借出前 ping，50ms 预算 | 3/3 超时 | 3/3 未更新 | 3/3 idle | false |
| 默认借出前 ping，500ms 预算 | 0/3 超时 | 3/3 已更新 | 3/3 idle | false |

500ms 对照中，实际更新耗时 **190–218ms**。首次预热 touch 为 **262–294ms**。

关闭 ping 是定位取消阶段的实验对照，不是生产修复方案：它仍不能保证更新时间推进，也没有解决其他路径取消 BEGIN/COMMIT 的问题。500ms 也是验证预算问题的对照，不代表只调整常量就能根治驱动取消安全性。

这些结果将“令牌更新是嫌疑触发器”推进为“已用原始代码、匹配的网络延迟复现具体触发路径”。不应再将 25P02 简单归为普通并发冲突或数据库默认隔离级别问题。

### 业务热点与证据边界

19:26 左右的只读锁快照显示 SIReadLock 涉及 background_work、background_work_expiry_idx、resource_leases、resource_leases_work_expiry_idx、cache_version_epoch 及其主键。

还捕获到 `INSERT INTO resource_leases ... ON CONFLICT DO NOTHING` 等待 transactionid 锁；另一条事务曾停留在 node_assignments 读取约 7 秒。两秒后的取样已无满足长事务/锁等待条件的会话，且 pg_blocking_pids 未给出稳定阻塞链。

这些快照说明调度、租约与缓存版本共享数据存在交叠，但不足以把每条 40001 归到某条 SQL。优先清除取消造成的事务残留，随后用事务标签、连接 PID 与 SQLSTATE/detail 统计剩余冲突，避免在异常连接状态下调整业务隔离策略。


## Deployed fix (2026-10-26)

The fix is deployed as hysteriax-server:tx-cancel-20261026-r1. See [implementation and production verification](../plans/0037-fix-transaction-cancellation.md). Statements above about a pending fix describe the earlier investigation phase.


## Current fix: application-level pool reset

The dependency patch was withdrawn at the user's request. Production now runs app-pool-20261026-r2 with unmodified official SQLx. See [0038 implementation and verification](../plans/0038-application-pool-transaction-reset.md). Earlier 0037 describes a superseded historical release.
