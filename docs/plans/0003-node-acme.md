# 节点独立 ACME 证书管理与 mTLS 共用

制定日期：2026-10-18

状态：已实施并完成本地隔离验证（2026-10-18）；Server1 管理服务已部署，存量节点迁移仍需后续显式操作。

## 目标与已确定的取舍

- 新增节点 Go 程序 `hysteriax-acme`，负责本地申请、续期、保存和发布服务端证书；管理服务器通过现有 SSH 通道配置和监控它。
- 覆盖全部 ACME 节点；已有节点仅在下一次显式部署、同步或回滚时迁移，不因管理服务升级自动修改远端。
- 保持原版 Hy2 v2.12.3，不维护 Hy2 补丁。每个节点签发一张包含全部配置域名的 SAN 证书；接受域名共同验证、续期以及不再分别选择证书的行为变化。
- 支持当前 HTTP-01、TLS-ALPN-01、DNS-01、全部 8 个 DNS 服务商及旧版配置字段。迁移和配置切换允许维护窗口，正常续期不重启 Hy2。

## 节点组件与证书生命周期

- 固定 CertMagic v0.25.4 及 Hy2 对应的 libdns 版本，提交 Go 模块锁定文件和依赖许可说明。
- 使用 `ACMEIssuer.PreCheck/Issue` 签发包含完整域名集合的 CSR；不使用会按域名单独签发的 `ManageSync`。由组件承担 SAN 证书续期调度、重试和发布。
- 沿用当前字段映射：CA 名称和别名、邮箱、监听地址、目录、三种验证类型、备用端口、旧版验证开关，以及所有 DNS 参数；保留 ZeroSSL 邮箱获取 EAB 的流程。未知 DNS 参数继续保留但不启用新语义。
- 使用 ECDSA P-256 私钥，每次签发生成新密钥。启动时检查证书，此后每小时检查；剩余有效期进入总有效期的最后三分之一时续期。失败从一分钟指数退避到六小时上限，加入抖动并遵守 CA 返回的等待要求，重启后保留重试进度。
- 提供 `check`、`ensure`、`run`、`status` 命令：分别检查配置、确保有效证书、持续维护和输出版本化 JSON 状态。配置文件传递秘密，命令行与日志不包含 DNS 凭据、私钥或 EAB。
- 签发后检查密钥匹配、完整域名覆盖、有效期和证书链，再写入不可变版本目录，通过原子替换 `current` 链接发布。发布失败保留旧证书；启动时修复未完成的发布。
- 组件使用独立 systemd 服务、受限账号和 `CAP_NET_BIND_SERVICE`。Hy2 仅获得已发布证书的读取权限；ACME 账户及 DNS 秘密不向 Hy2 开放。组件与部署操作使用同一节点本地文件锁协调写入。

## 配置、部署与恢复

- 公共配置继续保存 `acme` 和独立的 `mtls` 策略，保留用户输入的 `tls/acme` 互斥规则。部署编译阶段才把 `acme` 转换为托管的 `tls.cert/key`，随后注入生成的 `clientCA`；Hy2 最终配置不含 `acme` 或管理策略字段。
- 同步调整后端校验、预览、资源解析、探测及 macOS 表单，允许 ACME 选择托管 mTLS。公共接口继续禁止手写 `tls.clientCA`。
- 配置快照固定 ACME 策略和组件版本，证书路径按策略标识稳定生成。普通续期不增加节点修订、不修改 mTLS 激活记录、不触发用户证书重发或订阅刷新。
- 本地构建 Linux amd64、arm64 组件，将两种架构的产物及校验清单打入管理服务镜像；通过现有 SFTP 部署，节点无需安装 Go 或现场编译。
- 显式部署顺序：预检并暂存产物 → 保存恢复材料 → 停止旧 Hy2/证书组件 → 获取本地锁 → `ensure` 证书 → 切换配置和服务 → 执行现有代理及 mTLS 探测 → 确认部署。首次签发沿用现有 ACME 的 180 秒等待预算，超时取消并恢复。
- 失败恢复上一套组件、配置、证书发布指针和 systemd 服务；远端结果不明时沿用现有恢复状态，不提前发布订阅。回滚恢复目标策略并重新检查证书有效性，不强行恢复过期叶证书。
- 保留 `acme.dir` 的节点目录含义及现有默认解析行为；迁移前检查权限和目录归属，仅修改确认属于本项目的内容。保留可兼容的 CertMagic 账户状态；单域名有效证书可复用，多域名缺少完整 SAN 证书时重新签发。
- 切回手动 TLS 时停止证书组件并保留恢复材料；卸载时停止并移除组件，只清理本项目拥有的文件。数据库备份文档明确说明节点 ACME 账户与私钥需独立备份。
- 不自动修改节点防火墙或占用已有网站端口；端口冲突应给出明确错误。HTTP/TLS 备用端口仍要求公网 80/443 正确转发。

