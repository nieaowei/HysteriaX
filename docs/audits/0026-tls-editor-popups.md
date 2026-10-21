# 0026 TLS 独立编辑弹窗定位

日期：2026-10-20。核查对象：当前 macOS 订阅编辑器与固定 sing-box 1.14.2 Schema。本项记录当前交互路径，供后续统一内联编辑使用。

后续实施见 [0027 TLS 主对象内联与默认值核查](0027-tls-inline-enabled-defaults.md)：TLS 主对象已移除该层弹窗，并按开关或协议自动启用规则显示参数。下方清单保留定位时状态；证书提供者、握手、控制通道封装和指纹集合等子编辑入口另行区分。

## 会打开编辑弹窗的字段

`*` 表示集合索引或字典键。对象字段点击“编辑…”打开 `TemplateStructuredFieldEditor` 的 SwiftUI sheet；集合字段点击项目编辑按钮进入该项目的 sheet。字段是否出现、是否允许编辑，还受 TLS 开关、协议模式、引擎与平台能力控制。

| 路径或入口 | 触发条件与表现 |
| --- | --- |
| `inbounds[*].tls` | 带 TLS 的入站使用通用对象入口，TLS 主对象弹窗编辑 |
| `outbounds[*].tls` | 带 TLS 的出站使用同一对象入口；连接参数仍受产品可编辑范围限制 |
| `dns.servers[*].tls` | `tls`、`https`、`quic`、`h3` 类型服务器的 TLS 主对象弹窗编辑 |
| `endpoints[*].tls` | `openconnect`、`openvpn-client`、`openvpn-server` 的 TLS 主对象弹窗编辑；OpenVPN 静态密钥模式下该项隐藏 |
| `services[*].tls` | `api`、`ccm`、`derp`、`hysteria-realm`、`ocm`、`ssm-api` 的 TLS 主对象弹窗编辑 |
| `…tls.certificate_provider` | 选择内嵌提供者对象时弹窗；共享证书提供者的字符串引用使用下拉框 |
| `…tls.certificate_provider.external_account` | ACME 外部账户对象再开一层编辑弹窗 |
| `…tls.certificate_provider.dns01_challenge` | ACME DNS-01 验证对象再开一层编辑弹窗 |
| `…tls.certificate_provider.http_client` | 使用自定义 HTTP 客户端对象时弹窗；共享客户端字符串引用使用下拉框 |
| `…tls.reality.handshake` | 入站 Reality 握手对象弹窗；Reality 外层本身已经内联 |
| `…tls.reality.handshake.domain_resolver` | 对象形式的解析器通过单对象集合入口编辑，再打开 sheet |
| `endpoints[*].tls.control_wrap` | OpenVPN 客户端和服务端的控制通道封装对象弹窗编辑 |
| `…tls.certificate_public_key_sha256` | 数组形式进入复杂集合编辑器，逐项编辑弹窗；也影响 HTTP 客户端 TLS |
| `…tls.client_certificate_public_key_sha256` | 入站客户端证书公钥指纹的数组形式同样进入复杂集合编辑器 |

共享 ACME 证书提供者 `certificate_providers[*]` 下的 `external_account`、`dns01_challenge` 和对象形式 `http_client` 具有相同的嵌套弹窗行为。

ShadowTLS 的 TLS 相关入口也会弹窗：`inbounds[*].handshake`、`inbounds[*].handshake_for_server_name`，后者字典中的握手对象还可逐项打开编辑面板。握手中的对象形式 `domain_resolver` 同样通过单对象集合进入 sheet。这些字段不在 `tls` 主对象内部。

## 已经内联的例外

- HTTP 客户端的 TLS 主对象直接拆成 TLS 分组，没有单独的 TLS 主对象编辑按钮。包括共享 `http_clients[*]` 与自定义 HTTP 客户端对象进入其专用编辑器后的 TLS 内容。
- sing-box 的 `tls.ech`、`tls.utls`、`tls.reality` 已在通用表单内联；但 Reality 的 `handshake` 不在内联列表中。
- 一般证书内容、证书路径列表、ALPN、加密套件等纯字符串列表使用标签编辑。两种公钥指纹联合类型不满足现有纯标量列表判定，是上述例外。
- Mihomo 的根级 `tls` 已由模块直接展开；其 `custom-certifactes` 是字符串列表，本次未发现同样的 TLS 子对象弹窗。

## 原因与证据

1. `SubscriptionTemplateFields.swift:44` 的内联对象名单包含 `ech`、`utls`、`reality`，但没有 `tls`、`certificate_provider`、`external_account`、`dns01_challenge`、`handshake`、`control_wrap`、`http_client`。
2. 普通对象在 `SubscriptionTemplateFields.swift:286` 的分支生成“编辑…”按钮，由 `:241` 的 `.sheet` 打开独立结构编辑器。诊断定位到这类对象的子字段时，`:245` 的逻辑也会自动打开面板。
3. `TemplateHTTPClientFields.swift:48` 将 TLS 拆成分组，并显式以内联方式展示 ECH、uTLS、Reality，因而与普通 TLS 对象表现不同。
4. 公钥指纹支持字符串、字节数组以及由这些类型组成的列表。数组项 Schema 存在两个分支，`SubscriptionTemplateFields.swift:373` 要求的 `itemVariants.count <= 1` 不成立，因此进入 `TemplateCollectionFields`，由其 `:88` 的 `.sheet` 逐项编辑。

使用临时 Swift 探针调用当前 `TemplateSchema`，分别检查单个字符串、字符串数组、整数数组和嵌套整数数组。两个公钥指纹字段的数组样例均得到 `scalar-list=false`、`item-variants=2`、`collection-sheet=true`；单个字符串不会走集合弹窗。该验证确认 Schema 与当前分流条件，不代替真实鼠标操作验证。
