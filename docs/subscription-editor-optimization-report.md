# 订阅编辑器完整优化与核查报告

日期：2026-10-20。实施编号：0023。

默认值后续专项补充见 [0024 默认值核查报告](audits/0024-default-values.md)：修复此前未覆盖的默认值、显式设置入口及动态默认值显示。

后续交互修复包括：可批量编辑的标签组件统一默认使用标签模式；路由与 DNS 规则增加独立的 `action` 下拉选择，详见第 10 节。

重复渲染专项后续核查见 [0025 重复渲染核查](audits/0025-duplicate-rendering.md)：补充修复 ACME 供应商、Snell 固定版本、内联对象判别字段，以及折叠对象概览与详情的重复类型入口。

TLS 独立编辑弹窗的具体路径、例外和触发原因见 [0026 TLS 弹窗定位](audits/0026-tls-editor-popups.md)，其中也包含两种公钥指纹数组进入集合弹窗的情况。

TLS 主对象的后续内联实施与不同位置 `enabled` 默认值见 [0027 TLS 主对象内联与默认值核查](audits/0027-tls-inline-enabled-defaults.md)：覆盖 34 个主对象位置、自定义 HTTP 客户端引用和五个子功能开关。

仍使用整数／字符串切换的字段见 [0028 类型切换定位](audits/0028-integer-string-switches.md)：区分直接字段、DNS 列表项目、已有统一输入及只读代理 Schema。

后续已按用户规则统一类型，见 [0029 固定编辑类型](audits/0029-fixed-scalar-editor-types.md)：纯整数／字符串联合中，`mark` 后缀固定字符串，其余固定整数；`query_type` 顶层保留整数／数组入口。

本轮在工作区已有的 0019–0022 文档驱动编辑器改动基础上，完成了固定版本全配置文档的逐项对应核查、在线中文文档的全导航差异核查，以及一批条件显示、默认值和文档定位修复。报告把“Schema 有字段”“表单交互已回归”“原生内核运行通过”分开记录，避免以字段数量代替正确性。

## 1. 核查依据与完整性