## 状态与客户端界面

- 增加节点 ACME 观测记录，在节点摘要及详情中提供可选的 `acme_status`：运行状态、域名、证书指纹、有效期、组件版本、最近成功时间、下一次尝试时间、脱敏错误及采集时间。
- 复用现有 SSH 采集周期读取状态，证书采集失败不影响流量统计；Hy2 停止或 mTLS 激活中也可采集组件状态。结果绑定实际部署策略，过期采样不得覆盖新部署状态。
- macOS 显示签发、续期、失败、到期和数据陈旧状态；沿用已有通知机制，增加续期失败及 30/14/7 天到期提醒并去重。
- 保持现有配置写入接口；恢复重试使用已有同步入口，不新增强制签发接口。更新 OpenAPI、生成模型及中英文文案。
- 多域名配置处明确提示共同签发和续期；管理服务离线期间节点继续维护证书，界面展示最后观测时间。

## 验证与交付

- Go 测试覆盖全部字段与 DNS 参数映射、旧版验证组合、ZeroSSL EAB、SAN CSR、证书校验、续期时间、退避、进程锁、崩溃恢复和日志脱敏；复用现有 DNS 服务商 fixtures。
- 使用隔离 ACME 测试 CA 验证 HTTP、TLS-ALPN、DNS、备用端口、多域名及失败重试，不依赖生产 CA。8 个真实 DNS 服务商完成映射测试，真实签发只使用明确配置的测试账户。
- 在 systemd 节点容器中验证：首次部署、已有 ACME 迁移、配置切换、超时恢复、重启续期、管理服务离线、手动 TLS 切换和完整卸载。
- 使用原版 Hy2 验证 SAN 各域名连接，以及 ACME+mTLS+ECH；轮换服务端证书时并发握手不得使用错误证书对，并确认已有连接无需重启。验证 mTLS CA 轮换、回滚和恢复流程不受影响。
- 运行相关 Rust、Go、API、订阅及 macOS 模型和界面检查。实际部署时本地构建镜像后推送服务器，遵循仓库部署要求。
- 实施完成后将实际验证结果补充到本计划，并更新凭据、节点维护和备份文档。


## 实施结果（2026-10-18）

- 新增 `components/hysteriax-acme`，提供 `check` / `ensure` / `run` / `status`，固定 CertMagic v0.25.4 和八家 libdns 对应版本，提交模块锁文件、MIT 许可及 27 个运行依赖的许可文本。
- 完整 SAN CSR、新 P-256 密钥、系统信任链及服务端用途校验、后三分之一续期、持久化退避和 CA Retry-After、ZeroSSL EAB 私有缓存已实现。发布使用不可变版本目录、原子链接与恢复日志。
- 新增受限账号、systemd 单元和共享锁；ensure 同样仅授予低端口绑定能力。已处理暂存目录 noexec、服务启动限频、取消后的恢复预算，以及已切换目标配置的激活恢复路径。
- 公共 ACME 与 mTLS 策略保持独立，部署和预览先编译证书路径再注入信任链；存量节点仅在显式操作时迁移。正常续期不重启 Hy2、不增加修订或触发用户证书/订阅发布。
- 迁移 0036 保存绑定部署修订、摘要和策略的观测；SSH 证书状态采集失败与流量统计隔离。API、OpenAPI、Swift 模型、表单、状态、双语和提醒去重已更新。
- 两种 Linux 架构产物、校验清单及许可包打入管理镜像并由 SFTP 下发。新增 CI、构建/许可更新/systemd 验收脚本，维护与独立备份文档已更新。

