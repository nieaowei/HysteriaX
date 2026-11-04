# 0015 生产锁争用修复

实施日期：2026-11-04～05（Asia/Shanghai）。基线提交 `fb883ed`。

## 目标和边界

按 10 万用户、1 万节点、100 万分配修复已证实的套餐扫描、统计汇总和流量入账持锁热点。初始交付代码、迁移、一次性测试迁移工具和验证材料；随后按用户 2026-11-05 的明确部署要求完成生产发布，见末节。

租约收尾竞态、串行化冲突的逐条归因、全局事务隔离级别和采样窗口不在本轮范围。管理 API、管理协议 2 和订阅格式不变；数据库从 schema 9 升至 10。

归因证据位于私密目录 `production/diagnostics/20261104-2029-log-review/`；公开文档不包含生产原始日志或秘密。

## 已实施

1. **统计队列分别提交。** 周期/小时/分钟/任务重试默认每批 1/64/64/32 个键，最多四个独立队列事务。普通读取 generation，使用带数据库、schema 和队列命名空间的事务 advisory try-lock 协调计算者；以原生复合键分页，忙键不会挡住后续候选。计算完成后仅在最后一条 SQL 按原 generation 确认，并跳过被源写入锁住的 dirty 行。结果和确认同事务提交；并发新 generation 留待重算，历史查询继续覆盖旧结果。重试链按 root 协调，移除旧 guard 表。
2. **套餐按到期索引执行。** 增加 `next_evaluation_at`，取未来最近的月度重置、到期提醒或到期时刻；无未来事件为 NULL。配置、用量修正及采样继续立即评估并刷新调度，直接维护 SQL 修改配置也会使调度失效。创建节点先写初始用量再评估，避免固定套餐没有下一事件时漏掉初始超额。到期查询使用 `statement_timestamp()`，使时间条件能进入索引范围而不是过滤全部未来行。共用纯计算逻辑，集合 SQL 提交套餐、提醒、限制 epoch、审计和 admission，SQL 次数不随节点数增加。每事务最多 64 节点，四条独立维护通道；同一租约最多处理四个独立事务后释放，摊薄认领开销，节点锁不跨页保留。每页 due 字段即持久检查点，接管只处理剩余到期行；保留删除、重置、计费和 fencing 保证。
3. **缩短计费事务。** 保持 500 项分页与节点→批次→有序用户→基线/项目的锁顺序。合并批次/前序/重启快照、用户锁后的成员与基线读取、基线/账本/用量写入，以及检查点/完成状态更新。空页跳过用户和计费 SQL。额度撤权 admission 在用量可见后执行，与整个页面提交保持原子；保留前序批次检查、收到响应后恢复、已捕获数据及清零一次性语义。
4. **迁移和工具。** 新增 `0010_lock_contention.sql`，旧 0001～0009 不变；既有套餐先标为待评估，移除旧全节点扫描记录和租约，保留限制/恢复任务。正常维护仍用新服务 `--migrate-only`。另提供 `scripts/migrate-lock-contention-test-db.py`：只接受明确确认的停止状态测试库，核对历史 checksum 和运行契约，在同一事务执行 DDL、迁移记录和版本更新，逐表比较业务值摘要，失败回滚、重复安全。

## 验证设计

- 套餐：月度跨期、时区/DST、提醒进入与退出、过期、额度修正、忙节点、删除、限制反复变化；每事务 64 行上限，前页已提交而末页确认失败后的恢复，不重复 epoch/审计。
- 统计：计算已经写入旧派生结果时，源写入仍能提交；旧 generation 不清掉新 dirty，历史查询覆盖旧结果；同键互斥、忙键后续仍推进、回滚同时撤销结果和确认、进程事务退出可重新认领。
- 计费：重复交付、500 项分页、跨节点共享用户、额度重置、撤权回滚、前序批次、清零丢响应和接管，验证记录/用量/检查点不重不漏。
- 迁移：实际 schema 9 服务创建用户和密文后停止，注入 DDL 失败并验证完整回滚，再迁移、重复执行，最后启动 schema 10 服务并验证 readyz 和认证读取。
- 容量：同一 fixture 注入到原基线与当前源码，分别单实例/三实例，四组各至少 900 秒；每组 10 万用户、1 万节点、100 万分配，数据库响应延迟 25ms，并校验 SELECT 1 实测延迟。运行真实套餐、统计和带租约的计费执行路径，16 条源写入流、约每秒一页待入账；不运行 SSH 或 HTTP 端到端容量测试。合成导入阶段暂时关闭触发器并验证关系图，测量阶段恢复真实触发器和约束。

## 门槛与专项验收结果

交付完成要求：未到期套餐无评估写入；1 万节点首次到期评估不超过 60 秒；发生等待的节点锁 p95 比基线下降至少 70%；暂停统计计算不阻塞源写入；停止测试写入后 dirty/ready 队列 60 秒内收敛；账本、用量、小时汇总和原始在线来源一致，无源写入或执行错误。

