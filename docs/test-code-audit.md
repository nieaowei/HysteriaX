# 测试代码清查（2026-10-28）

原始审计基于提交 `9857301` 的已跟踪代码。随后按用户要求执行 Rust 清理：删除 3 个夹具自测、47 处失效 SQL、50 组分散的旧锁持有/释放；将 3 个执行计划测试改为显式性能专项，并保留集中及代表性旧锁回归。以下审计位置与统计描述清理前代码，实施及验证结果见 [0007 实施记录](plans/0007.md)。

后续补充清理：两份旧迁移验证脚本已删除，共减少 245 行；未发现运行入口引用，不影响当前基线验证。

**补充结论：Rust 确实存在值得精简的测试负担。** 第一轮主要按代码等价筛查，不能据此推导出这些测试都值得长期保留。按维护收益重新审视后，发现测试夹具自测、已失效的旧触发器处理、散布的旧锁验证和绑定执行计划的断言。具体候选见第 8 节。

## 结论

| 分类 | 结果 | 建议 |
| --- | --- | --- |
| 已删除的过时验证 | 2 个脚本，245 行 | 已移除旧升级链的事后验证工具 |
| 性能实验混在测试目录 | 2 组，共 4 个文件、225 行 | 移至 benchmark；合并公共测量代码，保留不同场景 |
| 已失修的测试入口 | 模板 workbench 的 `--client` 模式 | 修复编译依赖，保留测试 |
| 未接入常用自动入口 | 3 个测试文件及 1 个独立 Go 测试模块 | 补运行入口，不因未引用而删除 |
| 无 Redis 时被误报为通过 | 5 个 Rust 测试 | 保留断言，显式报告未执行并配置 Redis 专项 |
| Rust 整段重复测试 | 静态筛查未确认可直接删除的重复项 | 相似并发用例仍覆盖不同故障条件 |

## 1. 已删除的旧迁移验证

### `scripts/verify-credentials-migration.py`（166 行）

- 文件开头明确说明：比较升级前后数据库，验证旧内联 SSH/resources、上一版凭据中心及破坏性 mTLS reset。
- 第 44～45 行读取 `config_resources`；第 62～72 行复现旧 mTLS 清理规则；第 80～83 行处理旧内联 SSH 字段。这是已结束的升级路径验证，不是当前空库初始化测试。
- [迁移说明](../crates/hysteriax-server/migrations/README.md)明确取消原 0001～0074 升级链，只支持新建空库；旧库应使用匹配的旧服务版本。
- 清理前未发现工作流、运行脚本或业务代码引用该脚本。
- **已删除整个文件。** 当前初始化及重复迁移验证继续由 `scripts/verify-migrations.py` 和 Rust 数据库测试承担。

### `scripts/verify-tls-pairs-migration.py`（79 行）

- 第 2～5 行说明其用途是对比 TLS-pair 迁移前后副本。
- 第 62 行排除 `credential_migrations` 等旧迁移对象，第 67 行断言消除独立 `certificate` / `private_key` 类型，第 68～75 行验证转为同一 TLS identity 的引用。
- 它验证已经完成、且历史 SQL 链已移除的一次性数据转换；当前基线不能用于重演该旧链。
- 清理前未发现工作流、运行脚本或业务代码引用该脚本。
- **已删除整个文件。** 如需维护历史版本，使用对应 Git 版本中的脚本。

两项删除理由是受测升级路径已经退出当前支持范围；“没有进入 CI”只是辅助证据。删除后与[部署测试说明](deployment-testing.md)所述原迁移链升级专项已移除的状态一致。

## 2. 可整理的重复性能基础设施

| 文件组 | 代码位置 | 实际用途 |
| --- | --- | --- |
| overview resize | `tests/overview-resize.swift:5`、`scripts/verify-overview-resize.sh:1` | 创建窗口、重复缩放并渲染，输出平均值和 p95 |
| main split resize | `tests/main-split-resize.swift:5`、`scripts/verify-main-split-resize.sh:1` | 创建窗口、重复缩放并渲染，输出测量结果 |