## 实际验证结果

- Rust 全套回归：**300 passed，0 failed，1 ignored**（既有性能基准）。all-targets 检查、Clippy `-D warnings`、rustfmt 和差异空白检查通过。
- Go race 测试 9 个顶层用例全部通过；`go vet` 通过。覆盖八家 DNS fixtures、旧开关、EAB、SAN、新密钥续期、取消、端口冲突、退避、Retry-After、锁、可信链和崩溃恢复。隔离 Pebble v2.10.1 实际完成 HTTP-01、TLS-ALPN-01、DNS-01、备用端口、多域名及 CA 拒绝后的持久化重试。
- Debian 13 原生 arm64 systemd 验收通过：首次经本地隔离 CA 实际签发 SAN；持续 pending 时达到 **175 秒取消期限**，恢复上一套证书策略和服务；通过手动 TLS 首装、兼容 SAN 缓存迁移、私有权限、激活恢复 ensure、组件独立重启、服务切换失败恢复和完整卸载。ensure 的有效和 ambient 能力均仅为低端口绑定（0x400）。
- 原版 Hy2 v2.12.3 两个 SAN 域名的 mTLS+ECH 转发通过；轮换发布时完成 24 次客户端握手（每轮 6 个并发）并保留已有连接，Hy2 InvocationID 未改变。切回手动 TLS 后组件停止且恢复材料保留，卸载保留项目外哨兵文件。
- macOS arm64 Xcode Debug 构建、ACME DNS 客户端、30/14/7 天和失败提醒去重、订阅 DTO、概览渲染、中英文、OpenAPI Redocly 和生成模型一致性检查通过。
- 新数据库迁移及单实例保护通过。镜像 `hysteriax-server:node-acme-0003-local` 已本地构建，镜像内两种架构产物和许可包校验通过；ELF 架构为 amd64 / arm64，组件版本 0.1.0。镜像在隔离数据库下 readiness 和迁移 0036 验证通过。

### 验证边界

- 未使用生产 CA 或真实 DNS 账号，未推送或部署生产镜像，未升级存量远端节点。真实 DNS 服务商完成字段映射测试；实际协议签发使用隔离 CA 和测试 DNS provider。
- 完整 systemd 验收运行于原生 arm64；amd64 完成交叉构建、校验与 ELF 检查，CI 在原生 amd64 runner 执行同一套验收。未替代云主机及公网防火墙/转发验收。


## Server1 部署记录（2026-10-18，Asia/Shanghai）

- 按用户部署指令，在本地构建 Linux amd64 管理镜像，并通过 SSH 传输至 Server1（hysteriax.cc），服务器未现场编译。
- 当前镜像：`hysteriax-server:node-acme-0003-20261018-01`。本地与服务器的全部 RootFS 层摘要及运行配置一致；服务器镜像 ID 为 `sha256:3cae57bdc132cc4ce1d09ffb79dd2871c523cb8b823bea96d9698c9ebddee1a1`。镜像内两种架构组件及许可包校验通过，amd64 组件执行版本 0.1.0。
- 旧镜像 `hysteriax-server:managed-mtls-0002-20261017093726` 保留。升级前暂停 API 写入，创建并验证 schema 0035 数据库备份，备份文件约 55 MiB，权限 0600：`/home/nieaowei/HysteriaX/backups/node-acme-0003-20261018-01/postgresql-before.tar.gz`。同目录保存受限的原环境配置、节点状态快照及部署回执。
- 管理服务已升级至 schema 0036；Docker health 为 healthy，重启次数 0；本机 HTTPS 与经 SSH 转发的 HTTPS `/readyz` 检查通过。
- 6 个节点的目标/部署配置、修订、摘要、mTLS 代次与状态均保持不变，5 个 ACME 节点的新证书路径预览通过。仅失效相关缓存代次，未自动部署或迁移远端节点。
- 部署时补充下载重试，并让 Go 构建阶段使用本机构建平台交叉生成两种架构产物，避免为交叉构建下载和模拟执行另一架构的 Go 工具链。


