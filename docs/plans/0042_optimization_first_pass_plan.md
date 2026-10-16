# 0042 优化报告纠正与首轮可靠性修复

日期：2026-10-16（Asia/Shanghai）。状态：已实施并完成本地验证；后续后端部署见 [0043](0043_optimization_deployment_plan.md)。

## 目标与实施

1. 修订 `docs/optimization-assessment.md`，保留 50 个条目编号；纠正 runner 标签、干净克隆、SQLx 默认值、默认请求超时、迁移执行及索引运算符等错误。按已确认、待测量、工程建议、待核实区分证据，撤回全量核验与未经测量的生产收益断言。
2. 凭据列表、详情和引用 GET 统一使用一致性快照：只读事务先检查引用索引脏来源；干净时不拿咨询写锁；脏时退出快照，独立写事务维护后重试。最多两轮维护后回退原权威写事务。拆分维护与纯查询，删除等写入仍同步维护引用并保护历史引用。
3. 迁移 0029 为 traffic_records 新增 `(user_id,node_id,sampled_at DESC)`、`(node_id,sampled_at DESC)` 索引；事务内建索引，lock_timeout=5s。回退脚本拒绝存在较新迁移的数据库，只删除新增索引和迁移记录。未来实际部署需停 API 维护窗口，本轮不部署。
4. 七类客户端分页统一静默处理 CancellationError、URLError.cancelled，保留旧内容与真实错误展示。共享请求仍不因单个消费者取消而被取消。
5. refresh 按连接会话合并执行，普通调用复用当前任务；执行期间的强制刷新合并为下一轮，后续轮次仍可合并新请求。调用方等待队列完成，isLoading 覆盖整段执行。切换连接取消并隔离旧任务，关键 await 后校验会话和取消状态，阻止旧结果和旧清理回填。
6. macOS CI 新增分页缓存、概览网络、DNS、授权组及 SSH 指纹脚本，保留 xcode-27。HTTP/OpenAPI/生成 DTO 无变化。
7. 提供 `scripts/benchmark-optimization.py` 与仅测试编译的 opt-in Rust 采样测量入口。固定种子 42、100 节点、1,000 用户、100,000 流量记录；独立 schema 比较旧/新 Debug 二进制，采集 EXPLAIN ANALYZE BUFFERS、接口分位数与错误率、实际采样应用事务时间，以及服务共享池的 SQLx 获取连接耗时日志、独立合成池的连接获取/咨询锁等待。

## 验证要求

- 凭据干净时持有全局写锁仍能完成三个 GET；脏来源维护、快照内一致性、持续变脏有界回退、既有配置和历史引用删除保护。
- 新建 schema、填充后升级、重复启动、用户/节点级联删除汇总一致及剩余用户计费不变；较新迁移拒绝回退、回退不改源数据、锁冲突原子回滚并可重试。
- 客户端取消分类、旧请求拒绝、共享消费者隔离、普通刷新合并、多轮强制刷新、加载状态、服务切换及错误恢复。
- Rust 全量测试、格式、严格 Clippy；Swift 网络/缓存/相关功能脚本、生成模型一致性；macOS 构建与启动。

## 复现方式与限制

使用可丢弃本地 TEST_DATABASE_URL，不读取项目 .env 或远程服务器凭据。先保存修改前构建的二进制，再执行：

```sh
cargo build -p hysteriax-server
python scripts/benchmark-optimization.py --before /path/to/before-binary \
  --output docs/diagnostics/0042_optimization_verification.json
```

需要服务共享池耗时时使用 `--profile-before-ref <修改前提交>` 替代 `--before`：脚本在临时源码副本启用 SQLx 现有 acquire_time_level 日志，前后构建使用同一探针，不修改仓库生产源码，也不新增诊断接口；结束后恢复普通本地二进制。副本保留用户已有的订阅模板资源，避免混入无关差异。

Python 使用 scripts/requirements-test.txt 中的依赖。性能脚本和 Rust 全量测试须串行运行：咨询锁是数据库级，独立 schema 仍会互相干扰。脚本创建并删除自己的 schema，不能指定线上 schema。

首轮 HTTP 请求只表示预热前，不代表清空 OS/PostgreSQL 缓存；SQLx 日志包含服务共享池的 HTTP 与后台获取连接耗时（含建连/健康检查，不等同纯信号量等待）；独立池合成测量不是 HTTP 池实际遥测。两个采样测量使用相同的真实 apply_sample 代码，生产采样算法未改；新索引可能增加写成本。所有本地数值不外推为线上收益。

## 验证结果

- `cargo test --workspace`：220 项通过，0 失败；1 个 opt-in 性能入口由性能脚本在前后两套独立 schema 显式执行并通过。
- `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`git diff --check` 通过。
- 分页缓存、概览网络、DNS、授权组、SSH 指纹客户端脚本与 `ruby scripts/generate-swift-api-models.rb --check` 通过。概览网络新增可控制响应延迟的多轮并发、错误恢复和服务切换验证。
- `./script/build_and_run.sh --verify` 构建并启动 macOS 应用成功；仅有本机同时匹配 arm64/x86_64 destination 的 Xcode 提示。
- `scripts/verify-migrations.py` 通过：新库启动、原生 schema 和第二实例拒绝。性能脚本另验证填充后 0028→0029、重复启动、较新迁移回退保护、源数据保留、5 秒锁冲突原子失败及重试、用户/节点级联删除和剩余用户计费不变。
- 使用相同 SQLx 日志探针的临时诊断构建进行最终对照，生产 db.rs 无改动。修改前源码基于 `153a5d3db7606a050907d1c16fd71ac2ae789df0`，两套副本均保留用户的本地订阅模板改动。完整结果见 [验证数据](../diagnostics/0042_optimization_verification.json)。
- 12 路 HTTP 混合请求中，各接口预热后及采样期间各 30 次，全部无错误。采样期间凭据列表 P95 1792.83→30.17 ms、详情 1797.21→19.72 ms、引用 1796.06→17.90 ms。该单次 Debug 对照只说明本地机制改善，不构成生产 SLA 或固定倍数承诺。
- 100,000 行下最新用户节点采样查询 7.656→0.077 ms；用户/节点外键查找由顺序扫描改为匹配新增索引的 Bitmap Heap Scan。1000 用户真实采样应用事务前 1726–1795 ms、后 1639–1788 ms，逐用户 SQL 成本仍存在。
- 服务共享池获取连接耗时 P95：预热后 0.959→1.146 ms，采样与合成争用期间 1.232→2.023 ms。此负载未显示持续池饥饿，不能据此推荐扩容；凭据接口改善主要对应移除不必要的写锁等待。
- 独立合成池负载 P95 获取连接等待前 563.25 ms、后 558.48 ms；咨询锁等待前 203.64 ms、后 253.24 ms。此项未改算法，差异不应解释为连接池优化成果。

## 保持边界

未改全局锁/SSE 游标协议、采样算法、连接池容量、搜索语义、订阅资格缓存、缓存键/容量/TTL 或连接失败策略。用户已有的 sing-box 模板改动保留，不属于本轮修改。