两组都没有性能阈值或回归比较，性能变差本身不会令命令失败；它们是手工 benchmark，不能作为性能回归通过的证明。两组重复了窗口创建、预热、采样、排序和 JSON 输出逻辑。

**建议：移至专门的 benchmark 目录，并抽取公共测量入口。暂不直接删除任何一整组。** overview 组测 overview / traffic / quality / jobs，main split 组测授权组、节点、用户、任务、DNS 和凭据详情；后者没有覆盖 overview 的全部场景，而且两组计时区间也不同，历史数字不能直接互换。

`tests/detail-header-layout.swift` 和 `tests/vertical-split.swift` 有实际布局断言，包括标题/操作区重叠、条件控件变化、分隔条位置保持等，应保留，不能与这些 benchmark 一起删除。

## 3. 过时入口需要修复，测试仍有价值

### 模板 workbench 的 `--client` 模式无法通过类型检查

位置：`scripts/verify-subscription-template-ui.py:81`。

脚本手工列出 Swift 文件编译 `tests/subscription-template-workbench-live.swift`。本次按它原样列出的文件执行 `swiftc -swift-version 6 -parse-as-library -typecheck`，失败并报告：

```text
SubscriptionTemplateDocument.swift:69: cannot find 'TemplateRequiredFields' in scope
SubscriptionTemplateDocument.swift:134: cannot find 'TemplateHTTPClientLayout' in scope
SubscriptionTemplateDocument.swift:180: cannot find 'TemplateHTTPClientLayout' in scope
```

**建议：补全依赖或统一维护客户端测试的编译源文件清单，再重跑。** 上述是本次实际出现的错误，不代表补完这两个类型后没有其他错误。本次没有启动数据库及该集成测试的完整流程。

该测试直接调用生产 `SubscriptionTemplateStore`，验证非法源码留在源码模式、草稿与发布隔离、预览过期、历史回滚和导入导出。它与 UI 点击测试处于不同层次，不能仅因流程相似便删除。

## 4. 未接入入口不等于冗余

以下文件没有被其他已跟踪文本按文件名引用；CI 也没有通过目录发现方式运行它们：

| 文件 | 独立覆盖 | 本次验证 | 建议 |
| --- | --- | --- | --- |
| `tests/public-port-client.swift` | 监听端口联动、跳跃端口、手动覆盖、重新加载和非法输入 | 与生产 `PublicPortDraft.swift` 编译并运行通过 | 增加运行脚本并接入 macOS CI |
| `tests/template-documentation-audit.py` | 文档标题锚点、协议字段作用域、嵌套证据路径、导航过滤 | 7 个测试通过 | 调整命名以纳入发现，或显式接入 CI |
| `tests/test_fixture_network_guard.py` | fixture 所有权、共享网络及特权操作边界 | 6 个测试通过，包含在本次 Python 发现的 19 个测试中 | CI 使用完整 `test_*.py` 发现，或增加显式步骤 |

另有独立模块 `tests/traffic-clear/`（`traffic_test.go`、`go.mod`、`go.sum`）未在 workflow、脚本或当前验证指南中找到运行入口。它直接测试固定 Hysteria 版本的真实 `trafficlogger`：清空计数后在线状态保持，持续连接仍能继续计费。这与 Rust 端模拟 HTTP 响应的测试不同，建议保留并提供独立 Go 入口；本次未运行该模块。

## 5. 应保留的 Redis 测试存在执行盲区

以下测试在没有 `TEST_REDIS_URL` 时直接 `return`，Rust 测试框架会计为通过：

- `crates/hysteriax-server/src/quota_rank.rs:439`：更新、重置、恢复和热缓存不扫描套餐表。
- `crates/hysteriax-server/src/quota_rank.rs:595`：提交后通知和 Redis 故障回退。
- `crates/hysteriax-server/src/quota_rank.rs:653`：旧快照不能复活已删除项。
- `crates/hysteriax-server/src/quota_rank.rs:712`：乱序/丢失通知及 bigint 版本精度。
- `crates/hysteriax-server/src/read_snapshot.rs:134`：Redis HTTP 缓存填充保持数据库快照一致。

