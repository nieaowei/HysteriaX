# 文档索引

本目录保留当前使用、开发与运维指南。已提交的历史计划、审计和验收记录可从 Git 历史查阅。

## 安装与运维

| 文档 | 内容 |
| --- | --- |
| [项目入门](../README.md) | 功能、首次连接和常用操作。 |
| [安装与升级](installation.md) | 本机构建并传输镜像、维护迁移、多实例及开发环境。 |
| [备份与恢复](backup-restore.md) | 数据库、主密钥、外部操作时间线及节点 ACME 私钥。 |
| [故障排查](troubleshooting.md) | 就绪、协议、并发写、订阅和 HTTPS 故障。 |
| [发布](release.md) | Universal DMG、容器发布及版本切换。 |
| [兼容性](compatibility.md) | 客户端构建要求、固定内核及功能边界。 |

## 业务与开发

| 文档 | 内容 |
| --- | --- |
| [架构](architecture.md) | 维护入口、事务、租约、缓存和事件协议。 |
| [凭据](credentials.md) · [托管 mTLS](managed-mtls.md) | 凭据版本、CA 轮换和身份迁移。 |
| [授权组](authorization-groups.md) · [节点套餐](node-packages.md) | 用户授权及整机配额。 |
| [订阅](subscriptions.md) · [字段覆盖](field-coverage.md) | 模板发布、平台选择及配置支持。 |
| [DNS](dns.md) · [节点 ACME](node-acme.md) | 域名管理及节点独立证书维护。 |
| [验证指南](deployment-testing.md) | 验证入口、隔离要求及发布边界。 |

## 版本与维护

当前管理协议为 2，管理 API 使用 `/api/v2`，旧 `/api/v1` 返回 426。所有普通 API 停止后，单独运行 `--migrate-only`，校验完成再启动。数据库、主密钥及节点 ACME 私钥的保存与恢复方式见 [备份与恢复](backup-restore.md)。
