# 0046 SSH 指纹连续流程部署

日期：2026-10-16（Asia/Shanghai）。状态：已部署并验证健康。上线时间：2026-10-16 15:39:54。

## 目标与实施

将 0044、0045 的 SSH 指纹确认、部署／同步／回滚首次信任继续执行，以及卸载失败后的 SSH 诊断恢复部署至 Server1（https://hysteriax.cc）。

1. 本地使用 Dockerfile 和 hxamd64 builder 构建 `linux/amd64` 镜像 `nieaowei/hysteriax-server:ssh-flow-20261016-0045`。隔离 PostgreSQL schema 验证镜像启动、0030 迁移、等待任务读取、确认接口鉴权及概览；保留固定版本 Hysteria 探测客户端。
2. 以 SCP 上传压缩镜像和精确构建源码。服务器执行 `docker load`，核对镜像归档 SHA-256、文件系统层、运行配置和服务端二进制；未在服务器编译，也未从 registry 拉取发布镜像。
3. 确认没有运行中的任务后停止 API，保留旧镜像和受保护的环境配置，生成 PostgreSQL custom dump 并通过 `pg_restore --list` 检查。备份包含 284 个源码文件的完整归档与摘要。
4. 新镜像执行 `--migrate-only`，将 schema 0029 升级至 0030。迁移前后源数据、配置、既有任务历史和计费摘要一致。只重建 API，PostgreSQL、Redis、Caddy 容器实例保持不变。
5. 验证公网 readyz、两项新指纹流程能力标记、确认接口鉴权／不存在任务响应、分页与全局统计、凭据详情及引用、6 份现有 sing-box 订阅、监控及历史接口，并观察 25 秒后台采样推进。

## 结果

- API healthy，RestartCount=0，启动后的 ERROR 日志行数为 0。
- 服务端二进制 SHA-256：`3d10b188add60387f2c7c1d25e133ec6c6b6bc5cbb765d0e01437a60e8d3901e`；本地镜像、远端加载镜像与运行容器内容已核对。
- schema 为 0030，`jobs_status_check` 包含 `waiting_confirmation`。
- 保留 5 个节点、7 个用户、11 个托管凭据；统一凭据目录总数为 48。迁移期间没有改变配置、计费或历史任务。
- 公网确认接口的未鉴权请求返回 401，已鉴权的不存在任务返回 404；验证过程中未创建或修改业务任务、用户、节点或凭据。
- 6 份 sing-box 订阅结构检查通过；5 个节点的 last_sample_at 全部推进。
- macOS 客户端使用已本地构建验证的改动，本次未打包或发布安装包。未对 Server2 或既有 Hysteria 节点执行部署、卸载或 SSH 配置变更。

## 备份与回退

备份目录：`/home/nieaowei/HysteriaX/backups/ssh-flow-20261016-0045`。

数据库 dump 为 44514179 字节，SHA-256：`b443dd76d5235fa86647869870f2ff0b2195ed35f33a8b8748ec5440c0f637d9`。环境配置副本权限为 0600，备份目录为 0700。旧镜像 `nieaowei/hysteriax-server:optimization-20261016-0042` 已保留。

回退入口为备份目录中的 `rollback.sh`：停止 API，使用 `rollback-ssh-test-confirmation.sql` 取消等待任务并记录取消事件、恢复旧状态约束和 schema 0029，再恢复旧环境配置及旧镜像。脚本拒绝跨越未知的更高 schema；不覆盖后续业务数据。自动失败回退已准备，本次未触发或在线演练回退。

完整结果见 [部署验证数据](0046_ssh_flow_deployment_verification.json)。