当前 `.github/workflows/ci.yml` 的 Rust job 只定义 PostgreSQL，没有 Redis service 或 `TEST_REDIS_URL`。因此，按仓库中的 CI 配置，这五项不会执行其 Redis 断言。

**建议：保留用例，为 Redis 配置专项执行，并使依赖缺失可见。** 可采用明确标记的专项测试并由专门 job 必跑，或在要求 Redis 的执行入口缺少配置时失败。不要把这些用例当作“无作用测试”删除。

## 6. 相似但不可直接删除的 Rust 用例

静态筛查扫描了 159 个含测试的 Rust 文件、1,074 个测试函数声明（server 1,073，validator 1）。去空白及近似文本比较用于产生候选，随后核对触发条件与断言；没有确认整段等价、可安全删除的 Rust 用例。这不等于逐项运行或证明不存在语义重复。

具体保留依据：

- `api/nodes/transaction_tests.rs:94` 与 `:113`：文本高度相似，但分别覆盖等待期间 revision 改变与节点进入 deleting，并且走 deploy / sync 两个动作。可抽参数化 helper，不能减掉任一场景。
- `credentials/rotations/publication/tests.rs:382` 与 `:405`：分别覆盖 CA/publication 归档或 superseded，以及 identity 归档或用户删除；判断层次、SQL 条件和结果断言不同。
- API 各资源的幂等键、分页、审计回滚测试虽然结构相似，但绑定不同资源处理器及数据库关系；只保留一类资源会失去其他入口的覆盖。
- `#[ignore]` 的 SSH journal、快照节点清理及手工计费 benchmark 都写明环境要求。特别是快照节点清理测试仍由 `scripts/verify-snapshot-node-cleanup.py:79` 显式执行，不应按 ignore 状态判定过时。
- `overview_tests.rs`、`ssh_*_tests.rs` 及分页测试通过 `#[path = ...]` 以不同模块名接入，不能因没找到同名 `mod` 而删除。

## 7. 本次验证与边界

已执行：

```sh
python3 -m unittest discover -s tests -p 'test_*.py'
# 19 passed
python3 tests/template-documentation-audit.py
# 7 passed
ruby scripts/verify-management-contract.rb
# passed：61 个写操作的幂等头、握手与游标契约
```

另外以临时目录编译并运行 public-port 测试，通过；使用 workbench 脚本的原始源文件清单进行类型检查，失败如第 3 节所述。没有运行完整 Rust、Go、数据库、Docker 或 XCUITest 套件，没有执行部署或故障注入。

审计还覆盖了 59 个 `scripts/verify-*` 入口的清单、现有 workflows、23 个顶层 Swift 测试程序及 5 个 macOS UI 测试文件（15 个测试方法）的组织和引用关系。逐项深入阅读集中在删除候选、相似候选及失修入口；不是全量逐行人工审阅。

## 8. Rust 按维护收益重新筛查

以下结论补充第 6 节。“不是逐字重复”只说明触发条件不同，不足以证明每个测试的维护价值。

### 8.1 优先删除：两个只验证旧锁夹具的测试

| 位置 | 测试 | 删除理由 |
| --- | --- | --- |
| `crates/hysteriax-server/src/db.rs:464` | `legacy_fixture_waiters_leave_connections_available_for_reads` | 启动 12 个并发任务，验证测试专用旧锁等待者不会占满连接池；调用的不是生产写事务路径 |
| `crates/hysteriax-server/src/db.rs:533` | `legacy_fixture_serializes_its_own_events` | 两次取得同一个测试锁，再断言事件顺序；证明的是夹具与 PostgreSQL 锁行为，不能证明生产事件发布顺序 |

