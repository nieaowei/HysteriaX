# 0027 TLS 主对象内联与 enabled 默认值核查

日期：2026-10-20。基线：sing-box 1.14.2，提交 `af6e64c3b69e6132ebaee0e1a3d24e93903f6709`。

## 实施行为

`tls` 主对象在所属配置表单内直接展示，取消该层“编辑…”与 TLS 专属 sheet。诊断定位到 TLS 子字段时也不再因主对象自动打开 sheet。

普通 TLS 首先展示 `enabled`；缺失时预览关闭。启用后显示参数，关闭后隐藏参数但保留原值，撤销／重做可恢复开关状态。打开表单、查看默认值不会创建 TLS 对象或写入默认值。

TLS 的内联策略集中到 `TemplateEditorGuidance.isInlineObject`，视图展示、对象详情委托和诊断定位使用同一策略。默认值与参数显现使用相同的默认值来源，避免开关看起来关闭而字段已展开。

DNS 的加密传输及 HTTP 客户端的 HTTPS 请求由协议自动启用 TLS。这些位置直接显示参数并说明自动启用，隐藏不能实际关闭 TLS 的 `enabled` 控件；已有源码中的 `enabled=false` 仍保留，也不会误把参数区隐藏。

OpenConnect 与 OpenVPN 的专用 TLS Schema 没有 `enabled`，直接内联其参数。OpenVPN 继续由上层 `mode` 控制：缺失时为 `tls`，选择 `static_key` 时整个 TLS 配置隐藏并保留数据。

## 各位置默认值

下表按 Schema 中的协议位置计数。同一 TLS 类型被多个协议引用时分别计数。

| 位置 | 协议 | 数量 | `enabled` 缺失时的实际行为 |
| --- | --- | ---: | --- |
| `inbounds[*].tls` | anytls、http、hysteria、hysteria2、mixed、naive、trojan、tuic、vless、vmess | 10 | `false`；关闭状态只显示开关 |
| `outbounds[*].tls` | anytls、http、hysteria、hysteria2、naive、shadowtls、trojan、tuic、vless、vmess | 10 | `false`；关闭状态只显示开关 |
| `services[*].tls` | api、ccm、derp、hysteria-realm、ocm、ssm-api | 6 | `false`；关闭状态只显示开关 |
| `dns.servers[*].tls` | tls、quic、https、h3 | 4 | 有效状态为启用；传输构造器强制 `Enabled=true`，显式 false 也不能关闭 |
| `http_clients[*].tls` | HTTP 客户端 | 1 | HTTPS 请求自动使用 TLS；不通过此开关启用或关闭 |
| `endpoints[*].tls` | openconnect、openvpn-client、openvpn-server | 3 | 无此字段；不伪造 false 开关 |
| 自定义 HTTP 客户端对象的 `tls` | `HTTPClientReference` 的对象分支 | 额外检查 | 与共享 HTTP 客户端一致；字符串引用不生成 TLS 子表单 |

共 34 个主对象位置：26 个默认关闭、5 个自动启用、3 个无开关的专用对象，另检查自定义 HTTP 客户端引用。

TLS 子功能也分别核查：入站 ECH、入站 Reality、出站 ECH、出站 uTLS、出站 Reality 的 `enabled` 缺失时均为 `false`。主 TLS 启用不会同时自动启用这些子功能。

**协议要求 TLS 与默认启用是两件事。** 例如 Hysteria/Hysteria2、TUIC 等要求显式启用 TLS，省略或关闭会导致构造失败，不能因此把默认值填为 true。在线配置示例中的 `"enabled": true` 也不视为默认值定义。

## 核查依据

- [TLS 文档](https://sing-box.sagernet.org/zh/configuration/shared/tls/)：启用字段与子功能说明；在线站点滚动更新，不把 1.15 新字段加入当前 Schema。
- [固定版 TLS 选项](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/option/tls.go)：主 TLS 和五个子功能开关均为 bool，缺失时为零值 false。
- [固定版服务端 TLS 构造](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/common/tls/server.go)、[客户端 TLS 构造](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/common/tls/client.go)：关闭时不创建 TLS 配置。
- DNS 强制启用：[TLS](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/dns/transport/tls.go)、[HTTPS](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/dns/transport/https.go)、[QUIC](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/dns/transport/quic/quic.go)、[H3](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/dns/transport/quic/http3.go)。
- HTTP 客户端：[Go 引擎](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/common/httpclient/client.go)、[Apple 引擎](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/common/httpclient/apple_transport_darwin.go)。
- 必需 TLS 示例：[Hysteria2 出站](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/protocol/hysteria2/outbound.go)、[DERP 服务](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/service/derp/service.go)。
- 无开关的专用选项由当前 Schema 的 `OpenConnectTLSOptions`、`OpenVPNOutboundTLSOptions`、`OpenVPNInboundTLSOptions` 与固定版端点构造实现交叉确认；OpenVPN 默认模式在 [客户端](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/protocol/openvpn/client.go) 和 [服务端](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/protocol/openvpn/server.go) 均回落到 TLS 模式。

## 验证

模型对全部 34 个主对象位置分别检查缺失、null、空对象、仅参数、显式 false、显式 true、enabled 为 null 共七种状态，检查默认值、显现、内联策略与不改写草稿；另检查自定义 HTTP 客户端及五个 TLS 子功能开关。

null 情形用于验证未完成草稿的显示和数据保留，不表示这些值均为有效内核配置。

原生视图回归直接检查实际文本输入是否出现：普通 TLS 在开关后显现／隐藏，自动 TLS 在显式 false 时仍展示参数，专用 VPN TLS 直接展示参数；诊断定位与开关操作均检查主窗口没有附加 TLS sheet，保留字段并验证撤销重做。原生测试使用离屏 NSHostingView，不代替协议联网测试。

命令：`sh scripts/verify-subscription-client.sh`、`sh scripts/verify-template-picker-ui.sh`、`bash scripts/verify-localization.sh`、macOS Debug `xcodebuild`、`git diff --check`。

以上检查均通过。原生回归已确认实际参数输入的显现／隐藏、自动 TLS 的强制启用行为、VPN 专用 TLS 内联、诊断定位不创建 TLS sheet，以及配置保留和撤销重做。
