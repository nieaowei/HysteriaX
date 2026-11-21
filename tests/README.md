# 仓库级测试索引

本目录统一保存仓库级验证程序、单元测试、性能测试、环境辅助代码和共享夹具。可执行的 Python、Shell 或 Ruby 验证程序也按测试职责归入这里。

与操作工具、组件专属测试的边界见 [目录与测试归属](../docs/code-layout.md)。新增验证程序按下表归组，正式验收的环境和门槛以各入口说明为准。

历史采样清理回归入口是 `cargo test -p hysteriax-server background::retention::tests`。持续容量对照使用可丢弃的本地 `TEST_DATABASE_URL`、本机可运行的 `HYSTERIAX_PROBE_BINARY` 和私有 `HX_SAMPLING_OUTPUT`。设置 `HYSTERIAX_SAMPLING_INTERVAL_SECONDS=60`、`HYSTERIAX_EXECUTION_LEASE_SECONDS=180`、`HYSTERIAX_DB_POOL_MAX_CONNECTIONS=100` 和 `RUST_MIN_STACK=16777216`，执行 `cargo test --locked -p hysteriax-server background::sampling_benchmark::retention::sampling_retention_capacity -- --ignored --exact --nocapture --test-threads=1`。夹具含 10 万用户、1 万节点、每分钟 3 万条进入清理窗口的合成工作及 6 个走本地 SSH/QUIC 路径的采样节点。

数据库历史清理回归入口是 `cargo test -p hysteriax-server background::history_retention::tests`。容量入口复用上段的 PostgreSQL、Hysteria 及运行环境，测试名为 `history_retention_capacity`，参数为 `--ignored --nocapture --test-threads=1`。基线 5 分钟，清理 30 分钟，均从完整采样周期开始。两阶段均运行 0048 并注入每分钟 3 万条到期采样工作。设置 `HX_HISTORY_RETENTION_SMOKE=1` 时两阶段各 65 秒，供检查夹具与入口。

| 目录 | 内容 | 常用入口 |
| --- | --- | --- |
| [unit/](unit/) | 运维脚本、测试辅助代码和文档提取工具的 Python 单元测试 | `unittest discover` |
| [integration/](integration/) | HTTP、数据库、SSH、DNS、部署、升级和多实例功能验证 | `verify-deployment-matrix.py`、`verify-multi-instance-billing.py`、`verify-execution-finalization.py` |
| [integration/traffic-clear/](integration/traffic-clear/) | 独立 Go 模块，验证固定 Hysteria 的流量清零、在线状态和继续计费行为 | `go test` |
| [contracts/](contracts/) | OpenAPI、配置内核、固定 schema 和模板文档契约验证 | `verify-management-contract.rb`、`verify-subscription-validator-compatibility.py` |
| [performance/](performance/) | 容量、并发、锁争用、查询计划及大规模清理检查 | `benchmark-multi-instance.py`、`benchmark-maintenance-transactions.py`、`benchmark-maintenance-poll.py`、`benchmark-maintenance-causality.py`、`benchmark-sampling-concurrency.py`、`verify-lock-contention.py`、`verify-execution-finalization.py`、`verify-query-plans.sh` |
| [support/](support/) | 共享测试客户端、隔离数据库、故障注入和确定性夹具生成 | 由验证程序导入；`overview-fixtures.py` 也可直接运行 |
| [fixtures/](fixtures/) | 跨组件共享证书、配置、schema 评估语料与固定结果 | 保留固定输入和结果 |
| [fixtures/runtime/](fixtures/runtime/) | 测试节点镜像、网络隔离脚本和测试 CA 服务 | `systemd-node.Dockerfile`、`egress-guard.Dockerfile` |

Python 依赖见 [requirements.txt](requirements.txt)。从仓库根目录执行，例如：

```sh
python3 -m pip install -r tests/requirements.txt
python3 -m unittest discover -s tests/unit -p 'test_*.py'
ruby tests/contracts/verify-management-contract.rb
go -C tests/integration/traffic-clear test -count=1 ./...
bash apps/macos/Tests/runners/verify-localization.sh
```

组件专属测试靠近组件：独立 macOS 回归源码与运行入口位于 [apps/macos/Tests/](../apps/macos/Tests/README.md)，UI 用例保留在 `apps/macos/UITests/`，Rust 测试继续位于所属 Rust 模块中。

性能与功能验证分别调用。正式锁争用验收、快照容量和查询计划有独立规模、时长和环境要求，冒烟模式不能替代正式结果。环境要求及完整矩阵见 [部署与回归验证](../docs/deployment-testing.md)。构建、生成、运维和实际迁移工具见 [操作脚本索引](../scripts/README.md)。

`python3 tests/performance/summarize-pool-comparison.py <本地证据目录>` 仅汇总已取回的生产池大小交叉观察，不连接生产。结果分开列出启动与稳定窗口、慢获取分段、就绪探测延迟、采集终态和资源消耗；阈值日志不构成全量 acquire 分布，三个完整周期的生产观察也不替代容量验收。执行与边界见 [0035](../docs/plans/0035-pool-size-comparison.md)。相关配置保存/失败恢复和时间边界测试可用 `python3 -m unittest discover -s tests/unit -p 'test_pool_comparison*.py'` 执行。

`python3 tests/performance/benchmark-maintenance-scheduler.py --baseline <冻结基线测试二进制> --candidate <冻结候选测试二进制> --output <新证据目录>` 在自管隔离 PostgreSQL 上按 ABBA 顺序比较后台空预检合并。默认连接池 100，每轮 120 秒、预热 30 秒；两份二进制须包含相同的池大小与窗口汇总测试插桩。汇总区分实际数据库空预检与内存跳过，协议命令总数包含预热；回环 SSH 故意失败，不能作为采集成功率或容量验收。实现及结果见 [0038](../docs/plans/0038-maintenance-poll-coalescing.md)。解析器测试：`python3 -m unittest discover -s tests/unit -p test_maintenance_scheduler_benchmark.py`。 加 `--redis` 会创建隔离 Redis，并要求两份二进制的相同夹具都启动配额排行榜 worker；其直接只读事务计入协议总量，但不计入 db helper 事务汇总。

`TEST_DATABASE_URL=<可丢弃本地 PostgreSQL> RUST_MIN_STACK=16777216 cargo test --locked -p hysteriax-server node_limits::tests::background_idle_probes_at_target_cardinality_are_bounded -- --ignored --exact --nocapture --test-threads=1` 显式创建 10 万用户、1 万节点，验证采样周期成员冻结、空闲节点扫描使用到期索引，以及管理任务/采样过期/统计队列在该基数下的空闲路径。只做数据库与分页检查，不运行 SSH，也不证明采集容量。后台优化说明与同池对照见 [0042](../docs/plans/0042-background-idle-work-reduction.md)。

`python3 tests/integration/verify-maintenance-startup.py --image <本地发布镜像> --output <新证据目录>` 在自管内部 Docker 网络与 PostgreSQL/Redis 上验证完整 linux/amd64 服务默认线程栈启动。默认运行 330 秒以覆盖 300 秒维护兜底，检查持续就绪、零重启和 SIGTERM 后三类采样排空、正常退出；只清理带本次专属标签的夹具，保留日志和 inspect 证据。测试验证 0048 自动删除一条到期采样工作，0049 启动 9 个 worker 并删除一条过期审计记录。该场景用于启动与功能验证。发布栈回归见 [0039](../docs/plans/0039-deploy-maintenance-scheduler.md)。
