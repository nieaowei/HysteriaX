# 后端全量扫描排查

日期：2026-10-15。代码与线上均包含 0030 在线补算扫描修复。

检查方法：代码审查并执行线上只读 EXPLAIN ANALYZE，设置局部 jit=off、force_custom_plan、8 秒语句超时。检查历史趋势、后台汇总 SELECT、审计/任务分页与搜索、凭据目录和采样清理。数据见同目录 20261015_full_scan_audit.json。表规模来自 pg_stat_user_tables 的估算，查询扫描行数来自实际执行计划；首次读取耗时包含冷页面影响，不作为稳定性能承诺。

## 优先处理

### 1. 后台小时汇总扫描完整分钟表

位置：crates/hysteriax-server/src/analytics/refresh_rollups.sql:2–5。

traffic_dirty_hours JOIN traffic_minute_totals 实测只有 5 条待处理小时，但使用并行顺序扫描，两个工作者合计读取约 133,141 行分钟汇总，SELECT 部分执行 535.486 ms。

这不是读取本窗口全部样本的必需成本。应按待处理的 source、node_id、hour 驱动参数化范围查询，避免全表扫描。后台汇总在 db::begin_write 的共享写入锁事务中运行，因此此类读取可能延长其他写事务的等待。本轮没有执行生产 INSERT/DELETE 来测量完整事务，只测了其原样 SELECT 部分。

### 2. 凭据列表内部仍全量构建后分页

位置：crates/hysteriax-server/src/api/credentials.rs:76、166。

catalog_on 读取全部托管凭据及当前版本、引用、管理员令牌、订阅凭据和节点分配，合成完整目录。list 在 Rust 中过滤、更新状态、排序，最后 skip/take 分页。命中目录缓存可以省去静态构建，但未命中或请求 no-cache 时会重建；客户端再次进入已有列表时发送 no-cache，因此缓存不能消除这一路径。

线上托管凭据仅约 10 条，catalog 主查询当下约 0.491 ms；这是随目录规模增长的结构性问题，不能把整个列表耗时等同于这条主查询耗时。建议把合成条目组织成可查询的统一目录，分类/筛选/排序/分页下推，只为本页补引用及运行状态。

## 随历史数据增长会加重

### 3. 审计列表总数及自定义排序

位置：crates/hysteriax-server/src/api/mod.rs:613–640。

每次请求都精确 count(*)；无搜索时仍遍历整个审计表。按 action 等无匹配排序索引的字段排序也读取全表再取第一页。本轮 audit_total 和 audit_action_page 均扫描 447 行；当前表小，暂不是最大瓶颈。

建议无条件总数使用事务性计数，常用排序建立相应复合索引；有条件的精确计数需保留筛选语义，不能直接套用全局计数。

### 4. 任务和审计自由文本搜索

位置：crates/hysteriax-server/src/api/mod.rs:486、623。

strpos(lower(concat_ws(...)),lower(q)) 搜索不能直接利用当前普通 B-tree 索引。jobs_search 实测遍历全部 238 条任务，audit_search 遍历全部 447 条审计。搜索 COUNT 和分页读取各有独立查询。

建议设计可索引的搜索字段，并采用可利用相应索引的查询操作符；必须保持当前百分号、下划线按字面字符匹配的语义，不能简单把 strpos 替换成未转义的 LIKE。

### 5. 任务按状态等字段排序仍可能遍历全部任务

位置：crates/hysteriax-server/src/api/mod.rs:462–504。

默认 created_at 排序实测只读取 50 条；status 排序实测遍历 238 条（走索引但仍读全量）再取 50 条。全量处理不只表现为 Seq Scan。应按常用排序补充匹配 ORDER BY 和稳定 ID 次序的复合索引；任意深页 OFFSET 本身也需要遍历前面的索引记录。

## 需要单独设计，不能视作无效扫描

### 6. 探测精确分位数

位置：crates/hysteriax-server/src/analytics/probe_history.sql:23–29。

P50/P95 仍对原始成功样本做 percentile_cont。7 天查询扫描约 66,212 行，保留 47,260 行；30 天保留 65,747 行。查询覆盖表中大部分数据，优化器选择顺序扫描有合理性。24 小时查询使用 Bitmap Heap Scan，约读取 7,143 条成功样本。

需要降低这部分读取量时，应单独设计精确或近似分位数汇总及其统计口径；不能承诺加普通索引即可消除这类全量读取。

### 7. 小目录及业务必要的集合读取

授权组目录读取全部组和成员；采样循环列出全部已部署节点；用户到期检查每轮按启停、到期和额度条件检查用户。当前节点 5、用户 7，这些小表的 Seq Scan 不应仅因名称被判定为性能缺陷。授权组全量返回是现有接口契约，要降低大规模时的数据量需设计分页/成员按需读取。

## 本轮已验证正常或尚不能认定为全量读取

- 流量趋势 users/network 的 24h/7d/30d 整小时窗口：原始 traffic_records/node_network_samples 扫描次数为 0。
- 7d 非整小时窗口：分钟汇总按范围使用 Bitmap Heap Scan；整分钟窗口的原始记录仍 0 次扫描，非整分钟窗口仅通过索引补边界。
- 探测计数的整分钟窗口：原始边界分支执行次数为 0；原始探测读取来自上文的精确分位数。
- proxy_probe_samples、online_samples 的 31 天过期样本查找均使用索引，当前没有过期行。
- 最新节点在线及探测读取已有按节点的 LATERAL LIMIT 1；默认任务分页通过排序索引读取 50 条。
- 后台 online_dirty_minutes JOIN online_cycle_metrics 在本次空待处理队列下，虽然计划中出现并行 Seq Scan，但各分支只取到约 1 行即停止，不是读完整张表。非空待处理队列场景仍有相似规划风险，后续修改后台汇总时应一并验证，不能仅凭 Seq Scan 字样认定已发生全表读取。

本次只排查，没有继续修改查询或部署新版本。

## 后续实施

后台重建/删除、列表搜索/排序、审计总数以及凭据页摘要加载已按 docs/plans/0031_query_optimization_plan.md 优化并部署。上文保留为优化前的诊断记录；精确分位数、大比例匹配与目录头部计数的成本仍需按相应口径评估。
