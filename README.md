# HysteriaX

HysteriaX 是一个自托管的 Hysteria 2 节点与用户管理工具。通过 macOS 原生客户端集中配置服务器、部署代理节点、管理用户权限和订阅，并查看流量、在线连接及运行状态。

项目由 Rust 管理服务和 SwiftUI macOS 客户端组成。管理服务负责保存数据、执行 SSH 任务和持续采样；Mac 客户端用于日常操作，关闭客户端后服务端仍会继续工作。代理流量由用户的代理客户端直接连接节点。

## 主要功能

| 功能 | 说明 |
| --- | --- |
| 节点管理 | 管理 SSH 连接、公网地址和 Hysteria 配置，支持部署、配置同步、回滚及卸载；查看任务进度和失败原因。 |
| 用户与授权组 | 创建用户，设置启用状态、到期时间和流量额度；通过授权组批量分配节点，同一用户可加入多个组。 |
| 订阅与导出 | 生成自动识别客户端的订阅地址，支持 Mihomo YAML、sing-box JSON、Base64 和 Hysteria2 URI，也可导出本地文件。 |
| 流量与监控 | 查看节点、用户用量、在线连接和历史趋势，识别采样延迟与统计缺口。 |
| 节点套餐 | 设置整机流量额度、有效期、计费方向和月度重置规则；支持预警、超限限制及额度恢复后自动解除限制。 |
| 凭据中心 | 集中管理 SSH 密钥或密码、TLS 证书、CA、ECH、DNS 凭据及管理员令牌，支持版本管理、跨节点复用和到期提醒。 |
| DNS 管理 | 对接 Cloudflare，管理 A、AAAA、CNAME 记录，为节点分配或绑定域名，并在 SSH 公网 IP 更新时安排对应 DNS 更新。 |
| 任务与审计 | 保存部署、同步、撤权等任务的进度及结果，支持重试、服务重启后恢复和操作审计。 |
| Mac 客户端 | 支持简体中文与英文、Keychain 保存登录令牌、应用内提醒和可选系统通知；离线时可查看此前缓存的概览。 |

SSH、节点连接和订阅等敏感材料在服务端加密保存。管理员令牌仅在创建时显示明文，配置修改带有版本校验，避免多端操作相互覆盖。

## 开始使用

首次使用按以下顺序进行：**部署管理服务 → 连接 Mac 客户端 → 添加并部署节点 → 创建用户与授权组 → 导入订阅**。

### 1. 准备环境

- **管理服务器**：安装 Docker Engine、Docker Compose 插件、Git 和 OpenSSL；准备一个指向该服务器的域名，并放通 TCP 80、443，供管理 HTTPS 和证书签发使用。
- **代理节点**：准备可通过 SSH 访问的 Linux 服务器。已验证的系统包括 Debian 12/13、Ubuntu 22.04/24.04，架构包括 amd64 和 arm64；部署依赖 systemd，需要 root 或可用的 sudo 权限。
- **网络**：管理服务需要能够 SSH 连接节点，节点需要放通所配置的 Hysteria UDP 端口或端口范围。项目不会自动修改云安全组或主机防火墙。
- **管理客户端**：使用运行 macOS 26 或更高版本的 Mac。

管理服务域名用于访问管理 API 和订阅；节点连接地址用于代理流量，两者可以使用不同的域名和服务器。

### 2. 部署管理服务

先在本机构建镜像，再传输到管理服务器。两端使用相同版本的项目文件；以下以 `hysteriax-server:dev` 为例，正式部署应使用固定版本标签。

在本机项目目录执行：

```sh
docker build -t hysteriax-server:dev .
docker save hysteriax-server:dev | gzip > hysteriax-server.tar.gz
scp hysteriax-server.tar.gz user@server:/tmp/hysteriax-server.tar.gz
```

本机与服务器架构不同时，改用 `docker buildx build --platform linux/amd64 --load -t hysteriax-server:dev .`（arm64 服务器选择 `linux/arm64`）。随后在管理服务器执行，并替换管理域名：

```sh
git clone https://github.com/nieaowei/HysteriaX.git
cd HysteriaX
scripts/init-secrets.sh manage.example.com
gzip -dc /tmp/hysteriax-server.tar.gz | docker load
docker compose up -d postgres redis
docker compose --profile maintenance run --pull never --rm --no-deps migrate
docker compose up -d --no-build api caddy
```