两段共 79 行（包含测试属性）。`db/legacy_lock_fixture.rs:1` 已明确说明该模块仅用于测试，生产代码不能调用。建议删除这两个独立测试；生产事务重试、连接释放、事件发布顺序应继续由各自生产路径的测试覆盖。若旧锁夹具仍保留，只需让其消费者的超时/失败暴露夹具问题，不继续维护这套并发自测。

### 8.2 可删除：47 处旧触发器禁用代码

45 个 Rust 文件中出现 47 次 `hysteriax_serialize_writes`，均为测试中查询 `pg_trigger` / `pg_proc` 并禁用对应触发器的 SQL。当前 `0001_baseline.sql` 不定义该函数，生产 Rust 代码也没有创建它的路径。测试从当前基线新建隔离 schema，这些循环没有目标对象。

例子：

- `api/authorization_groups/transaction_tests.rs:4`。
- `api/nodes/transaction_tests.rs:18`。
- `api/users/transaction_tests.rs:38` 的 `disable_legacy_lock`。
- `api/credentials/transaction_tests.rs:49` 的 `disable_legacy_lock`。

**建议：删除这些 SQL，删除因此变空的 helper 及调用，保留业务测试。** 这是可以减少维护噪声的实质清理，不需要靠减少业务场景数实现。实施后仍需实际运行受影响测试确认。

### 8.3 收敛：散布在业务测试中的旧全局锁验证

`hold_retired_global_write_lock` 在 47 个文件中出现 63 次；该计数包括定义、导出及测试调用，不等于 63 个测试。旧锁是数据库级 advisory lock，即使 schema 隔离，多个测试仍可能争抢同一 key；夹具为此实现了轮询和退避。

**建议：保留 `db/cutover_tests.rs:3` 的集中回归，并视生产调用路径保留少量代表性入口；从其余业务用例中去掉附带的旧锁持有/释放。** 集中测试不能保证所有直接 SQL 路径，所以应先核对绕过 `db::write` / `serializable` 的入口，再收敛，不机械删除全部调用。

例如 `api/nodes/transaction_tests.rs:68` 的重试测试和 `api/authorization_groups/transaction_tests.rs:212` 的授权测试，应继续断言最终业务结果、审计和任务数量，但不必每次同时验证已经完成的锁架构切换。

### 8.4 从常规回归移出：绑定具体执行计划的测试

优先候选：

1. `api/query_optimizer_tests.rs:12`：`selective_history_search_and_status_paging_use_indexes`。插入两组 6,000 行数据，使用 `EXPLAIN ANALYZE` 并要求出现指定索引名。建议移至性能专项；字面搜索与分页正确性保留在接口测试中。**同文件第 76 行的审计总数/回滚测试不随之移除。**
2. `analytics/followup_tests.rs:243`：`aligned_online_history_skips_cycles_until_an_edge_or_dirty_minute_needs_them`。断言特定底层表的扫描循环，应由性能专项维护。
3. `analytics/followup_tests.rs:309`：`rollup_refresh_reads_only_dirty_ranges_and_skips_empty_queues`。构建 20,000 条 minute totals 和 6,000 条 cycle metrics，并要求执行计划读取行数小于 100。建议将计划/规模验证移至专项，保留必要的结果正确性断言。

这类测试对避免全表扫描有价值，但把特定索引名、扫描策略与大数据 fixture 放进每次运行的常规正确性回归，会提高数据库版本变化和实现重构的维护成本。不能仅因为“绑定实现”就全部删除；专项应继续验证性能目标。

### 8.5 精简边界

本轮可以明确提出的是：**删除 2 个夹具自测、删除 47 处失效 SQL、收敛旧锁探测、将 3 个重型计划测试拆分或移至专项。** 不预设删掉某个比例，也不因计费、证书轮换、撤权、幂等和故障恢复用例数量多就整组删除。

以上为清理前静态审阅依据。实际实施另删除了 DNS fixture 的旧锁自测；其余业务场景保留，编译及运行结果记录在 0007 实施记录中，不据此推断性能提升比例。