## Server1 部署记录（2026-10-18 二次升级，Asia/Shanghai）

- 按用户部署指令，将工作区当时未提交的订阅兼容性修复本地构建为 Linux amd64 镜像 `hysteriax-server:node-acme-0003-20261018-02`，经 SSH 以 zstd 流传输并加载，服务器未现场编译。修复内容：通用下载器（UA 未知）在内核版本未知时不再过滤 mTLS/ECH/Realm 节点，订阅缓存键升级为 `subscription:v2-unknown-kernel`。
- 本地验证：`cargo test -p hysteriax-server formats` 10 项通过（含新增 `unknown_kernel_exports_advanced_nodes_with_all_required_material`）；镜像内 `/usr/local/bin/hysteriax-server` SHA-256 为 `cb4f8395c89434dff828ab393ed70d828306cd421619b3ff8dab252905fd7632`，与上次构建镜像内二进制一致（源码自 06:20 后未再变化），amd64 组件 SHA-256 为 `ec2b04357d4218e834615b5e04c9779e38065358d0065266694291e989d01f9f`。
- 升级前用 `python3 scripts/backup-restore.py backup` 生成经验证的数据库备份：schema 36，57 238 947 字节，master-key 指纹 `8b0a578d2cb57086da171c3fc21cf597c240d35ceed1726cee4c02e4e2f050b9`，路径 `/home/nieaowei/HysteriaX/backups/node-acme-0003-20261018-02/hysteriax-postgresql-before.tar.gz`；同目录保存升级前 `.env`（env.before）与 `receipt.json`。
- 切换 `HYSTERIAX_VERSION` 后 `docker compose up -d api` 重建容器：服务器镜像 ID `sha256:9d38dbda96689cf93e6843ce18be25796c7e48c89b482de7fa11c347fa5fa8eb`，health 为 healthy，容器内 `/readyz` 返回 ready，schema 保持 36，重启次数 0，API 日志无错误。旧镜像 `node-acme-0003-20261018-01` 保留用于回退。
- 升级后实测：以 `curl/8.0`（通用 UA）请求 sing-box 订阅返回 `x-hysteriax-filtered-nodes: 0`，6 个节点全部返回，mTLS 节点带 `client_certificate`（13 行）与 `client_key`（5 行）；mTLS 仍强制生效（无客户端证书被拒 `tls: certificate required`），携带订阅证书可连接并经该节点出网；4 个存量 ACME 节点证书与首尔节点 `acme_status=ready`、`mtls_state=ready` 均无变化。
- 本次镜像按部署当时的工作区状态构建，上述改动与节点 ACME 组件一样仍未提交到 Git。


## sing-box mTLS 修复部署记录（2026-10-18 三次升级，Asia/Shanghai）