锁争用专项验收已通过。全仓回归未全绿，两类结论分别保留：

| 验证 | 当前结果 |
| --- | --- |
| 统计最终回归 | 16 通过，2 个查询计划检查由专用脚本另行通过，包含并发新 generation、回滚断言及通用计划下复合键索引分页检查 |
| 流量回归 | 53 通过，1 个手动性能检查未在该轮执行 |
| 计费延迟检查 | 25ms 响应延迟下，100/200/500 项页面均从基线 27 次命令降至 17 次；500 项三次由 0.99～1.05 秒降至 0.72～0.73 秒 |
| 套餐最终回归 | 24 通过，1 个容量检查由正式对比单独执行；包括页间释放、末页确认回滚恢复及未来套餐索引空范围检查 |
| 节点创建回归 | 5 通过，覆盖固定和月度套餐的初始用量立即限制及原生事务重试 |
| 最终源码服务端全量 | 1043 通过、6 失败、15 跳过，未全绿；5 项为 mTLS 轮换/发布断言，1 项为节点清理断言。原基线同配置全量 1039 通过、2 失败、15 跳过，其中相同的 `a_pause_after_publication_keeps_authoritative_access_disabled` 断言也失败，另 1 项为计费 fixture 连接池超时。此前当前轮换模块串行曾 17 项全部通过，不以一次重测通过代替全量结论 |
| 查询计划 | `scripts/verify-query-plans.sh` 的 3 项检查全部通过 |
| 编译与静态检查 | 格式和差异空白检查通过；Clippy 排除未改文件中既有的 3 类告警后通过，严格全仓检查仍有既有告警 |
| 测试迁移工具 | 实际旧服务初始化、故障回滚、升级、重复执行及新服务就绪全部通过 |
| 满规模 90 秒预检 | 首轮评估由 80.67 秒降至 32.71 秒；0 错误，待处理队列清零，计费与汇总一致；不是正式 15 分钟验收 |
| 正式四组对比 | 四组均完成 900 秒测量；最终 `acceptance/report.json` 的 `acceptance_passed=true`，单/三实例均满足全部专项门槛 |

每组均为 10 万用户、1 万节点、100 万分配；25ms 人工响应延迟下，SELECT 1 的实测 p50 为 28.5～29.6ms。节点等待仅统计实际发生且被观测到的正等待，非全部请求延迟；新版正等待观测较少，保留样本数，不把本地结果扩展成生产百分位承诺。

| 指标 | 基线单实例 | 新版单实例 | 基线三实例 | 新版三实例 |
| --- | ---: | ---: | ---: | ---: |
| 节点锁正等待 p95（ms） | 11349.69 | 191.65 | 11586.83 | 349.66 |
| 正等待观测数 | 648 | 9 | 625 | 11 |
| p95 下降 | — | 98.31% | — | 96.98% |
| 万节点首轮评估（秒） | 未完成 | 30.68 | 未完成 | 33.14 |
| 源事务 p95（ms） | 19466.20 | 1320.34 | 20129.94 | 1372.88 |
| 源写入成功数 | 5156 | 31056 | 4901 | 28279 |
| 初始套餐及 dirty/ready 待处理数 | 4496 | 0 | 4752 | 0 |
| 注入批数 / 账本行 / 用量字节 / 小时汇总字节 | 508 / 508 / 508 / 508 | 872 / 872 / 872 / 872 | 488 / 488 / 488 / 488 | 858 / 858 / 858 / 858 |

新版停止源写入后的收敛与核对耗时（含工作进程收尾）分别为 7.41、12.28 秒；原始与缓存在线连接数、用户数均为 10000 / 10000，未入账项目为 0，执行入口与源写入返回错误数为 0。该错误计数不等同于所有 SQL 重试或监督器日志数；租约收尾竞态仍按边界延期。

本地证据保存在 `target/lock-contention/`：最终报告 `acceptance/report.json`、四组原始 JSON/日志及测试二进制 SHA256，最终源码回归 `workspace-final-source.log`，基线回归 `workspace-baseline-serial.log`，最终服务迁移 `migration-verification-final.json`。没有使用 90 秒预检、中断组或环境故障组替代正式测量。

## 未解决的全仓回归与环境事件

最终源码全量失败的用例保留原断言，未为取得通过结果而修改轮换测试、租约监督或完成协议：

- `rotation_flow::explicit_replacement_retires_unpublished_target_and_old_tasks_cannot_run`
- `rotation_flow::published_target_can_only_be_superseded_explicitly_after_issuer_expiry`
- `publication::a_pause_after_publication_keeps_authoritative_access_disabled`
- `publication::an_archived_or_superseded_publication_does_not_overwrite_pending_identity_metadata`
- `publication::three_hundred_one_identity_deadlines_are_scheduled_in_one_hundred_item_pages`
- `jobs::node_cleanup_tests::large_record_removal_is_queued_idempotent_and_completes_without_ssh`