用户指定入口：[sing-box 中文配置文档](https://sing-box.sagernet.org/zh/configuration/)。在线站点滚动更新，项目服务端字段目录和验证基线固定为 **sing-box 1.14.2**。

| 项目 | 核查结果 |
| --- | --- |
| 上游固定提交 | `af6e64c3b69e6132ebaee0e1a3d24e93903f6709`，对应 v1.14.2 |
| Schema 来源 | 上游 `docs/schema.json`，与仓库文件逐字节一致 |
| Schema SHA-256 | `15133dc518142b7082f120fa7ed08dabb5b28768850e4bebcde67272cbd58d47` |
| 英文配置文档树 SHA-256 | `550f8358d23bd270a3806912a2fdec06b85753be1dcf61875318ad4151fa9933`，按排序路径、NUL、内容、NUL 连续计算 |
| 固定版本页面 | 121 页全部进入清单，其中 111 页有可提取字段或动作标题 |
| 逐项条目 | 1,173 个**页内去重**字段或动作标题；不是 1,173 个独立配置属性 |
| 根配置 | 14 项，全部纳入检查 |
| Schema 定义 | 93 个 `$defs`；通用模型递归核查类型和引用 |
| 在线中文配置导航 | 125 页全部读取，固定版本 121 页均仍在导航中 |
| 在线差异 | 4 个新增页面，22 页存在标题集合差异；共 65 个新增标题、6 个未出现标题，包含翻译和排版差异，不全是新增功能 |

固定来源：[v1.14.2 配置文档](https://github.com/SagerNet/sing-box/tree/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration)。生成工具同时验证 Schema 与文档树摘要，防止用滚动文档或不完整下载生成版本化元数据；中文副本不会被重复作为配置页面。

完整材料：

- [逐字段阅读版附录](audits/0023-singbox-documentation-fields.md)：121 页逐页列出字段、核查状态及原因。
- [逐字段机器清单](audits/0023-singbox-documentation-inventory.json)：包含来源、页面哈希、Schema 指针与结果；全部指针都在生成时验证可解析。
- [在线文档全页差异快照](audits/0023-singbox-website-delta.json)：125 页的 URL、页面哈希、字段标题及相对固定版本的差异。
- [验证记录](audits/0023-verification.md)：命令、结果及验证边界。

## 2. 每一项的归类结果

| 结果 | 数量 | 实际含义 |
| --- | ---: | --- |
| `schema` | 901 | 同版本 Schema 在相应对象中有对应属性，包括已识别的嵌套标题。具体语义和视觉验收仍以相应测试为准。 |
| `policy` | 160 | 手工出站受产品策略限制：只开放 direct、selector、urltest；Hysteria2 连接参数来自已授权节点注入。不是编辑器漏掉 160 个可自由编辑的字段。 |
| `omitted` | 41 | 文档仍保留的废弃、已移除或 `schema:omit` 字段，不重新包装成已受验证的配置项。 |
| `historic` | 32 | Legacy DNS/FakeIP、GeoIP/Geosite、旧 DNS/WireGuard 出站等迁移文档。 |
| `migration` | 13 | TLS 旧内联 ACME 段落；新配置使用证书提供者。 |
| `reference` | 12 | 协议探测、Pre-match、Wi-Fi/邻居解析、独立规则集文件等说明性标题。 |
| `discriminator` | 11 | route、reject、gRPC 等动作或类型标题，不是独立属性。 |
| `upstream_gap` | 3 | 文档与同提交 Schema 的明确差异，见第 6 节。 |
| 未归类 | **0** | 不留没有解释的字段对应差异；这不等于运行时没有未验证项。 |

`omitted` 中包括旧 sniff、旧地址拆分、旧 DNS 策略/缓存项、规则集 download_detour、Hysteria 旧 QUIC 参数和旧 direct 地址覆盖。编辑器继续保留导入文档中的未知字段，可在源码中查看；不自动迁移或删除。

## 3. 根配置逐项结论

| 根项 | 核查与优化结果 | 验证范围 |
| --- | --- | --- |
| `$schema` | 对应 Schema 文档；保持 JSON Schema 声明与版本基线分离。 | 目录映射、类型与引用 |
| `log` | disabled 决定后续配置显示；未把示例中的日志路径、时间戳当作默认值。 | 模型与构建 |
| `dns` | 检查 15 种 DNS 服务器、规则/动作、缓存与 optimistic；TLS 类型自动启用 TLS，默认端口与 `/dns-query` 按上下文预览；补齐查询超时 10s。 | 模型、类型切换、默认值不落盘 |
| `ntp` | enabled 控制服务参数；端口 123、同步间隔 30m 只作默认预览；拨号字段保留共同顺序。 | 模型、根字段分类 |
| `certificate` | 系统证书库、内联证书及路径/目录使用 Schema 控件，引用证书文档。 | 字段对应与类型 |
| `certificate_providers` | ACME、Tailscale、Cloudflare Origin CA；账户、签发、挑战和存储分组；DNS01 与 HTTP/TLS-ALPN 挑战依赖；共享引用与内联提供者可区分。 | 模型与 SwiftUI 类型/开关回归 |
| `http_clients` | 共享与内联表单共用；HTTP 版本、Go/Apple 引擎、HTTP/2/QUIC 和 TLS 分组；不兼容已有配置可查看并撤销清理。 | 模型、共享/内联 SwiftUI 回归 |
| `network_namespaces` | default/unshare 两种类型；默认命名空间保留必填路径提示，Linux 平台限制沿用平台规则。 | 变体与字段完整性 |
| `endpoints` | WireGuard、Tailscale、OpenConnect、OpenVPN Client/Server；接口、认证、密钥、路由及模式依赖，详见下一节。 | 变体、默认值及重点交互 |
| `inbounds` | 20 个 Schema 分支（Snell 版本分别计数）；协议与监听字段分清归属；TUN 路由/DNS/MAC 依赖、Cloudflared 协议依赖；不把监听 detour 当拨号 detour。 | 全分支排序完整性、重点 SwiftUI 回归 |
| `outbounds` | 原始 Schema 21 个分支用于模型核查；实际目录保留 3 类手工出站，动态节点独立注入。代理组顺序和默认目标保留。 | 模型完整性、切换与撤销 |
| `route` | 匹配条件与动作分组、嵌套逻辑规则、规则集来源与下载设置；动作字段按选中的动作 Schema 归属；远程规则集更新间隔 1d 默认预览。 | 模型、规则集切换与文档定位 |
| `services` | 11 个 Schema 分支，含 USB/IP server 的 provider 分支；API、DERP、Resolved、SSM 等用通用表单；USB/IP client 只显示有效 detour 拨号字段。 | 变体、依赖和未文档化分支回退 |
| `experimental` | cache_file、Clash API、V2Ray API；功能开关、Clash UI 下载设置、缓存路径 cache.db 默认预览；debug 沿用 Schema 通用编辑。 | 模型、内联对象、字段对应 |

## 4. 协议与共享配置核查

### DNS、规则与规则集

检查 DNS 服务器的 server/endpoint/service 引用、传输种类、TLS 使用方式和端口默认值，区分 HTTP/2 的 idle_timeout 与 URLTest 自身的 idle_timeout。DNS 规则包含匹配、响应匹配及动作联合类型；路由规则包含 8 类动作，DNS 动作包含 6 类。

动作、域名/规则集、IP、端口、进程/用户分组沿用工作区已有优化。规则动作的 override_port 等参数通过当前动作 Schema 判断，避免误归入匹配条件；network_strategy 的说明不再套用拨号专用限制。逻辑规则的子项保留有序数组、删除与撤销语义。

规则集检查 inline/local/remote 三类：format 在可从 `.json`/`.srs` 路径推断时不虚标必填；下载设置支持共享或内联 HTTP Client。远程规则集 update_interval 缺省显示 1d，浏览不写入草稿。DNS evaluate/respond 的顺序关系、race/speculative 的运行时竞争和复杂规则匹配逻辑仍需原生内核场景验证，未以 Schema 检查代替。

### TUN、监听与拨号

TUN 已按接口、DNS、路由接管、自动重定向、流量范围、UDP NAT、系统代理分组。新增修复：include_mac_address/exclude_mac_address 需要 **auto_route 与 auto_redirect 同时开启**；仅 auto_route 开启时不再误显示。dns_mode=disabled 隐藏 dns_address；路由与重定向的其他依赖保留。

监听 detour 转发到入站，拨号 detour 选择出站，二者不会互相套用“其他参数忽略”的提示。默认接口自动探测、显式接口、fallback 网络策略、TCP 保活等条件继续由上下文决定；隐藏保留已有值。

TUN 平台权限、nftables、Android VPN 与 Apple 图形客户端约束，以及绕过环路的接口配置，沿用平台提示和原生文档，不宣称已在所有系统实机验证。

### TLS、HTTP Client、HTTP/2 和 QUIC

TLS 检查基础握手、ALPN/版本、服务器证书、客户端身份、ECH/uTLS/Reality、分片/伪装和 kTLS。HTTP Client 的 TLS 字段覆盖包含 Schema 兜底，避免静态白名单漏字段；共享与内联对象使用同一规则。

新增把系统 TLS 引擎支持范围用于**通用 OutboundTLSOptions 表单**：apple/windows 只展示其支持的参数，已有的客户端私钥等不支持字段保留并列出名称。HTTP Client 的 Apple 引擎与 tls.engine 是不同层级，不混为一项。未把 1.15 certificate_sha256/client_certificate_sha256 加进 1.14.2 表单。

QUIC 复用的接收窗口等 HTTP/2 字段依照该固定版本文档和现有实现保留；HTTP/3 额外展示 QUIC 参数。窗口编辑保持容量字符串、整数和 UInt64 的原始值。OpenConnect 未设置保活时保留系统时序，不再显示通用拨号的 5m 假定值。

### WireGuard、Tailscale、OpenConnect、OpenVPN

| 对象 | 核查与改进 |
| --- | --- |
| WireGuard | 接口/对端分组；对端地址、公钥、允许网段和 keepalive 独立编辑；MTU 1408 文字默认值只作预览，CPU workers 等动态值不固定化。 |
| Tailscale | 登录、身份、出口节点、路由、中继、系统接口、SSH 和状态目录；按系统接口开关等条件显示；不把系统主机名、动态监听端口当作固定默认值。 |
| OpenConnect | 认证、token、客户端身份、CSD/HIP/TNCC、证书和会话；修正保活默认值覆盖；flavor、主机检查与证书成对配置的完整运行时语义没有冒充已做实机认证。 |
| OpenVPN | 新增 tls/static_key 模式依赖：静态密钥模式隐藏 TLS、用户名/密码、拉取/推送、TLS 重协商；TLS 模式隐藏 static_key/static_key_path/key_direction/cipher。控制信道 direction 仅 tls_auth，force_cookie 仅 tls_crypt_v2；push.redirect_gateway_flags 随开关显示。已有值保持并支持撤销重做。 |

### 入站协议、传输与服务

AnyTLS、Cloudflared、Direct、HTTP/Mixed/SOCKS、Hysteria/Hysteria2、Naive、Redirect/TProxy、Shadowsocks/ShadowTLS、Snell、Trojan、TUIC、VLESS、VMess 均进入文档与 Schema 对应检查。Snell 嵌套联合分支按版本区分；接收窗口沿用共同控件。Cloudflared 的 datagram_version 仅在 protocol=quic 时显示。

V2Ray 的 HTTP、WebSocket、QUIC、gRPC、HTTPUpgrade 五个分支使用通用类型切换；重复标题不再猜测页内锚点。多路复用的连接数/流数互斥与开关依赖保留；UDP over TCP 补齐独立共享文档链接，**缺省版本改为正确的 2，而非 enum 第一项 1**，用户显式设置的 1 仍保留。

服务对象检查 API dashboard 的多种表示、DERP/STUN、SSM 入站引用、CCM/OCM 和 USB/IP。USB/IP client 文档明确只有 detour 有效，因此不继续展示无效的 bind_interface/connect_timeout 等控件；若已有值，列出被隐藏字段名。oom-killer、experimental.debug 等有 Schema 而无配置导航专页的部分回退分类入口和通用控件，不伪造专属说明。

## 5. 本次新增修复及验证点

| 问题 | 修复 | 回归证据 |
| --- | --- | --- |
| 合并标题被漏提取 | up_mbps/down_mbps 均入目录，指向共同标题 | 提取器合并/重复标题回归与模型 URL 断言 |
| 重复标题可能跳到错误方向 | 记录锚点覆盖，歧义字段回退整页；TLS 保持整页 | 生成元数据校验、重复标题断言 |
| 中英文源树一起输入可能重复生成页面 | 只索引规范英文标识，严格验证完整树摘要 | 完整源树重生成及 `--check` |
| UoT 对象默认显示协议 1 | 版本 2 只读预览，显式 1 优先 | 默认预览不落盘、显式编辑与撤销 |
| TUN MAC 过滤缺少依赖 | 同时要求 auto_route/auto_redirect | 双开关模型回归 |
| OpenVPN 模式展示无效字段 | 按会话模式、控制包装类型和推送开关显示 | 模型与真实 SwiftUI 模式切换/撤销 |
| 系统 TLS 仍展示不支持的 Go 参数 | 通用表单与 HTTP Client 共用支持范围 | 引擎切换、隐藏值保留回归 |
| 隐藏字段存在性不可见 | 独立编辑区与内联对象显示保留字段名说明 | 模型隐藏集合与 SwiftUI 渲染；不展示凭据内容 |
| USB/IP 显示无效拨号参数 | 只保留 detour | 字段依赖模型回归 |
| OpenConnect 保活预览误用通用默认 | 不再覆盖其系统缺省时序 | 专项模型断言 |
| 默认值帮助反复展开无关 Schema | 跳过无关类型展开、只在确需时解析帮助上下文 | 模型、原生表单交互与构建回归；未声称量化提速 |

其他有明确文字依据的默认值预览包括 DNS timeout=10s、规则集 update_interval=1d、缓存文件 cache.db、WireGuard mtu=1408。示例值、平台动态值和依赖编译标签的默认值不自动物化。

## 6. 上游差异与版本边界

### 同一固定提交的 3 项差异

| 文档项 | 同版本 Schema/源码 | 本轮处理 |
| --- | --- | --- |
| FakeIP `inet6_address` 标题 | Schema 与选项源码为 `inet6_range` | 表单遵循 Schema；逐项清单明确标注，在线站点已改为 inet6_range。 |
| OpenVPN server `users.username` | `$defs.User` 使用 `Username` | 保留上游 Schema，不擅自改校验器；大小写差异单列。 |
| OpenVPN server `users.password` | `$defs.User` 使用 `Password` | 同上；源码导入的小写值不能被当作“受固定 Schema 校验支持”保证。 |

旧 ECH pq_signature_schemes_enabled/dynamic_record_sizing_disabled、旧 ACME 等文本仍存在，但固定 Schema 已省略或采用新结构。它们进入 omitted/migration，而非未解释缺失。

### 在线站点的完整标题差异

下表保留所有 22 页差异；“新增标题”仅指相对固定英文版本提取结果的变化，不能直接等同于版本新增属性。逐页完整集合及哈希保存在在线快照中。

| 页面 | 新出现标题 | 未出现标题/说明 |
| --- | --- | --- |
| dns | fakeip | 历史配置标题差异 |
| dns/rule | dns_search_domain、dns_server_address | 需要随新基线评估 |
| dns/server/fakeip | inet6_range | inet6_address；文档命名修正 |
| endpoint/masque-client | advertise_routes、disable_version_fallback、headers、mtu、name、on_demand、password、path、server、server_port、system、tls、username、version | 新增页面，标注自 1.15.0 起 |
| endpoint/masque-server | address、advertise_routes、mtu、name、path、system、tls、users、version | 新增页面 |
| endpoint/openconnect | on_demand | 新基线字段 |
| endpoint/openvpn-client | on_demand | 新基线字段 |
| endpoint/tailscale | on_demand | 新基线字段 |
| endpoint/wireguard | on_demand | 新基线字段 |
| experimental/cache-file | buffer_size、flush_interval | 新基线字段 |
| inbound/http | version | 需结合类型与版本判断 |
| inbound/tailcat | derp_map_url、derp_region、derp_servers、http_client、pre_shared_key、private_key、users、users.public_key | 新增页面 |
| inbound/tun | auto_redirect_tproxy_mark、multi_queue | 新基线字段 |
| outbound/http | disable_version_fallback、version | 手工出站仍受产品限制 |
| outbound/tailcat | derp_map_url、derp_region、derp_servers、http_client、pre_shared_key、private_key、server_disco_key、server_public_key | 新增页面，且受出站策略限制 |
| outbound/tuic | heartbeat、zero_rtt_handshake | 需结合版本和产品范围判断 |
| outbound/wireguard | 无 | peers、peers.allowed_ips、peers.reserved；旧出站迁移页差异 |
| route | network_strategy、rule | default_network_strategy、rules；中文标题与固定 Schema 不一致，不据此改字段名 |
| route/rule | dns_search_domain、dns_server_address | 需要随新基线评估 |
| service/derp | verify_client_inbound、verify_client_key | 需要随新基线评估 |
| shared/neighbor | macOS | 平台说明标题，不是配置属性 |
| shared/tls | certificate_sha256、client_certificate_sha256 | 在线文档标注 1.15.0；不加入当前固定表单 |

## 7. 数据保护与可维护性

- 查看帮助、默认值、文档或表单不写入草稿。已设置值优先于预览值，显式 false、0、null 与缺失仍由原有状态模型区分。
- 条件隐藏只改变显示；保留的字段名可查看，相关值不出现在说明摘要中。切换类型仍沿用已有“保留共同字段和未知扩展、清理旧类型专用字段”的行为，不混淆为条件隐藏。
- 源码仍是未知字段的完整编辑入口；未把未来字段标成已经验证。
- 新增字段目录由脚本产生，可从不可变上游提交重现；审计清单没有数据库、服务器凭据或用户配置。
- 普通字段继续使用 Schema 通用控件；更细的模式专用交互逐步增加，避免每个协议各自维护互相偏离的字段白名单。

## 8. 验证结果与未覆盖场景

已执行的命令和最终状态见 [验证记录](audits/0023-verification.md)。覆盖模型类型/引用/默认值、全部协议变体排序不丢字段、主要 SwiftUI 开关和类型变化、撤销重做、复杂集合、大列表、中英文资源及 macOS Debug 构建。另执行现有生产验证器的 1,046 项冻结语料对比，确认此次客户端优化未改变服务端校验结果。

尚未进行每个协议在每个平台上的联网互通、企业 OpenConnect/OpenVPN 真实认证、ACME 实际签发、TUN/nftables/USB-IP 权限与驱动配置、不同系统版本 TLS 握手、所有 DNS race/evaluate/respond 顺序组合。这些不是 Schema 或离屏视图能够证明的；完整清单和编译通过不能替代这些验证。

本次没有部署服务，也没有把 sing-box 验证基线升级到 1.15。后续若升级，应同时更新 Schema、校验器冻结语料、平台矩阵、原生内核校验和文档元数据，再开放新增表单字段。

## 9. 复现方式

取得上游固定提交到 `SOURCE` 后运行：

```sh
python3 scripts/generate-template-documentation-order.py SOURCE/docs/configuration --check
python3 scripts/audit-template-documentation.py SOURCE/docs/configuration --write
python3 scripts/audit-template-website.py --as-of 2026-10-20 --write
sh scripts/verify-subscription-client.sh
sh scripts/verify-template-picker-ui.sh
sh scripts/verify-template-collection-ui.sh
bash scripts/verify-localization.sh
python3 scripts/verify-subscription-validator-compatibility.py
```

在线快照反映抓取时网站内容；再次执行可能产生差异。固定版本清单则由完整文档树与 Schema 摘要限定，可复现。

## 10. 后续标签与规则动作修复

可批量编辑的标签组件统一默认使用标签模式，包括正则、URL、长文本及大列表。仍可手动切换批量编辑；输入和粘贴后不会自动切换模式。

规则的 `action` 原先随 Schema 当前动作分支作为常量显示，缺少独立选择入口。此前单独动作 Schema 的回归未覆盖实际规则中“匹配类型 + 动作”两层组合的交互。现已为路由和 DNS 的普通规则、逻辑规则提供独立动作下拉框，切换后刷新对应参数表单。

另修复普通规则中 `type` 的重复显示：顶部配置类型切换入口已经控制该字段，下方不再重复渲染它的枚举控件。此项同时适用于路由与 DNS 规则；逻辑规则仍使用同一个顶部入口切换。

- 路由动作：`route`、`route-options`、`direct`、`bypass`、`reject`、`hijack-dns`、`sniff`、`resolve`。
- DNS 动作：`route`、`evaluate`、`respond`、`route-options`、`reject`、`predefined`。
- 切换保留匹配条件、逻辑子规则、共用参数和未知扩展；清理旧动作独有且新动作不支持的参数。特别保护同时属于匹配条件和 `direct` 动作参数的 `network_type`。省略或空字符串动作按默认 `route` 处理，打开表单不会补写动作。
- 未知动作保留原名称和参数，可由用户选择已知动作。仅含匹配条件的嵌套规则与规则集规则不增加动作选择。

模型回归覆盖两种规则结构下共 200 个动作转换组合，另检查默认动作、未知动作和无动作的匹配规则。原生界面回归使用完整 `Rule` / `DNSRule` Schema，检查普通规则和逻辑规则切换后的渲染、条件保留与撤销重做；不将模型驱动渲染视为真实鼠标点击验证。

验证结果：`sh scripts/verify-subscription-client.sh`、`sh scripts/verify-template-picker-ui.sh`、macOS Debug 构建和 `git diff --check` 均通过。

DNS 规则编辑页的“规则动作”移至“域名与规则集匹配”之前，其他编辑页顺序保持不变。

配置对象及动态节点注入的独立编辑弹窗，在“完成”左侧统一增加“取消”。取消或按 Esc 恢复打开弹窗时的本地文档、撤销/重做历史及错误状态；完成保留修改。嵌套弹窗各自保存检查点，取消内层后保留外层此前的修改。取消回归覆盖嵌套编辑、超过撤销历史上限及恢复原有重做分支。

下拉枚举的“未设置”重复来自两处：程序补充的未设置占位项与 Schema 中的空字符串同时渲染；已知默认值又与其空字符串别名同时渲染。现统一通过选项模型生成菜单：没有固定默认值时，只保留一个未设置入口；拒绝方式 `method`、`udp_mapping`、`udp_filtering`、`wildcard_sni` 的空字符串与已核查的非空默认值合并显示。打开编辑器不会改写已有空字符串、未知枚举或显式 null，只有用户选择新值才写入。数组元素不加入删除字段意义的未设置入口，其他显式空字符串明确标注“空字符串”。

`network_strategy` 的未设置与 `default` 不合并：前者可继承路由的全局网络策略，后者显式选择按顺序连接。菜单分别显示“未设置（继承默认）”及“按顺序连接（default）”。参考：[路由全局策略](https://sing-box.sagernet.org/zh/configuration/route/)、[拨号策略](https://sing-box.sagernet.org/zh/configuration/shared/dial/)、[拒绝方式](https://sing-box.sagernet.org/zh/configuration/route/rule_action/#method)、[UDP NAT 默认值](https://sing-box.sagernet.org/zh/configuration/shared/udp-nat/)。

枚举回归遍历 sing-box 229 处及 Mihomo 12 处枚举声明，其中分别有 24 处及 1 处包含空字符串；覆盖缺失值、现有各枚举值、未知值和显式 null，检查每个选择值只对应一个菜单项。原生回归使用真实字段渲染及共享选项模型验证菜单内容、保留原文档与撤销；SwiftUI 菜单未暴露 NSPopUpButton 项，未将这些检查记为真实菜单点击验证。

本次下拉修复验证：客户端模型回归、完整原生界面回归、1940 条双语资源检查、macOS Debug 构建及 `git diff --check` 均通过。

## 11. 对象与引用的级联变更

对象列表内修改名称或标签时，自动同步已识别的引用、默认目标和动态节点注入组，并检查重名；同名域名、密码及未知字段不会被替换。名称和引用更新放在同一撤销事务中。

根据用户选择，删除被引用对象时先展示级联删除预览，确认后删除依赖规则并继续处理后续依赖、组成员、默认引用和注入规则。逻辑规则按完整外层规则删除，避免移除条件导致匹配范围变宽。预览包含操作前路径及完整差异，取消不写入，过期预览不能提交，整次操作可一次撤销。

覆盖范围、具体删除策略和验证记录见 [0030 级联变更报告](audits/0030-cascade-reference-changes.md)；实施计划见 [0030 计划](plans/0030-subscription-editor-cascade-changes.md)。
