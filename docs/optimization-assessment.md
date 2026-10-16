# 优化评估报告（2026-10-16）

原报告覆盖 Rust 服务端、PostgreSQL/SQL 层、Swift 客户端、并发与安全、构建/测试/运维工具链。2026-10-16 经二次核查并实施首轮修复，保留原编号用于追溯。条目标记为 **已确认**（源码或回归验证）、**待测量**（机制存在，严重程度尚无充分运行证据）、**工程建议**（策略选择）或 **待核实**（本轮未完整核查）；同一项的量化收益不因机制得到确认而自动成立。工作量 S/M/L 仅为初步估计。

原审计记录了 Clippy、密钥脱敏、常量时间比较、analytics 汇总及共享分页缓存等基线。它们不能证明不存在性能或并发缺陷；“所有依赖均使用”“全程无跨 await 持锁”等全库断言未在本轮重新证明。历史计划和原始验证数据已清理，可从 Git 历史查阅；下文保留当时的评估结论。

---

## 原 P0 分组 —— 已重新评估，不代表现存阻塞

### 1. [已确认·提交检查] 本地模板改动须与引用代码一同提交
`formats.rs` 与新 `singbox-template.json` 均为本地未提交改动。当前 HEAD 不引用新文件，因此不能据此认定已提交版本无法从干净克隆构建。风险是未来只提交引用代码而漏带模板。

**建议：** 提交该功能时同时检查源码与资源的提交完整性；不是当前线上 P0 故障。0042 未修改这组已有改动。

