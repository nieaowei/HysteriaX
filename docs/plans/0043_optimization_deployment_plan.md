# 0043 首轮优化后端部署

日期：2026-10-16（Asia/Shanghai）。状态：已部署并验证健康。

## 目标与实施

将 0042 的凭据读取优化和流量记录索引部署至 Server1（https://hysteriax.cc），保留已经上线的 sing-box 模板功能。macOS 客户端仍使用上一阶段已本地构建验证的版本，本次没有打包或发布 DMG。

1. 本地使用 Dockerfile 和 hxamd64 builder 构建 linux/amd64 镜像 `nieaowei/hysteriax-server:optimization-20261016-0042`，验证容器启动、新库迁移、概览/历史接口及固定版本探测客户端。
2. 导出压缩镜像，以 SCP 推送至服务器。归档 SHA-256、文件系统层、运行配置和服务端二进制均核对一致；服务器只执行 docker load，没有编译或重新拉取发布镜像。
3. 确认没有 running 作业后停止 API。保留受保护的 .env、Compose、旧镜像标记及回退脚本，生成一致性 PostgreSQL custom dump 并通过 pg_restore --list 检查。归档精确构建源码及完整文件摘要，覆盖未提交代码和模板资源。
4. 使用新镜像运行 --migrate-only，将 schema 0028 升级至 0029，创建两个 traffic_records 查找索引。核对停机前后源数据、配置与计费摘要，随后只重建 API；PostgreSQL、Redis、Caddy 容器实例保持不变。
5. 验证线上凭据列表、3 个凭据的详情/引用、鉴权拒绝、全局统计与分页一致性、监控与历史接口、6 份现有 sing-box 订阅及后台采样推进。

## 结果

- API healthy，RestartCount=0，新容器启动日志 ERROR 行数为 0。
- 线上二进制与本地镜像 SHA-256 一致：`22574228df9e59791d79046a894fc4be7d968a8a4039c674f78ba485ed5d0811`。
- schema 为 0029，`traffic_records_user_node_time_idx` 与 `traffic_records_node_time_idx` 均存在。迁移期间源记录和计费摘要不变，配置摘要也保持一致。
- 保留 5 个节点、7 个用户、11 个托管凭据；包含合成条目的统一凭据目录总数为 48。本次没有创建测试用户、凭据或作业，没有调整业务配置和配额。
- 6 份已有 sing-box 订阅的默认分组、组引用和模板占位符检查通过。
- 25 秒观察中，5 个节点的 last_sample_at 全部推进，服务保持 healthy 且无重启。
- 本地和远端加载后的镜像 ID 不同，归档、镜像层、运行配置及二进制摘要均已独立核对一致，两个 ID 均记录在验证数据中；不以标签或镜像 ID 单独证明交付内容。

## 备份与回退

备份目录：`/home/nieaowei/HysteriaX/backups/optimization-20261016-0042`。

数据库 dump 为 43,444,564 字节，SHA-256 为 `1ebde533c55043a1cada122714f956acf23df51bc9d44469c9bad1c15a08906e`。备份含 .env、Compose、旧镜像标记、回退 SQL/脚本、构建源码、摘要与验证结果。

旧镜像：`nieaowei/hysteriax-server:singbox-template-20261016`。

回退入口：备份目录中的 `rollback.sh`。脚本停止 API，使用带版本保护的 SQL 删除 0029 新索引和迁移记录，使 schema 恢复到旧二进制支持的 0028，再恢复旧 .env 并启动旧镜像。拒绝跨越未知的更高迁移；不还原业务数据库、不覆盖后续采样数据。自动失败恢复已安装，本次部署没有触发或在线演练回退。

完整结果见 [部署验证数据](0043_optimization_deployment_verification.json)。