数据库初始化和升级必须先运行 `migrate`。普通 API 启动只检查 schema、协议和主密钥，不执行初始化改写。已有安装升级前先备份并停止所有 API 实例，完成迁移后再启动。

Compose 会启动管理 API、PostgreSQL、Redis 和 Caddy。Caddy 自动申请和续期管理域名的 HTTPS 证书；数据库和 Redis 不向宿主机公开端口。

初始化脚本生成权限为 `600` 的 `.env`，包含管理员令牌、主密钥和数据库等凭据。首次连接客户端时使用其中的 `HYSTERIAX_ADMIN_TOKEN`。脚本拒绝覆盖已有 `.env`，已有安装不要重新初始化。

请单独备份 `HYSTERIAX_MASTER_KEY`，并妥善保存 `.env`；数据库备份无法替代主密钥，丢失主密钥后无法解密已保存的敏感数据。不要把这些文件提交到仓库。

检查服务是否就绪：

```sh
docker compose ps
curl --fail https://manage.example.com/readyz
```

就绪接口应返回：

```json
{"status":"ready","database":"ok","management_protocol":2}
```

安装、已有数据库升级和旧 SQLite 安装的处理方式见 [安装指南](docs/installation.md)。

### 3. 安装并连接 Mac 客户端

从 [GitHub Releases](https://github.com/nieaowei/HysteriaX/releases) 下载已发布版本的 DMG，打开后将 `HysteriaX.app` 拖入 `Applications`。发布包未进行 Developer ID 签名和公证；如 macOS 阻止打开，按系统安全设置中的提示允许运行，详见 [发布说明](docs/release.md)。

也可以在安装 Xcode 的 Mac 上，从项目根目录构建并运行：

```sh
./script/build_and_run.sh
```

打开 HysteriaX 的「设置」：

1. 填写管理服务的 HTTPS 地址，例如 `https://manage.example.com`。
2. 填写 `.env` 中的 `HYSTERIAX_ADMIN_TOKEN`。
3. 点击「验证并保存」，确认显示「已连接」。

令牌保存在 macOS Keychain 中。后续可在「凭据」中心创建、切换和撤销管理员令牌；「设置」中也可切换语言及启用系统通知。

### 4. 添加并部署节点

1. 在「节点」页面添加服务器，填写 SSH 地址、端口、用户名及认证凭据。公网连接地址可使用外部域名/IP，也可通过已连接的 Cloudflare 分配或绑定域名。
2. 测试 SSH 连接，核对并确认服务器指纹。
3. 打开「代理配置」，设置 Hysteria 监听地址、UDP 端口、TLS 证书或 ACME，以及需要的带宽、混淆和路由配置。
4. 发起部署，在「任务」页面查看进度；成功后确认节点显示为已部署。
5. 后续修改代理配置时使用「保存并同步」。需要恢复旧配置时使用回滚操作，并检查对应任务结果。

「服务器配置」用于调整基本信息、SSH 和有效期与流量套餐；「代理配置」用于设置并下发 Hysteria 配置。SSH 测试只验证连接，不会部署代理。

证书和私钥可在「凭据」中心统一管理，再由节点引用。Cloudflare 域名配置见 [DNS 管理](docs/dns.md)，套餐与整机流量统计见 [节点套餐](docs/node-packages.md)。

### 5. 创建用户并分配权限

1. 在「用户」页面创建用户，按需设置有效期、流量额度和启用状态。
2. 在页面工具栏切换到「授权组」，创建组并选择成员用户及可用节点。
3. 检查权限变更预览后保存。用户加入多个组时，获得这些组的节点权限并集。
4. 如节点启用 mTLS，按提示为对应用户绑定个人客户端证书。

当前版本通过授权组管理节点权限。移除一个组的授权时，只要其他组仍提供同一节点的权限，用户就仍可访问。用户自身的停用、到期和额度限制持续生效。具体操作见 [授权组](docs/authorization-groups.md)。

### 6. 获取并使用订阅

先在「订阅模板」中编辑并发布全局模板，再在用户的订阅菜单中明确选择模板和目标平台，复制地址或导出配置。地址始终读取该模板的最新发布版本；保存草稿不影响正在使用的订阅。浏览器打开自动地址时，会显示模板与平台选择页面。

| 格式 | 用途 |
| --- | --- |
| Mihomo YAML | 导入 Mihomo / Clash.Meta 内核客户端。 |
| sing-box JSON | 导入 sing-box；监听、DNS、分流规则和节点选择器由发布模板管理。 |
| Base64 / URI | 导入支持 Hysteria2 节点链接的客户端。 |

可导出包含全部平台覆盖和注入规则的完整模板包，也可按用户及目标平台导出最终 `.yaml`、`.json` 配置。Base64 / URI 直接按授权节点生成。订阅仅使用已经成功部署的节点配置；高级特性会按客户端能力筛选，详见 [订阅格式与兼容性](docs/subscriptions.md)。

订阅地址包含访问令牌，请按密码保管。用户停用、到期或额度耗尽会限制订阅和代理访问；更换订阅令牌后，旧地址失效。

## 日常维护

- **查看运行状态**：在概览查看流量和连接趋势，在节点与用户详情确认采样时间；任务失败时先查看「任务」中的具体错误，操作历史可在「审计」中查询。
- **查看服务日志**：在服务目录运行 `docker compose logs --tail=100 api`；证书或 HTTPS 连接异常时同时检查 `caddy` 日志。
- **备份与恢复**：按 [备份指南](docs/backup-restore.md) 备份数据库、匹配的主密钥和必要的 Caddy 状态。备份脚本会短暂停止 API 以生成一致快照。
- **升级**：先备份并停止全部旧实例，完成迁移后更新管理服务与配套 Mac 客户端。管理接口使用 `/api/v2`；升级前查阅 [安装指南](docs/installation.md) 和对应版本说明。

节点套餐统计所选网卡的整机流量，用户额度统计用户的 Hysteria 用量，两者口径不同。采集和撤权存在延迟，SSH 不可达时可能无法立即断开已有连接，具体边界见 [节点套餐](docs/node-packages.md)。

## 开发与 API

服务端为 Rust + Axum，使用 PostgreSQL 18 保存数据、Redis 提供缓存；Mac 客户端使用 SwiftUI。Rust 最低版本声明为 1.89，客户端构建使用 Xcode；SwiftPM 构建要求见 [Package.swift](apps/macos/Package.swift)。

管理 API 位于 `/api/v2`，请求使用 `Authorization: Bearer <管理员令牌>`，写请求还需携带稳定的 `Idempotency-Key`。服务提供 `/openapi.yaml`，契约源文件为 [openapi/openapi.yaml](openapi/openapi.yaml)。列表接口使用分页响应，批量读取需遍历各页。

本地数据库配置与测试环境准备见 [安装指南](docs/installation.md#local-development-and-tests)。配置好测试数据库后，可运行以下常用检查：

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
ruby scripts/generate-swift-api-models.rb --check
```

Mac 客户端的 API 模型由 OpenAPI 生成。构建并检查应用能否启动：

```sh
./script/build_and_run.sh --verify
```

## 详细文档

完整使用与运维指南见 [文档索引](docs/README.md)。已提交的历史计划、审计及阶段记录可从 Git 历史查阅。

| 文档 | 内容 |
| --- | --- |
| [安装指南](docs/installation.md) | 服务部署、本地环境和版本迁移。 |
| [凭据中心](docs/credentials.md) | SSH、证书、令牌及凭据版本管理。 |
| [授权组](docs/authorization-groups.md) | 用户与节点授权、批量成员管理。 |
| [订阅格式](docs/subscriptions.md) | 格式识别、客户端要求和节点过滤规则。 |
| [节点套餐](docs/node-packages.md) | 有效期、整机流量统计、额度重置和预警。 |
| [节点 ACME](docs/node-acme.md) | 独立证书维护、SAN 与 mTLS、节点备份及恢复。 |
| [DNS 管理](docs/dns.md) | Cloudflare 连接、记录管理和节点域名绑定。 |
| [配置字段覆盖](docs/field-coverage.md) · [兼容性](docs/compatibility.md) | 支持的 Hysteria 配置及功能限制。 |
| [备份与恢复](docs/backup-restore.md) · [故障排查](docs/troubleshooting.md) | 数据保护与常见问题处理。 |
| [架构](docs/architecture.md) | 组件职责、数据存储与代码结构。 |
| [部署测试](docs/deployment-testing.md) · [发布说明](docs/release.md) | 部署验证方法与安装包发布。 |