### 2. [已确认·部分修复] CI 覆盖仍有缺口，runner 标签有效
* `xcode-27` 是 GitHub 官方托管 runner 的预览标签，不能据此判定 job 不启动。北京时间 2026-10-16 05:12 启动的 [macos job](https://github.com/nieaowei/HysteriaX/actions/runs/38808001942/job/116673767638) 已成功完成。[官方标签列表](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)。
* 原 workflow 仅接入三个 Swift 校验脚本，0042 新增分页缓存、概览网络、DNS、授权组及 SSH 指纹检查，保留现有 runner 标签。未接入的其他脚本仍需分别判断适用性，不能使用未经核对的“14/16”计数。
* `release-container.yml` 实际使用 build-push-action 构建并推送镜像；缺少常规 PR 镜像冒烟门禁，不能写成“没有任何 workflow 构建镜像”。Compose 已配置 `/readyz` 健康检查，Dockerfile 没有 HEALTHCHECK 不等于部署无健康检查。

### 3. [已确认·工程建议] 发布版本身份需统一
[api/mod.rs:294](../crates/hysteriax-server/src/api/mod.rs#L294) 返回 `service_version: env!("CARGO_PKG_VERSION")` —— 当前 package version 为 `0.1.0`（`Cargo.toml:3`）；[api/mod.rs:295](../crates/hysteriax-server/src/api/mod.rs#L295) 又硬编码 `"hysteria_version": "app/v2.12.3"`，而 [deployment.rs:20](../crates/hysteriax-server/src/deployment.rs#L20) 已经拥有 `HYSTERIA_RELEASE_TAG` 常量。回滚/灰度检查读的正是这个字段，而 Hysteria 版本号还在 `config.rs:57,505,599` 里另有一份。

**修法（S）：** 将发布 tag 注入 `service_version`；Hysteria 版本改为从既有唯一常量派生。

---

## 原 P1 分组 —— 性能风险与待测瓶颈

### 4. [待测量] 全局咨询写锁串行化受保护的写事务
`migrations/0001_initial.sql:178-184` 安装了 `hysteriax_serialize_writes()`，以 `BEFORE … FOR EACH STATEMENT` 触发器形式对 14 张表调用 `pg_advisory_xact_lock(62177345902811)`；而 `db.rs:13,57-64` 在 `begin_write()` 中显式使用**同一个 key**，有多处调用。配合 `max_connections(10)` 与 5 秒 `acquire_timeout`（`db.rs:21-25`），并发写事务会在**占用连接池连接**的情况下排队等待锁。当足够多写请求占用连接等待锁时，读请求可能获取连接超时；应测量排队、事务耗时及错误率，不能仅凭代码确定线上已发生饥饿。

这把锁是有意为之（它保证 `job_events.id` 的提交顺序，可续传 SSE 游标依赖这一点），因此不能贸然移除。

**修法（M/L）：** 将锁收窄到真正需要有序事件 ID 的作业/持久化路径；单独按资源加锁不能保证跨资源事件提交顺序，需要另外设计并验证 SSE 游标协议。在周期性 worker 抢锁前加一道「是否真有活要干」的廉价判断。

### 5. [待测量] node_limits 每轮逐节点拿全局锁
`node_limits.rs:454-474`：`check_all` 选出**所有**非删除中的节点，为每个节点单独开一个 `db::begin_write` 事务，然后 sleep 1 秒。这是几乎从不改变任何状态的幂等状态重算，它是第 4 条的潜在争用来源之一，尚不能断言是生产瓶颈主因。`evaluate`（`:301-388`）还会为每个活跃告警执行一次逐节点 `INSERT … ON CONFLICT`；restricted 标志只在真正变化时才写入。

**修法（M）：** 只挑选需要重新评估的 package（`next_reset_at`/`expires_at` 已到期，或用量越过阈值），把逐节点循环合并为一次基于集合的事务，并拉长间隔。

### 6. [已确认·已修复] 凭据 GET 的引用索引维护扩大写锁占用
列表、详情、引用 GET 原先使用 `begin_write()`。调用链中的 `reference_index::ensure_on()` 会拿同一写锁、解密脏来源、更新引用边；因此原先建议“直接改只读事务”不安全。

**0042 实施：** 在一致性只读快照内检查脏来源；干净时直接查询，脏时结束快照、写事务维护后重试。最多两轮维护后退回原权威写事务。删除等写操作仍在原事务内维护引用，保留历史引用保护。

### 7. [待测量] traffic::apply_sample 在全局锁内逐用户执行 SQL
`traffic.rs:233-300`：一次 `begin_write` 之后，**每个用户**执行 5 条语句（SELECT 用户、SELECT baseline、UPSERT baseline、INSERT `traffic_records`、UPDATE `users`），受限分支还要更多。用户数增加会延长写事务，具体时长需要实测。当前采样循环为 10 秒 tick，不能沿用未经核对的“三个采集器每 60 秒”描述。

**修法（M）：** 改为集合式重写 —— 用 `unnest(...)` 数组做 `INSERT … SELECT`，用一条 `UPDATE users … FROM (…)` 更新用量，踢下线入队改为批量。

### 8. [已确认·已修复] traffic_records 缺少外键查找索引
原表仅有 sampled_at 时间索引；用户/节点级联删除及用户节点组合的最新采样查询缺少匹配索引。0018 的删除触发器含 max(sampled_at) 子查询，但更新有 `sampled_at=OLD.sampled_at` 条件，不能断言每删除一行必定执行全表 max 或给出固定复杂度乘积。

**0042 实施：** 0029 新增 `(user_id,node_id,sampled_at DESC)` 与 `(node_id,sampled_at DESC)` 索引。事务内建索引设置 5 秒锁等待上限，未来部署需停止 API 的维护窗口；附回退脚本及锁冲突原子回滚/重试验证。实际计划与耗时以本地测量为准。

### 9. [待测量] 公开订阅路径在响应缓存前仍查询资格
`api/subscriptions.rs:227-257` 先查 token，再查用户；`:266` 再实时读取并校验每一条 assignment（注释写着「资格校验始终实时」）；`:288-292` 再取缓存域版本。响应缓存是在这之后才被查询的。该路由通过订阅 token 控制访问，不要求管理员鉴权（`api/mod.rs:203-208`，注册在 admin router 之外），且全代码库没有任何限流 —— 高并发请求可能争用连接池，饱和阈值需压测。

**修法（M）：** 若增加资格缓存，必须先定义禁用、到期和撤销的生效窗口与失效协议；对公开路由（也包括节点每次用户鉴权都会调的 `/hy2/auth/…`）加限流。

### 10. [已确认·待设计] 搜索谓词与现有 trigram 索引不匹配
`api/nodes.rs:133` 用 `strpos(lower(n.name), lower($1)) > 0 OR …` 对四个字段做过滤，但相关索引（`0027:1`）是建立在 `hysteriax_search_text(id,name,public_host,ssh_host)` 表达式上的 GIN trigram 索引 —— 表达式不同，而且 `strpos` 本身也不被 pg_trgm 加速。`api/dns.rs:352` 形态相同，且完全没有搜索索引。每个请求会把这个不可索引的谓词跑**两遍**（配对的 `count(*)`）。overview 列表的写法是正确的（`overview.rs:443-444` 用 `… LIKE lower($1) ESCAPE '\'`）。

**修法：** 按现有逐字段字面子串语义设计索引和 LIKE 转义。节点列表搜索含 state，现有表达式含 public_host，两者字段集合不同；不能直接替换并声称不改行为。需覆盖短词、跨字段、%、_、反斜杠及状态匹配。

### 11. [待测量] 列表逐行解密与 DNS 相关子查询
节点预览冷缓存可能逐行解密；DNS 列表的相关子查询包含 `record_ids ? r.id` 及 public_host 比较。存在额外处理成本，实际扫描与耗时需要 EXPLAIN 和冷缓存测量。

**修正建议：** `?` 运算符应评估默认 `jsonb_ops` GIN，`jsonb_path_ops` 不支持该运算符；采用后者必须同时改写查询并证明语义一致。表达式索引、JOIN 改写及并行解密均暂不在本轮实施。[PostgreSQL 官方说明](https://www.postgresql.org/docs/16/datatype-json.html)。

### 12. [待核实] 历史表保留策略
只有 `online_samples`/`proxy_probe_samples` 会被清理（31 天，`proxy_monitor.rs:74-88`，并正确级联到各自的汇总表）。从不清理的：`job_events` —— 它同时是可续传 SSE 日志，每次部署增长 ≥25 行（`deployment.rs:89-141`，`report_progress` 被调用 24 次）外加每个生命周期事件；`audit_records`；终态 `kick_requests`；`job_cache_versions`/`batch_cache_versions`（无外键，作业行删除时也不随之删除）。

**修法（S）：** 在既有批量清理循环中加入 `job_events` 的 id 水位线删除（保证 SSE 游标窗口安全）、版本表的孤儿清理，以及终态 kick request 的年龄过滤。

### 13. [待测量] 连接池容量应由负载决定
当前 max_connections=10、acquire_timeout=5 秒。SQLx 0.8.6 默认已开启 test_before_acquire，idle_timeout=10 分钟、max_lifetime=30 分钟；没有显式配置不代表这些机制不存在。后台并发上限相加也不能证明实际需求必然超过容量。

**建议：** 区分连接获取等待和拿到连接后的咨询锁等待，再决定是否配置容量或做工作负载隔离。0042 通过临时诊断构建记录服务共享池获取连接耗时，并另记独立合成池负载的等待时间，两者分开报告；本轮不扩大连接池、不延长超时。

---

## P2 —— 可靠性与安全

| # | 问题 | 证据 | 修法 |
|---|---|---|---|
| 14 | **[待核实]** worker 死亡是静默且无人监管的：`traffic::run` 用 `tokio::join!` 把采样与**配额/到期强制下线**绑在一起，采样路径一次 panic 就会永久关停超额与过期踢下线；`/readyz` 只跑 `SELECT 1`，完全察觉不到。七个 worker 的句柄只在关闭流程中被触碰。 | `traffic.rs:54-57`、`main.rs:75-117`、`api/mod.rs:298-305` | 两个循环独立 spawn；监管句柄；把 worker 健康纳入 `/readyz`。**M** |
| 15 | **[待核实]** 作业槽位可被无限期占满：`job_is_running` 遇数据库错误以 1 秒间隔无限重试，`retry_persistence` 以 30 秒为上限无限重试，两者都不响应关闭信号 —— 数据库故障期间 4 个槽位全部卡死，整条作业流水线停摆。 | `jobs.rs:129-145, 217-233` | 重试预算有界 → 转 `needs_attention`；响应 `state.shutdown`。**S/M** |
| 16 | **[待核实]** `PATCH /api/v1/nodes/{id}` 在请求处理器内同步发起 SSH 连接（最坏约 75 秒），目标是未经校验的 host:port —— 通过 400/409 的可区分结果构成内网端口探测/SSRF oracle，且没有并发上限。而 `ssh-test` 早已改为后台任务。 | `api/nodes.rs:511-543`、`ssh.rs:97-105,171` | 像 `ssh-test` 一样改为入队作业；拒绝非可路由目标。**M** |
| 17 | **[待核实]** 每个 SSE 客户端每批事件各自执行一次鉴权 `SELECT`，且没有任何机制限制单 token 的并发流数量。 | `api/mod.rs:402-410` | 仅在 `authorization.changed()` 时复查 + 慢速兜底扫描；限制并发流数。**M** |
| 18 | **[待核实]** 管理员 token 永不过期 —— `admin_tokens` 没有 `expires_at`（`0001:1-8`）；`last_used_at` 有记录但从未使用。泄露的 token 在被人工吊销前一直有效。 | `api/mod.rs:247-249` | 增加 `expires_at`，在 `require_admin` 与 SSE 复查中强制。**S** |
| 19 | **[待核实]** 审计无法归因到具体 token：`audit()` 把 `actor` 硬编码为 `'admin'`，而任何 token 都能签发全权限新 token 或吊销他人。 | `api/mod.rs:795-805` 对比 `:787` | 把 `AdminActor.id` 贯穿到 `audit()`。**S** |
| 20 | **[待核实]** 内部错误字符串被拼进 400 响应（约 12 处），可能外泄 provider/校验细节。核心错误映射本身是正确的。 | `api/dns.rs:61-63`、`api/nodes.rs:293,562,1180`、`dns/binding.rs` | 改为安全的固定文案。**S** |
| 21 | **[待核实]** 每次作业失败都会解密**全部**凭据版本：`SELECT payload_enc FROM credential_versions` 没有 `WHERE`/`LIMIT`，随后是 O(密钥数 × 消息长度) 的脱敏。 | `jobs.rs:496-510` | 限定为该作业引用的版本并设上限。**S** |
| 22 | **[待核实]** `proxy_monitor` 使用默认的 `Burst` tick 行为（而 `traffic.rs` 正确设置了 `Skip`）；`free_port()` 在复用前已释放监听，并发探测可能撞端口。 | `proxy_monitor.rs:57-64,174-178` | 设置 `MissedTickBehavior::Skip`；持有监听器。**S** |
| 23 | **[待核实]** 采样任务 panic 会永久拉黑该节点：`inflight` 只在 `Ok` 分支清理。 | `node_limits.rs:486-509` | `Err` 分支同样清理。**S** |
| 24 | **[待核实]** 全库无任何限流，也没有 `cargo-deny`/`cargo-audit` 和 dependabot。 | 全仓库 | 加限流与 `dependabot.yml`。**S** |
| 25 | **[待核实]** 非法的 `HYSTERIAX_REDIS_URL` 会以含义不明的 panic 启动失败，而不是清晰的启动错误。 | `cache.rs:312` | 把错误从 `AppState::new` 传播出来。**S** |
| 26 | **[待核实]** `node_limits::monthly_boundary` 对时区与日期计算使用 `expect`/`unwrap`/`unreachable!`，数据来自 `node_packages.config` JSONB 列。目前不可达（API 边界会校验），但恢复备份或直接 SQL 写入会把坏数据变成 1 Hz worker 内的 panic。 | `node_limits.rs:91-122` | 返回 `Result`/`Option`，或经 `validate()` 反序列化。**S** |

---

## P3 —— macOS 客户端

| # | 问题 | 证据 | 修法 |
|---|---|---|---|
| 27 | **[已确认·后续优化]** `ListPageCache` 每次 `peek` 都在 MainActor 上做 JSON 解码，列表视图存在多次读取缓存的调用路径，实际重算成本待测量；`load` 的 `Task {}` 继承 `@MainActor`，因此编码同样在主线程。被观测的全局 `revision` 计数器会让任何缓存写入失效所有已挂载列表（SSE 每秒最多 +1）。缓存 key 每次访问都用 `try! JSONEncoder()` 构造。 | `ListPageCache.swift:20,31-35,52,68,82`；`ManagementStore.swift:892-903`；`UsersView.swift:76-96,218-249` 及另外 5 个视图 | 缓存中直接存已解码值（类型化 box）；保留字段编码的防碰撞性质，避免直接拼接造成 key 冲突；失效改为显式 token。**M** |
| 28 | **[已确认·已修复]** 修复前，被取消的分页请求可能弹出红色错误横幅：`invalidate()` 会取消所有飞行中的请求（每次 mutation 和每个 SSE 事件都会调用），视图只检查自己的 `Task.isCancelled`，于是用户看到 "CancellationError error 1." 并附带重试提示。 | `ListPageCache.swift:38-53,73,85`；`ManagementStore.swift:195-198,865`；`NodesView.swift:263-272` 等 | 把 `CancellationError`/`URLError.cancelled` 视为静默无操作。**S** |
| 29 | **[已确认·已修复]** 修复前，`refresh()` 没有单飞保护：15 秒定时器、SSE 循环（≤1/s）、19 条 mutation 路径与 `ContentView` 会并发调用它，而 `isLoading` 由最先结束的调用清除。 | `ManagementStore.swift:195-241,881-887,853-879`；`ContentView.swift:140` | 0042 已合并执行及强制刷新后续轮次，isLoading 覆盖队列；缓存失效范围保留原规则。**M** |
| 30 | **[待核实]** 分页结果会覆盖共享的目录数组，导致浏览过筛选页之后，其他界面显示截断的节点 ID、丢失逐 assignment 的「证书…」按钮、以及错误的离线/总览计数。 | `ManagementStore.swift:336,354`；`UsersView.swift:491,722`；`OverviewView.swift:7-20,153-158` | 分页结果不要写入目录数组；总数用 `clientSummary`。**M** |
| 31 | **[已确认·范围修正]** 部分核心接口失败会导致整次刷新失败；overview、DNS、monitoring 已有局部降级，不能泛指任意接口。核心接口失败 → 全局「未连接」状态，并弹出每 15 秒重复出现的模态框，同时禁用所有写入 UI 直到某次刷新完全成功。 | `ManagementStore.swift:697-746,236-240`；`ContentView.swift:144-150` | 连接状态改为粘性（连续 N 次失败才翻转）、按接口降级、错误改用内联横幅。**S/M** |
| 32 | **[待核实]** `ProxyConfigurationView` 是单个约 1400 行的视图，`body` 约 375 行，且完整草稿 YAML 预览在每次按键时被急切重建（参数非 autoclosure）。 | `ProxyConfigurationView.swift:96-471,438-441,1576-1592,521-729` | 拆分为各 section 子视图；预览用防抖写入 `@State`。**M** |
| 33 | **[待核实]** 六套几乎相同的分页列表实现（各约 40 行）已经出现漂移 —— `DNSRecordsView` 以 `store.dnsUpdatedAt` 而非 `dataRefreshToken` 作为 key，因此错过了正常的刷新节奏。 | 上述 7 个视图文件 | 统一为泛型 `ListPageController<Page>` + 共享分页控件。**M** |
| 34 | **[工程建议·事实修正]** `APIClient` 未自定义业务超时策略，Foundation 默认请求等待超时为 60 秒；缺少统一重试/退避与传输错误翻译；只有 SSE 做了退避。中文界面里会直接出现英文 `URLError` 文案。 | `APIClient.swift:39-51,217-233`；`ManagementDisplayText.swift:180+` | 按业务区分超时；GET 重试、waitsForConnectivity 须定义等待预算。URLError 映射到 L10n。[Apple 默认超时](https://developer.apple.com/documentation/foundation/urlsessionconfiguration/timeoutintervalforrequest)。**M** |
| 35 | **[待核实]** 后台任务从不暂停：窗口关闭后 SSE + 15 秒轮询 + 各视图 15/60 秒轮询仍在运行，且 SSE 循环强持有 `self`，store 永远不会被释放。整个 Sources 中没有任何 `scenePhase`。 | `ManagementStore.swift:849-888,853-855`；`UsersView.swift:167-178`；`OverviewHistoryView.swift:105-128` | 按场景/窗口活跃度门控循环；循环内改用弱引用重新获取 `self`。**M** |
| 36 | **[待核实]** 分页 DTO 住在一个上帝对象里（44 个可观测属性、约 95 个成员），存在三套冗余的事实来源。 | `ManagementStore.swift:19-71,43-44,264-271,336,354` | 拆成 session 对象背后的各功能 store。**L** |
| 37 | **[待核实]** 低危项：缓存 key 的 `try!`/`!` 与 `mtls!`（`ManagementStore.swift:893,898,535`）、`prepareNode` 中静默的 `catch {}`（`:812`）、每次 body 重算派生集合（`OverviewView.swift:7-20,153-158`）、非惰性的 200 行 `ForEach`（`OverviewMonitoringView.swift:60-73`）、以及主线程上 20 MiB 文件读取 + base64（`ProxyConfigurationView.swift:1050-1067`、`ManagementStore.swift:440`）。 | 如上 | 基本为机械性改动。**S/M** |

---

## P4 —— 工程基础设施

| # | 问题 | 证据 | 修法 |
|---|---|---|---|
| 38 | **[待核实]** `apps/macos/Package.swift:7` 声明 `.macOS(.v27)`，而 Xcode 工程、`docs/compatibility.md` 与 `docs/release.md` 写的是 macOS 26 —— 从未在 CI 构建过的 SwiftPM 路径与文档承诺的 Intel 兼容下限自相矛盾。 | `Package.swift:7` 对比 `project.pbxproj:438` | 对齐到 `.v26`，或明确让 SwiftPM 仅支持 27。**S** |
| 39 | **[工程建议·事实修正]** 原有部分迁移包含大表回填或 GIN 建索引，升级成本需以数据量、执行计划及实际锁模式判断。SQLx 仅执行未应用的迁移，不能声称每次启动都回填，也不能笼统认定整个过程均持有 ACCESS EXCLUSIVE。0042 后共有 29 个 SQL 迁移，并新增一个回退脚本。 | `migrations/*`、SQLx 0.8.6 migrator | 在线建索引需另行处理非事务执行、失败残留及回退；本轮 0029 选择停 API 维护窗口，事务建索引并设置 5 秒锁等待上限。 |
| 40 | **[待核实]** `jobs` 表有 13 个索引（12 + 主键），其中一个 9 列 trigram GIN 包含 `stage`，而每次部署的 24 个进度点都会重写 `stage`。 | `0025:8-13`、`0001:155-156`、`deployment.rs:111-119` | 从 trgm 表达式中移除 `stage`/`status`，或进度只写入 `job_events`。**M** |
| 41 | **[待核实]** 每次目录写入会触发 5–6 个触发器，其中三个都在更新同一行 `cache_generation`；quota-rank 还会为每个变更行发一次 `pg_notify`。 | `0016:4-62`、`0020`、`0028:3-23` | 用 transition table 做语句级合并。**M** |
| 42 | **[待核实]** 六份分页样板代码（已经漂移：jobs 默认 `desc`，nodes 默认 `asc`）；所有列表都用 `OFFSET` + 实时 `count(*)`（凭据目录还按拼接表达式排序，任何索引都无法服务）；Hysteria 版本字符串重复出现在 5 处。 | `api/nodes.rs:97-116`、`api/users.rs:74-84`、`api/credentials.rs:151-161`、`api/dns.rs:314-323`、`api/mod.rs:481-504,610-620` | 共享 `PageQuery`/keyset 分页；版本号单一常量。**S/M** |
| 43 | **[工程建议·修正建议]** 数据库集成测试需要可丢弃 PostgreSQL；不能用某文件没有内联单元测试证明其没有被跨模块测试覆盖。0042 新增一个默认忽略、由本地性能脚本显式执行的测量入口；普通全量测试仍要求数据库。其他迁移脚本的 CI 覆盖待逐一核查。 | `db.rs`、`credentials/tests.rs`、`traffic/optimization_benchmark.rs`、`ci.yml` | 可拆分单元和集成测试入口；CI 缺数据库必须失败，不能默默跳过集成测试。 |
| 44 | **[工程建议]** CI 成本：未过滤的 `push` + `pull_request` 让仓内 PR 重复跑两遍；没有 `concurrency:`、没有 `permissions:`、没有 `timeout-minutes`，且 `cargo check` → `test` → `clippy --all-targets` 存在不同检查与构建产物的重复成本，但 Cargo 会复用缓存，不能等同三次完整编译。 | `ci.yml:3-5,36-39` | 加 concurrency group、只读权限，去掉冗余的 `check`。**M** |
| 45 | **[待核实]** 57 个脚本 / 约 8.5k 行，没有共享 harness（`free_port` ×6、`request` ×7、`wait_ready` ×3、固定版本二进制下载+摘要逻辑散落 5 个以上文件）；mihomo 摘要有重复且已与 `deploy/mihomo-v1.19.31.sha256` 不一致（`verify-mihomo-config.sh:19-32` 校验的资源该文件并未列出）。 | `scripts/*` | 建立 `scripts/lib/` + 单一版本来源。**M** |
| 46 | **[待核实]** `verify-singbox-config.sh` 未接入任何 workflow，而 sing-box 输出是已文档化并会真实渲染的格式（目前只有本地的 `subscription_formats_test.py` 覆盖）。 | `ci.yml`、`README.md:58-70` | 接入 CI。**S** |
| 47 | **[待核实]** 全新安装会得到 root 属主的 `./data/assets` 缓存挂载（`init-secrets.sh:23` 只创建 `backups/`，API 以 `${HYSTERIAX_UID:-1000}` 运行），而 `artifact_cache.rs:78-87` 在写入失败时静默地不缓存 —— 每次节点部署都会永久重复下载 10–30 MiB。 | `compose.yaml:43,53-54`；`init-secrets.sh:23` | 创建并 chown 该目录；缓存不可写时至少记录一次日志。**S** |
| 48 | **[待核实]** 非 Rust 代码完全没有 lint（无 swift-format/swiftlint、ruff、shellcheck、markdown 链接检查 —— 当前已有 5 个死链）；没有任务运行器（README 列了约 20 条命令）；24 个 shell 脚本中 20 个在参数错误时不打印用法；`scripts/verify-subscription.sh` 其实是 Python 程序却用了 `.sh` 扩展名。 | `ci.yml:36-39`；`scripts/` | 加上低成本 linter 与 `Makefile`；重命名错扩展名的脚本。**M** |
| 49 | **[待核实]** 文档/仓库整洁度：历史计划引用已在文档清理中修正，实现状态文档已删除；本项其余内容仍为原审计记录：测试计数过期（106/109/120/126，实际 212）；3 个孤儿脚本；`.gitignore:10` 忽略了并不存在的 `apps/macos/GeneratedAPI/`，而真正生成的 DTO 是被跟踪的；空的 `.env.tmp`。 | 如上 | 核实测试计数与孤儿文件。**S** |
| 50 | **[待核实]** `[低-中]` 长效节点 token 走 `/hy2/auth/{node_id}/{node_token}` 的 URL 路径（会进入代理/CDN/浏览器日志）；所有 admin 路由的 body 上限为 30 MiB；`cache.rs:227` 的 `.unwrap()` 可证明安全但缺少保护；`tokio` 使用了 `full` feature 集。 | `api/mod.rs:205-208,190` | token 改走 header、收紧 body 上限、收窄 feature。**S** |

---

## 修订后的执行顺序

1. 0042：纠正文档；修复 #6、#8、#28、#29；接入相关 CI 校验并完成本地回归。
2. 用固定数据测量 #4、#5、#7 的锁等待、采样耗时及接口分位数；#13 的池测量明确区分合成负载和服务池。结果见验证数据，不预设扩容或改锁结论。
3. 后续单独设计 #10 搜索语义、#27 缓存表示、#30 目录状态、#31 失败降级；本轮没有据此改变产品行为。
4. 其他待核实项完成源码与运行证据核查后再定优先级。令牌过期、SSRF 目标策略、历史清理及全局锁重构需要独立兼容性设计。

## 本轮本地验证结论

0042 的 Rust 全量测试 220 项通过，另一个默认忽略的采样测量入口在两套独立 schema 显式执行通过；相关 Swift 脚本、严格 Clippy、模型一致性及 macOS 构建启动通过。CI 已加入脚本，本轮没有推送代码触发新的远程 workflow。

100 节点、1000 用户、100000 条初始流量记录的单次本地 Debug 对照中，采样期间凭据列表 P95 为 1792.83→30.17 ms，最新用户节点采样查询为 7.656→0.077 ms。真实逐用户采样事务仍约 1.6–1.8 秒；服务共享池在采样及合成争用期间获取连接耗时 P95 为 1.232→2.023 ms，没有证明持续连接池饥饿。首轮修复有效，但不能据此推荐扩容或宣称采样算法已优化。

升级、回退、锁冲突原子失败/重试及用户/节点级联删除验证通过。条件、原始分位数、执行计划和限制可从 Git 历史中的 0042 计划与验证数据查阅。

## 结论可信度与限制

本报告保留原审计编号，但撤回“全部复核、仅一处推断”的声明。已确认只证明对应机制或回归，不证明生产严重程度、固定工期或收益。没有本地证据支持的全库计数及生产性能断言不应作为排期依据。

0042 验证使用本地可丢弃 PostgreSQL schema、固定种子和本地 Debug 二进制。首轮请求不等于清空操作系统/数据库缓存的冷启动；SQLx 获取连接日志含服务共享池的 HTTP 与后台工作，不等于纯信号量等待；独立池的合成争用另行报告。上述 0042 本地验证阶段未连接测试服务器或部署镜像。后续已完成后端部署（原记录编号 0043），验证中未创建测试业务记录或调整业务配置、配额。
