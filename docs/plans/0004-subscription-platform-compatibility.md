# 0004 订阅版本与平台兼容性

实施日期：2026-10-18（Asia/Shanghai）

## 目标与实施

1. sing-box 最低版本统一为 1.14.0，已知旧内核返回 422，包括空订阅；移除旧版字段降级路径。未知版本保留现有完整导出行为；ECH 仍保留 1.14.2 验证门槛。
2. 增加平台类型和 UA 检测，以及显式 platform 参数覆盖；支持 Linux/OpenWrt、Windows、macOS、Android、iOS，未知平台保持原模板。
3. sing-box 按平台过滤透明代理入站及 TUN 参数；Mihomo 在 Windows/移动端移除 redir-port。固定 YAML 地址保持版本无关节点契约，同时支持平台过滤。
4. 缓存键加入平台并升级命名空间；固定 YAML 输出也设置 Vary: User-Agent。
5. 更新选择页版本说明、订阅文档、单测和 HTTP 集成验证，覆盖旧版本拒绝、平台优先级、缓存隔离和节点材料保留。

## 验证

- `cargo test -p hysteriax-server formats`：14 项通过。
- `cargo clippy -p hysteriax-server --all-targets -- -D warnings`：通过。
- `cargo fmt --all --check`、`git diff --check`：通过。
- 本地 PostgreSQL 隔离 schema 运行 `scripts/verify-subscription.sh`：通过，覆盖旧版本/空订阅拒绝、显式平台优先级、UA 平台识别、跨平台缓存隔离、固定 YAML 地址过滤、sing-box 1.14.2 各平台输出解析与真实 SOCKS 转发，以及 Mihomo 1.19.31 高级订阅解析。
- `ruby scripts/generate-swift-api-models.rb --check`、`scripts/verify-subscription-client.sh`：通过；接口规范和生成客户端均包含可选 platform 参数。
- OpenAPI Redocly lint：通过。
- 实机边界：本次验证配置过滤、解析和本机转发，未声称完成 Windows/Android/iOS 各客户端实机运行验证。

## Server1 部署（2026-10-18，Asia/Shanghai）

- 按用户部署指令，目标为 Server1（hysteriax.cc）。本地构建 linux/amd64 镜像 `hysteriax-server:subscription-platform-0004-20261018`，经 SSH/rsync 增量传输镜像归档（直传较慢后改用旧镜像归档作为比对基础），服务器不现场编译。
- 本地镜像 ID：`sha256:057c45ede71e52b3162d69b8d1d1ddd3b431993b7c429dd5d04ad93ecc3a12df`；管理二进制 SHA-256：`8efc062aa083159b90a75efe360e0e6e2a652e4615a5b5b61dc707867ce322bd`。镜像内两种架构 ACME 组件及许可包校验通过。
- 升级前版本 `hysteriax-server:singbox-version-gate-20261018` 保留回退；schema 为 36。备份目录：`/home/nieaowei/HysteriaX/backups/subscription-platform-0004-20261018/`，包含环境文件、节点配置摘要快照、数据库备份和部署回执。
- 镜像归档 SHA-256：`85e6f9d183a91b4c5028e21c83c97437799ae1615cdc3ab287a84f6b874f63e9`，大小 166,350,848 字节，两端完全一致。服务器导入后镜像 ID 为 `sha256:37bb3fc56ae54fc818bc5d9d869270ba5ba32e7b31411d85bdde9b248b660988`；由于两端 Docker 镜像存储表示不同，已逐项确认 RootFS 层、运行配置、架构和 OS 完全一致，并校验服务器镜像内管理二进制及 ACME 产物。
- 升级前数据库备份大小 58,397,450 字节，schema 36，权限 0600；同目录保存 `env.before`、节点前后快照、校验脚本和 `receipt.json`。
- 于 2026-10-18 11:00（Asia/Shanghai）切换管理 API，容器内及公网 HTTPS `/readyz` 均 ready，schema 保持 36，Docker health 为 healthy，容器重启次数 0，启动后 API 日志错误行数 0。
- 线上使用现有订阅只读验收：sing-box 1.11/1.12/1.13 均返回 422；Linux/OpenWrt、Windows、macOS、Android、iOS 的 sing-box 与固定 Mihomo YAML 过滤符合预期；UA 检测、显式覆盖、跨平台缓存隔离、非法参数返回 400 均通过。6 个节点及 1 个 mTLS 节点完整保留，跨平台节点参数完全相同。
- 6 个远端节点的目标/部署修订、配置摘要和 mTLS 状态前后快照完全一致，未重新部署远端节点。旧镜像 `singbox-version-gate-20261018` 保留用于回退。
