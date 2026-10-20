# 0013 部署单份订阅模板配置

实施日期：2026-10-19。

## 目标

将单份订阅模板配置的服务端支持部署到 Server1 的 https://hysteriax.cc，保留现有模板、发布版本及回退能力。

## 已实施

1. 检查现有 Compose 服务、架构、磁盘空间及内外健康状态。
2. 按项目要求在本地通过 `hxamd64` Buildx 构建 `linux/amd64` 镜像，执行镜像构建内的模板运行时自检。
3. 通过 `docker image save`、压缩及 SSH 传输镜像，在服务器加载；未在服务器构建代码。
4. 保存部署前模板修订号、发布指针和草稿摘要，备份 `.env`、Compose 配置及旧镜像标识。通过项目备份脚本生成数据库归档，数据库 schema 版本仍为 37。
5. 更新服务器 `.env` 的镜像名称和版本，仅替换 API 容器。保留数据库、Redis、Caddy 和节点代理服务。
6. 检查运行镜像、容器健康、公开 HTTPS 就绪接口、两种格式的单份配置校验，以及部署前后的模板摘要。

## 发布制品

- 镜像：`hysteriax-server:single-config-20261019-203822`。
- 本地镜像 ID：`sha256:92e7da345bce7dffa0b33bb45bf9c47e91285d3a51b955cf5cc158e262c1127e`。
- 服务器加载后的镜像 ID：`sha256:5ea56539684aaa4034d7a074b2ce8f97fce902a1580b579bf08ed9378fb5ac0a`。
- 导入前后架构、操作系统、创建时间、完整 Config 和 RootFS 文件层一致；管理服务、Hysteria 二进制及 ACME 校验清单的 SHA-256 一致。
- 管理服务二进制 SHA-256：`c9b61873fed1927bbff749061ef49aa4b97a9f03274f87f225cf46e16a521920`。

## 备份与回退

- 服务器备份目录：`/home/nieaowei/deployment-backups/single-config-20261019-203822`，目录权限为 700。
- 数据库归档：`database.tar.gz`，70,202,847 字节，权限为 600。
- 部署前镜像：`hysteriax-server:0007-template-codegen-20261019`，未删除。
- 目录中保存部署前 `.env`、Compose 配置、镜像信息、模板前后摘要和最终验证记录；敏感配置未复制到本地仓库。
- 如需回退镜像，恢复该目录中的 `.env` 后仅重建 API 容器。若更新后已保存第 2 版模板文档，应使用支持该格式的服务端，不能直接降级为旧版编辑服务。

## 验证结果

- API 容器为新镜像，状态 healthy。
- 内部及公开 `https://hysteriax.cc/readyz` 返回 `{"database":"ok","status":"ready"}`。
- Mihomo、sing-box 第 2 版单份配置均通过部署后 API 校验。
- 部署前后均为 2 份模板，草稿内容摘要、修订号、默认指针、归档状态及发布版本完全一致。
- 未替换本地正在运行的 Mac 客户端进程；配套客户端构建已在上一任务完成。
