# 0030 消除在线趋势补算分支的多余扫描

状态：已实施并部署（2026-10-15）。

## 问题

online_history.sql 的 ranges 与 online_cycle_metrics 使用普通 JOIN。数据库可能先扫描周期表，再与补算时间段连接，即使 ranges 为空仍有扫描。诊断中输出该范围下 47,630 行周期记录，最后补算分支返回 0 行。

## 实施

1. 将补算分支改成 ranges 驱动的参数化 CROSS JOIN LATERAL，利用 scope_node_id、started_at 索引。
2. 使用 OFFSET 0 阻止优化器把子查询展开回普通 JOIN；没有边界或待聚合分钟时，不执行周期表扫描。
3. 保留分钟汇总和实际需要补算的数据，统计口径和接口结构不变。
4. 扩展原始周期数据对照测试：24h/7d/30d、整分钟/非整分钟、全部节点/单节点/无数据节点、已聚合/修改后待聚合/重新聚合；比较用户及连接平均值、峰值、覆盖率和缺失状态。
5. 新增 EXPLAIN ANALYZE 回归：干净整分钟窗口周期表 Actual Loops=0；存在边界或待聚合分钟时才执行补算。

## 验证和部署

- analytics::followup_tests 5 项通过，overview::integration_tests 16 项通过；严格 Clippy、格式及差异检查通过。
- 本地 hxamd64 构建 linux/amd64 容器镜像 online-history-scan-20261015-v1，再上传服务器；归档和可执行文件 SHA256 验证通过。
- 仅替换 API 容器，PostgreSQL、Redis、Caddy 容器身份保持一致，数据库版本仍为 0024。
- 线上全部节点和单节点的 24h/7d/30d 干净整分钟窗口，周期表 Actual Loops、Shared Hit Blocks、Shared Read Blocks 均为 0。
- 线上全部节点/单节点，整分钟及非整分钟窗口，与直接原始周期聚合双向 EXCEPT 差异均为 0。
- 六组质量趋势接口及概览摘要正常；接口仍返回 24/7/30 个时间桶。
- 备份及旧镜像配置保存在 /home/nieaowei/HysteriaX/backups/online-history-scan-20261015-v1，可恢复旧 .env 并重建 API 回滚。

部署后首轮接口仍有 2076.98 ms 的请求，部分单节点汇总首次执行也仍接近 1 秒；这不影响周期表扫描已被消除的验证结论。冷页面读取、首次准备和精确延迟分位数的剩余开销不在此次改动范围内，不能宣称首次加载波动已全部解决。

完整数据见 0030_online_history_scan_deployment_verification.json；诊断背景见 docs/diagnostics/20261015_overview_quality_first_load.md。