失败快照 `rotation-failure-snapshot.json` 显示一例用户 admission 已 complete、没有待签发用户，但轮换仍在 identities 阶段，且没有发布工作；这与已延期的完成后监督器返回 None 的竞态相符，尚未逐次捕获监督器取消分支，不能断言全部失败都已归因。基线的全量和单独复测分别复现了 `a_pause_after_publication_keeps_authoritative_access_disabled`、`an_archived_or_superseded_publication_does_not_overwrite_pending_identity_metadata` 两项相同发布断言，不能据此推断其他每项均与本次修改无关。全仓回归未全绿的限制必须与锁争用专项结果一起报告。

多轮全量测试保留的隔离 schema 累积后，本地 Docker 文件系统出现 `No space left on device`；三实例组在约 831 秒时因监控连接池超时退出，缺少报告，整组无效。原始日志与失败报告保存在 `current-3-environment-failure.log`、`report-before-environment-retry.json` 和 `local-environment-failure.log`。已清理本轮已结束的 schema 和独立回归容器，恢复约 30GB 空间，再单独重跑三实例；该测量期间不并行全量测试或数据库清理。此事件不涉及生产数据库。

恢复空间后的单独复测（`*-after-space-recovery.log`）结果：当前节点清理用例 1 通过，原基线计费用例 1 通过；当前发布模块 11 通过、2 失败，原基线同模块 12 通过、1 失败；当前轮换流程模块 13 通过、4 失败。mTLS 相关失败在恢复空间后仍存在，不归咎于磁盘事件，也不宣称全仓回归通过。本轮仅完成锁争用修复与专项验收，收尾竞态保持延期。

## 复现命令

`TEST_DATABASE_URL` 指向可销毁的 PostgreSQL 测试库；每项验证使用隔离 schema。Python 工具依赖 `scripts/requirements-test.txt`。迁移验证需单独的停止状态测试库，避免其他验证的活跃连接触发拒绝保护。

```sh
RUST_MIN_STACK=16777216 cargo test --locked --workspace -- --test-threads=1
scripts/verify-query-plans.sh
ruby scripts/verify-management-contract.rb
python3 scripts/verify-migrations.py
python3 scripts/verify-lock-migration.py \
  --before-server target/lock-contention/baseline-server \
  --after-server target/debug/hysteriax-server \
  --report target/lock-contention/migration-verification.json
python3 scripts/verify-lock-contention.py \
  --baseline-commit fb883ed --output target/lock-contention/acceptance
```

容量脚本需要全新的输出目录，默认先保存原基线和当前测试二进制，顺序运行四组 900 秒测试。`--smoke` 或只选择一个实例数仅供调试，不能产生完整验收通过结论。报告不包含数据库连接串或生产数据。

全量测试会保留隔离 schema；应在容量测量前清理已结束测试的数据并保证足够空间，容量测量期间不要并行全量回归。发生测试环境中断时，可使用 `--reuse-baseline --reuse-completed-current`：校验归档提交和基线 profile，仅复用当前二进制 SHA256、规模、延迟及时长都一致且成功退出的当前组，其余组重新完整测量。不会把中断日志或缺失报告作为通过依据。

本轮在初次新版测量期间修正统计候选分页的索引计划，因此中断该组且不采用其结果。`--reuse-baseline` 复用已完成的两组基线报告，先校验归档的 git 提交及规模、延迟、时长，再重新构建当前二进制并重新执行两组完整 900 秒测量。中断的新版测量日志保留为 `current-1-interrupted-before-index.log`，不进入最终门槛计算。

本地验收结束后已移除创建的两个临时 PostgreSQL 容器及匿名数据卷；报告、日志及基线/当前二进制保留在本地 `target/lock-contention/`。该阶段未部署或提交代码；后续生产发布见下。

## 后续生产部署（2026-11-05）

按用户后续要求，本地构建并传输 `hysteriax-server:lock-contention-20261105-r1`，验证 365 个构建输入、归档、镜像层、平台、标签及二进制。停 API 保存 35,873,918 字节数据库快照，以原生产 Compose 和主密钥执行 `--migrate-only`，schema 9 升至 10；24 张业务表（明确排除新增调度字段及被替换的旧扫描记录）摘要保持。API 于北京时间 04:01:58 启动、04:02:01 本机 ready，公网和认证读取、四组原有订阅下载及摘要核对通过，Caddy/Redis 与私密配置保持。

04:06:20 核对六节点 deployed/ready，18 组 network/proxy/traffic 均取得更晚的成功，管理任务无新增 failed。仍存在串行化冲突及续租 WARN，不宣称全部日志问题已修复。实际配置、快照校验和验收证据保存在 [本次发布目录](../../production/releases/20261105-lock-contention-r1/RELEASE.md)；未发布 macOS 安装包、未修改延期的租约协议，也未在生产执行模板诊断。