- 用户报告 OpenWrt 的 momo 插件（内核为 sing-box，订阅 UA 默认 `sing-box`）无法使用首尔 mTLS 节点，其他节点正常。
- 复现与定位：用真实 sing-box 1.14.2 加订阅内实际 outbound 连接节点，报 `quic: server-side tls.Config fields are not supported with ChromeParrot`。sing-box 1.14 起对 Hysteria2 默认启用 ChromeParrot（Chrome 风格 QUIC ClientHello），其 `sagernet/quic-go` 的该路径在 `utlsConfigFromStd` 中拒绝任何携带 `tls.Config.Certificates`（客户端证书）的配置；节点无客户端证书时不受影响，与“只有 mTLS 节点不可用”一致。
- 修复：`SubscriptionNode::singbox()` 在节点带有客户端证书时额外输出 `"disable_chrome_parrot": true`（该选项自 sing-box 1.14.0 存在，与模板 `dns.optimistic` 的下限一致），无证书节点保持默认 ChromeParrot 握手。新增单测 `singbox_keeps_chrome_parrot_without_client_certificate` 并扩展 `singbox_maps_fields_without_conflicting_endpoint_fields`；`cargo test -p hysteriax-server formats` 11 项通过。
- 端到端验证：本地经 UDP-over-TCP 中继（本机 → SSH → 管理服务器 → 首尔节点，本机网络劫持全部 UDP）运行 sing-box 1.14.2 与服务器实际下发的配置，SOCKS 出网 `ip=158.247.242.138 loc=KR`；未加该开关时同一配置必然失败。
- 镜像 `hysteriax-server:singbox-mtls-chrome-parrot-20261018` 本地构建（amd64），二进制 SHA-256 `17157ffadfbdcda0ecbf7d1ae383bfbd166c22e6f72720a7b56cced4b89bbeef`，zstd 流经 SSH 传输加载。升级前备份：schema 36，57 574 313 字节，路径 `backups/singbox-mtls-chrome-parrot-20261018/hysteriax-postgresql-before.tar.gz`；服务器镜像 ID `sha256:c9bb4156e340c8da4b2f013fabef8440516a346b61a29e578fe46fa3a614c687`，healthy、`/readyz` ready。上一版镜像 `node-acme-0003-20261018-02` 保留回退。
- 附带发现（未改）：sing-box 模板使用 `dns.optimistic`，1.13.x 及更早版本会因未知字段拒绝整个配置（实测 1.12.22 / 1.13.0 / 1.13.21 均 FATAL，1.14.0+ 通过），即当前 sing-box 订阅实际要求 ≥1.14.0；已记入 `docs/subscriptions.md`。OpenWrt 官方源 24.10 为 sing-box 1.12.22、25.12.5 为 1.13.21，均需用户自行升级内核。


## sing-box 1.14 字段门控部署记录（2026-10-18 四次升级，Asia/Shanghai）

- 按用户指令，为 `dns.optimistic` 与 `disable_chrome_parrot` 增加版本门控：请求标识为**已知** sing-box < 1.14.0 时两者都不输出（1.13 没有 ChromeParrot，也不需要该开关）；**版本未知**（浏览器、通用下载器、管理端导出）保留全部字段。
- 代码：新增 `ClientInfo::supports_singbox_1_14_fields()`；`render_singbox_template` 接收 `client` 并按需删除模板的 `dns.optimistic`；`SubscriptionNode::singbox(chrome_parrot)` 按需输出开关。新增测试 `known_singbox_before_1_14_omits_1_14_only_fields`（覆盖 1.13.0 / 1.13.21 / 1.14.0 / 1.14.3 / 无版本 `sing-box` / `curl/8.0` / 空 UA）。`cargo test -p hysteriax-server formats` 12 项通过，`clippy -D warnings` 与 `rustfmt --check` 通过。
- 镜像 `hysteriax-server:singbox-version-gate-20261018`，二进制 SHA-256 `7af2463084c673877090b8ff4d0955a95b4062d8160099db388c79c8f533712c`；升级前备份 `backups/singbox-version-gate-20261018/hysteriax-postgresql-before.tar.gz`；服务器镜像 ID `sha256:599b5deb26c5507b2e20d6a618c3673bce34f661b1f3836b8950fc693dc1fa42`，healthy、`/readyz` ready。上一版 `singbox-mtls-chrome-parrot-20261018` 保留回退。
- 线上实测：UA `sing-box/1.13.21`、`1.13.0` 下发的配置无 `dns.optimistic`、无 `disable_chrome_parrot`，mTLS 节点及客户端证书仍正常包含；`1.14.0`、`1.14.3`、无版本 `sing-box`、`curl/8.0`、空 UA 均保留两个字段。
- 边界（已记入 docs/subscriptions.md）：该门控只摘掉两个字段，模板仍使用其他 1.14 构造（`services` 的 `api` 类型、`http_clients`、`route.default_http_client`、`rule_set` tag 数组、DNS `evaluate` 与 `match_response`、`cache_file.store_dns`）。用 1.13.21 实测仍拒绝整份文档，共需 8 处改动（含 2 处结构性：多 tag 的 rule_set 需拆分；`services` 需回落 `experimental.clash_api`）。因此 sing-box 订阅实际最低版本仍为 1.14.0；完整支持 1.13 需另做 legacy 模板分支，本轮按用户决定暂缓。
