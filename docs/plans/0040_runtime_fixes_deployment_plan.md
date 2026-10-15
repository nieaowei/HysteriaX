# 0040 运行可靠性修复部署

日期：2026-10-16（Asia/Shanghai）。源代码提交：7447bb0。

## 目标与实施

将 0034–0039 的任务结果持久化重试、SSE 撤销、SSH 执行限制、并发流量采样、服务关闭和客户端摘要接口部署到 Server1：https://hysteriax.cc。

1. 使用项目 Dockerfile 在本地构建 linux/amd64 镜像 `nieaowei/hysteriax-server:runtime-fixes-20261016-7447bb0`，执行独立数据库的 Linux 镜像冒烟测试。
2. 导出压缩镜像，通过 SCP 推送至服务器，校验 SHA-256 后 docker load；本次没有在服务器编译，也没有发布到公共镜像仓库。
3. 保存旧镜像标记、受保护的 .env/Compose 文件和 PostgreSQL 一致性备份（40,937,508 字节），pg_restore --list 验证备份可读。
4. 确认无 running 任务后停止旧 API，在新镜像中执行 --migrate-only；迁移检查前后业务配置和计费摘要一致，数据库仍为迁移 0028。只重建 API 容器。
5. 验证线上就绪、能力、摘要、目录分页、历史查询、服务监控及未鉴权请求拒绝，并观察真实采样持续推进。

## 结果

- API healthy，RestartCount=0；线上二进制与本地镜像 SHA-256 一致：`bc76c515f0d35414f87ec26317d706c32932cb780ec68d003f73e1d256aa2488`。
- 保留 5 个节点、7 个用户、10 个托管凭据，业务配置摘要不变。统一凭据目录另含合成条目，分页总数为 47。
- PostgreSQL、Redis、Caddy 保留原容器实例。
- 新 client_summary 能力和接口上线，全局节点/用户计数与分页总数一致；无令牌访问返回 401。
- 20 秒观察期间，5 个节点的 last_sample_at 均推进；无容器重启。
- 本地 Linux 镜像在保持 SSE 连接时正常退出，ExitCode=0，实测约 0.15 秒。该退出测试没有在业务服务上额外重启执行。
- 没有创建线上测试用户、凭据或管理员令牌，没有修改线上配额或配置。

## 备份与回退

目录：`/home/nieaowei/HysteriaX/backups/runtime-fixes-20261016-7447bb0`。

该目录包含数据库 dump、校验摘要、受保护配置、旧镜像标记和 `rollback.sh`。回退脚本恢复旧 .env 并重新创建 API，使用 `nieaowei/hysteriax-server:redis-rank-20261015-v1`。本次无新增数据库迁移，回退无需还原业务数据库。

详见 [部署验证数据](0040_runtime_fixes_deployment_verification.json)。
