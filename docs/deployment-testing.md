# 部署与回归验证

在独立数据库、Redis 和自有 Docker 容器中运行验证。不要将开发脚本指向线上数据库或直接复用线上 Compose 项目。带真实快照的测试必须先隔离外部出站，避免副本触发 SSH、DNS 或撤权。测试环境准备见 [安装指南](installation.md#local-development-and-tests)。

## 目录与入口

测试按职责归类，完整规则见 [目录与测试归属](code-layout.md)。以下命令均从仓库根目录执行。

| 类型 | 位置或入口 | 环境 |
| --- | --- | --- |
| Rust 模块测试 | `crates/`，通过 `cargo test --workspace` 执行 | 数据库用例需要隔离 PostgreSQL，Redis 专项另行配置 |
| 脚本单元测试 | `tests/unit/` | Python 依赖见 `tests/requirements.txt`；使用 mocks 和临时文件 |
| 仓库级功能与契约 | `tests/integration/`、`tests/contracts/` | 各入口分别声明数据库、容器、内核和外部服务要求 |
| 性能专项 | `tests/performance/` | 独立的规模、时长及隔离环境要求 |
| 客户端独立回归 | `apps/macos/Tests/`，通过其 `runners/` 执行 | macOS 与相应 Swift/Xcode 工具链 |
| 客户端 UI 回归 | `apps/macos/UITests/` 及相关 runner | 按入口准备独立应用与 UI 环境 |

共享辅助代码位于 `tests/support/`，共享静态数据位于 `tests/fixtures/`，Docker/CA 测试环境资产位于 `tests/fixtures/runtime/`。实际迁移工具属于 `scripts/migrations/`，配套升级回归属于 `tests/integration/`。

## 常用检查

```sh
cargo fmt --all -- --check
RUST_MIN_STACK=16777216 cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
ruby scripts/generate/generate-swift-api-models.rb --check
ruby tests/contracts/verify-management-contract.rb
bash apps/macos/Tests/runners/verify-node-task-feedback.sh
bash apps/macos/Tests/runners/verify-management-display-text.sh
bash apps/macos/Tests/runners/verify-management-protocol.sh
bash apps/macos/Tests/runners/verify-localization.sh
python3 -m unittest discover -s tests/unit -p 'test_*.py'
go -C tests/integration/traffic-clear test -count=1 ./...
```

Rust 数据库测试需要可丢弃 PostgreSQL 的 `TEST_DATABASE_URL`；Redis 专项另需 `TEST_REDIS_URL`。全量调试构建沿用 16 MiB 测试线程栈，既有 mTLS 深层异步调用测试可能在默认栈下溢出；此设置仅作用于测试进程，不修改发布服务的线程栈。保留测试输出中的忽略项和失败，不把部分完成记为全量通过。上述是运行入口，不代表本次已经执行或通过。

调度器或后台任务 future 结构改动还需验证完整发布镜像的默认线程栈：`python3 tests/integration/verify-maintenance-startup.py --image <本地 linux/amd64 镜像> --output <新证据目录>`。该入口在带专属标签的内部 Docker 网络中创建 PostgreSQL/Redis，显式初始化测试库后启动完整服务；不设置 `RUST_MIN_STACK`，默认运行 330 秒以覆盖 300 秒维护兜底，随后验证三类采样排空、零重启和退出码 0。仅删除本次夹具并保留日志、inspect 和结果。大栈下的模块测试不能替代这个检查；空节点夹具及本机架构模拟也不能替代生产稳定观察。栈修复和发布记录见 [0039](plans/0039-deploy-maintenance-scheduler.md)。

执行计划性能专项单独运行：

```sh
TEST_DATABASE_URL=postgresql://... bash tests/performance/verify-query-plans.sh
```

该入口串行执行 3 个显式忽略的重型测试，检查搜索/分页索引、在线历史扫描路径和聚合脏区间扫描规模。常规 `cargo test --workspace` 保留业务结果和事务正确性回归，不默认运行这 3 项。脚本只选择这些性能用例，不会执行需要 SSH 或真实快照的其他忽略项。

后台维护事务对照使用 `tests/performance/benchmark-maintenance-transactions.py`。提供挂载相同 `retention_benchmarks.rs` 夹具的基线/候选冻结测试二进制、指向可丢弃本地 PostgreSQL 的 URL 文件和输出目录；`--delay-ms` 注入响应延迟，`--candidate-first` 可反转执行顺序。不同源码副本共用 Cargo target 时输出路径可能相同，应在每个版本构建完成后立即复制冻结并记录 SHA256，再构建下一个版本。入口拒绝同一路径，并核对运行前后摘要。它分别核对空清理、持续非空分页和空闲后新增候选的完成量、事务 SQL 数量及包含监督器的调用耗时，不作为争用率或容量验收。

同一 Rust 夹具另有显式忽略的 `credentials::retention::tests::benchmarks::background_contention_comparison`，在隔离数据库运行 90 秒真实后台循环，使用 26 万历史工作、24 万历史租约和 6 个合成节点。SSH 固定指向 `127.0.0.1:1`；该夹具观察事务竞争，不验证远端采样成功，也不替代 10 万用户 / 1 万节点容量场景。

空预检合并的交叉对照使用 `tests/performance/benchmark-maintenance-scheduler.py --baseline <冻结基线> --candidate <冻结候选> --output <新目录>`。两份测试二进制须有相同的池大小与窗口汇总插桩，默认池 100、每轮 120 秒、预热 30 秒，按 ABBA 顺序各运行两次；不并行编译或测试。入口默认创建带专属标签的回环 PostgreSQL，逐轮创建相同规模的隔离 schema；`--container` 只复用带 `maintenance-coalescing` 标签的测试容器且不删除它。结果区分实际空数据库预检与内存跳过，并保留窗口差分、包含预热的协议命令数和运行前后二进制摘要。事务墙钟耗时包含等待，不能当作 CPU 时间或精确连接占用；仍不能外推成功采样率、线上访问延迟或节点容量。实现与对照记录见 [0038](plans/0038-maintenance-poll-coalescing.md)。

0026 第一批的受控归因使用两个入口：`prepare-maintenance-variants.py` 从指定提交构建“基线、仅释放改动、仅空闲改动、合并”四个组合，每次构建后立即冻结二进制及摘要；`benchmark-maintenance-causality.py` 读取这两份元数据，新建自管本地 PostgreSQL 18.6，将同一份已准备数据库克隆给每轮运行。前置构建和计时应分开执行，计时期间不并行运行其他编译或测试。

```sh
python3 tests/performance/prepare-maintenance-variants.py --baseline-revision 523e151 --candidate-revision f119a5d --output-dir target/maintenance-variants
python3 tests/performance/benchmark-maintenance-causality.py --binaries target/maintenance-variants/variant-binaries.json --sources target/maintenance-variants/variant-sources.json --output-dir target/maintenance-causality --seconds 180 --warmup 60 --replicates 2
```

每轮先预热 60 秒，再测量 120 秒；控制器在两个时钟相位下反转运行顺序，使用窗口前后的累计计数差分，测量结束后才中止夹具任务。`results.json` 保留完成量、冲突、失败、耗时及截点在途差值；控制器核对克隆的节点摘要/关系计数、冻结二进制摘要和两个改动的文件组合。它只连接自己创建的回环数据库，结束时按容器 ID 与专属标签核对并删除该容器及匿名卷。夹具中的远程采样有意失败，前后窗口仍非确定性线程调度；结果属于本地合成负载归因，不能直接认定为生产因果或容量验收。

## 采集并发对照

`tests/performance/benchmark-sampling-concurrency.py` 比较每类采集队列的并发数。准备同一源码构建并冻结的 Rust 测试二进制，以及可在当前主机运行的固定 Hysteria v2.12.3 二进制：

```sh
python3 tests/performance/benchmark-sampling-concurrency.py --binary /absolute/path/server-tests --probe-binary /absolute/path/hysteria --output target/sampling-concurrency-run --nodes 6,64 --parallelism 4,8,16,32 --rounds 2 --delay-ms 25
```

默认创建自管的 PostgreSQL 18.6 容器，仅绑定回环地址；保持 fsync/synchronous_commit 开启，数据位于临时文件系统。退出时按容器 ID 和专属标签删除本次容器。可选 `--container` 只能复用带本专项标签的测试容器，脚本不删除复用容器。不要提供生产容器。

每组新建隔离 schema，准备 10 万用户、1 万节点、100 万条分配关系，再按 `--nodes` 启用实际参与本轮的节点。连接池固定 32，预建连接后注入指定数据库返回延迟。SSH 使用真实认证、exec 和 direct-tcpip 通道，用户流量及 Linux 网卡计数为确定性合成值。代理使用真实 Hysteria 客户端和一个共享的本地 Hysteria 服务端，夹具鉴权简化为读取本次有效探测令牌；不覆盖生产鉴权链和公网网络条件。TLS 使用仅限夹具的 localhost 自签证书。

实验先冻结一组 traffic/network/proxy 周期，再将共同准入窗口设为完整的 60 秒，以排除启动时分钟相位不同的影响。测试驱动保留每秒补查、队列内顺序领取和并发任务回收，调用产品原有的 claim、sample_work、supervise、finish 和 expire。生产配置当前默认及上限均为 16；更高并发仅用于显式忽略的测试驱动。0030 记录的首轮冻结二进制仍使用当时的生产默认/上限 4，后续参数改动另记于 0031，不覆盖旧结果。它不运行其他维护循环、管理 API 压力或连续周期积压，不能替代整个服务的容量验收。

两轮默认反转并发值顺序；每组记录状态、窗口内完成量、开始及完成时间分位数、领取/采集/确认耗时、连接获取耗时（含连接校验）、续租/执行停止告警、真实代理成功率，以及捕获批次、逐用户记录和用量的一致性。每批只允许一次清零；缺失结果、探测失败和过期均保留，不把任务 complete 直接当成代理健康。原始日志、逐组 JSON、输入源码和二进制摘要保存在输出目录。正式计时期间不要同时编译或运行其他测试。

这是一组本地有限任务队列的并发实验；数据底座达到 10 万用户/1 万节点，不代表所有节点都完成了采集。选值必须同时注明活跃节点数、连接池、延迟与硬件条件，生产和 1 万活跃节点的验收分别判断。

## 全进程与部署专项

| 入口 | 验证范围 |
| --- | --- |
| `tests/integration/verify-deployment-matrix.py` | 自有 systemd Linux 容器中的 SSH 部署、探测及卸载。 |
| `tests/integration/verify-multi-instance-billing.py` | 多实例计费批次、崩溃／数据库断连、租约接管和串行参考。 |
| `tests/integration/verify-multi-instance-cache.py` | 跨实例失效、版本与 Redis 故障。 |
| `tests/integration/verify-multi-instance-events.py` | 发布顺序、进程故障和游标恢复。 |
| `tests/integration/verify-multi-instance-cutover.py` | v2 多实例协议及幂等切换。 |
| `tests/integration/verify-migrations.py` | 新库基线、完整迁移校验和多实例就绪。 |
| `tests/performance/verify-snapshot-capacity.py` | 最终 Linux 镜像、Caddy 三副本及 Redis 停止／空重启；快照需属于当前基线。 |
| `tests/integration/verify-backup-restore-compose.py` | 备份恢复及外部操作时间线不一致时的拒绝行为。 |

先查看各脚本的参数和环境要求；提供命令行参数解析的 Python 专项可用 `--help` 查阅。当前脚本使用 v2。原迁移链的升级、旧版 mTLS 转换和旧版恢复专项已移除；当前快照测试仅接受新基线及其后续兼容迁移。镜像、内核下载、Docker 权限和 fixture 参数以各脚本为准；不将旧脚本的历史通过当作本次重新执行。

容量记录只描述具体数据、并发和限制下的实验。本机 Docker 可完成多实例功能验收，不要求额外原生 x86_64 服务器。生产 PostgreSQL、Redis 与 API 可分机部署，按实际主机角色核算资源。详见 [当前架构](architecture.md)。

## 文档审计与发布边界

模板文档审计结果可以按需重新生成：先用 `tests/contracts/audit-template-documentation.py` 的 `--write` 模式生成固定版本字段基线，再运行 `tests/contracts/audit-template-website.py` 比较在线文档。前者需要匹配的上游文档源码；后者依赖前者生成的清单并访问网络。脚本参数及版本以源码为准，生成结果不属于运行时资源。

验证通过不等于执行发布。实际部署须本机构建镜像后传输，停止全部旧 API，保留可恢复备份，单独迁移并核查后启动。不要对线上执行故障注入或容量压力测试。
