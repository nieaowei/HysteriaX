# 0024 默认值专项核查与补充报告

日期：2026-10-20。承接 0023 报告；上一轮完成了字段对应关系，但仍漏掉一些默认值和显示路径。

## 本次修复

- 统一维护并逐项检查 90 个按对象/协议区分的固定默认值，另处理模式、父级配置和内联对象决定的默认值。
- OpenVPN TLS 最低版本从 Schema 首项 1.0 修正为实际默认 1.2；服务端远端证书用途默认为 client，不再显示首项 server。
- OpenVPN MTU=1500、TLS 最大客户端数=1024；静态密钥模式使用 1、拓扑 p2p，TLS 服务端拓扑为 subnet。
- TUIC 心跳=10s、认证超时=3s；OpenConnect 重连超时=300s、队列=32、base_mtu=1406 等补齐。
- 补齐缓存、Clash CORS/模式/下载地址、共享 HTTP 引擎、证书签发者、USB/IP、Resolved、DERP STUN、API dashboard 和 Realm 端口映射等默认值。
- UDPTimeout 兼容字段以整数秒 300 预览；只接受 Duration 的 WireGuard 字段仍以 5m 预览。
- DNS 规则/域名解析器查询超时继承 dns.timeout；sniff 动作超时默认为 300ms；OpenVPN 单个远端继承其端点 network。
- 进入“显式设置”使用与表单预览一致的值；不再另走枚举首项/零值。
- 列表控件复用默认值预览，显式空数组优先，浏览列表不创建草稿修改。
- 没有固定默认值的枚举显示“未设置（由内核决定）”；整数输入保持空白，避免把首项或 0 当作实际默认。

## 固定值清单

以下直接从当前默认值目录生成。测试逐项确认属性存在、类型正确且枚举包含该值。90 项是目录条目数，含入站/出站或共享/内联对象的不同上下文，不是 90 个此前全部缺失的独立字段。

| 对象或协议 | 字段 | 默认预览 |
| --- | --- | --- |
| `LogOptions` | `level` | `"trace"` |
| `NetworkNamespace` | `type` | `"default"` |
| `RouteOptions` | `default_network_strategy` | `"default"` |
| `InboundTLSOptions` | `min_version` | `"1.2"` |
| `InboundTLSOptions` | `max_version` | `"1.3"` |
| `InboundTLSOptions` | `handshake_timeout` | `"15s"` |
| `InboundTLSOptions` | `client_authentication` | `"no"` |
| `OutboundTLSOptions` | `engine` | `"go"` |
| `OutboundTLSOptions` | `min_version` | `"1.2"` |
| `OutboundTLSOptions` | `max_version` | `"1.3"` |
| `OutboundTLSOptions` | `handshake_timeout` | `"15s"` |
| `OutboundTLSOptions` | `fragment_fallback_delay` | `"500ms"` |
| `OutboundTLSOptions` | `spoof_method` | `"wrong-sequence"` |
| `OutboundUTLSOptions` | `fingerprint` | `"chrome"` |
| `HTTPClient` | `engine` | `"go"` |
| `HTTPClient` | `version` | `2` |
| `HTTPClientReference` | `engine` | `"go"` |
| `HTTPClientReference` | `version` | `2` |
| `CacheFileOptions` | `path` | `"cache.db"` |
| `CacheFileOptions` | `rdrc_timeout` | `"7d"` |
| `ClashAPIOptions` | `default_mode` | `"Rule"` |
| `ClashAPIOptions` | `external_ui_download_url` | `"https://github.com/MetaCubeX/Yacd-meta/archive/gh-pages.zip"` |
| `ClashAPIOptions` | `access_control_allow_origin` | `["*"]` |
| `Inbound:tun` | `dns_mode` | `"hijack"` |
| `Inbound:tun` | `iproute2_table_index` | `2022` |
| `Inbound:tun` | `iproute2_rule_index` | `9000` |
| `Inbound:tun` | `auto_redirect_input_mark` | `"0x2023"` |
| `Inbound:tun` | `auto_redirect_output_mark` | `"0x2024"` |
| `Inbound:tun` | `auto_redirect_reset_mark` | `"0x2025"` |
| `Inbound:tun` | `auto_redirect_nfqueue` | `100` |
| `Inbound:tun` | `auto_redirect_iproute2_fallback_rule_index` | `32768` |
| `Inbound:tuic` | `congestion_control` | `"cubic"` |
| `Inbound:tuic` | `auth_timeout` | `"3s"` |
| `Inbound:tuic` | `heartbeat` | `"10s"` |
| `Inbound:shadowtls` | `version` | `1` |
| `Inbound:shadowtls` | `wildcard_sni` | `"off"` |
| `Inbound:hysteria2` | `bbr_profile` | `"standard"` |
| `Inbound:naive` | `quic_congestion_control` | `"bbr"` |
| `Outbound:anytls` | `idle_session_check_interval` | `"30s"` |
| `Outbound:anytls` | `idle_session_timeout` | `"30s"` |
| `Outbound:anytls` | `min_idle_session` | `0` |
| `Outbound:tuic` | `congestion_control` | `"cubic"` |
| `Outbound:tuic` | `udp_relay_mode` | `"native"` |
| `Outbound:shadowtls` | `version` | `1` |
| `Outbound:hysteria` | `hop_interval` | `"30s"` |
| `Outbound:hysteria2` | `hop_interval` | `"30s"` |
| `Outbound:hysteria2` | `bbr_profile` | `"standard"` |
| `Outbound:naive` | `quic_congestion_control` | `"cubic"` |
| `Outbound:naive` | `quic_session_receive_window` | `"15 MB"` |
| `Outbound:socks` | `version` | `"5"` |
| `Outbound:ssh` | `server_port` | `22` |
| `Outbound:ssh` | `user` | `"root"` |
| `Outbound:vless` | `packet_encoding` | `"xudp"` |
| `Outbound:bridge` | `bridge_name` | `"bridge"` |
| `Outbound:bridge` | `iproute2_rule_index` | `100` |
| `Endpoint:tailscale` | `state_directory` | `"tailscale"` |
| `Endpoint:tailscale` | `control_url` | `"https://controlplane.tailscale.com"` |
| `Endpoint:tailscale` | `taildrop_directory` | `"Taildrop"` |
| `Endpoint:openconnect` | `flavor` | `"anyconnect"` |
| `Endpoint:openconnect` | `version` | `"v9.21"` |
| `Endpoint:openconnect` | `compression_mode` | `"stateless"` |
| `Endpoint:openconnect` | `base_mtu` | `1406` |
| `Endpoint:openconnect` | `reconnect_timeout` | `"300s"` |
| `Endpoint:openconnect` | `queue_length` | `32` |
| `OpenConnectTNCCOptions` | `user_agent` | `"Neoteris HC Http"` |
| `V2RayTransport:http` | `idle_timeout` | `"0s"` |
| `V2RayTransport:http` | `ping_timeout` | `"15s"` |
| `OpenVPNOutboundTLSOptions` | `server_name_type` | `"name"` |
| `OpenVPNOutboundTLSOptions` | `remote_certificate_tls` | `"server"` |
| `OpenVPNOutboundTLSOptions` | `certificate_profile` | `"legacy"` |
| `OpenVPNOutboundTLSOptions` | `version_min` | `"1.2"` |
| `OpenVPNInboundTLSOptions` | `verify_client_certificate` | `"require"` |
| `OpenVPNInboundTLSOptions` | `client_name_type` | `"name"` |
| `OpenVPNInboundTLSOptions` | `remote_certificate_tls` | `"client"` |
| `OpenVPNInboundTLSOptions` | `certificate_profile` | `"legacy"` |
| `OpenVPNInboundTLSOptions` | `version_min` | `"1.2"` |
| `CertificateProvider:acme` | `provider` | `"letsencrypt"` |
| `CertificateProviderReference:acme` | `provider` | `"letsencrypt"` |
| `CertificateProvider:cloudflare-origin-ca` | `request_type` | `"origin-rsa"` |
| `CertificateProvider:cloudflare-origin-ca` | `requested_validity` | `5475` |
| `CertificateProviderReference:cloudflare-origin-ca` | `request_type` | `"origin-rsa"` |
| `CertificateProviderReference:cloudflare-origin-ca` | `requested_validity` | `5475` |
| `Service:resolved` | `listen` | `"127.0.0.53"` |
| `Service:resolved` | `listen_port` | `53` |
| `Service:api` | `access_control_allow_origin` | `["*"]` |
| `Service:usbip-client` | `server_port` | `3240` |
| `Service:usbip-server` | `listen_port` | `3240` |
| `Service:usbip-server` | `provider` | `"default"` |
| `Hysteria2RealmPortMapping` | `timeout` | `"10s"` |
| `Hysteria2RealmPortMapping` | `lifetime` | `"10m"` |

## 条件默认值与动态默认值

| 场景 | 处理 |
| --- | --- |
| OpenVPN TLS / static_key | max_clients、topology、cipher、TLS 重协商、data_ciphers 等按会话模式选择；data_ciphers 预览三个原生算法，保留原始顺序。 |
| OpenVPN mss_fix / ping_restart / renegotiate_bytes / renegotiate_packets | 依赖 MTU、传输、算法和服务器协商，提供说明，不猜固定值。 |
| OpenVPN TLS version_max / cipher / groups | 由目标 TLS 实现和证书策略决定，不把 enum 首项 1.0 当成最大版本。 |
| TUN stack | 启用 gVisor 构建时 mixed，否则 system；提供说明，不读本地构建结果代替目标客户端。 |
| DNS Hosts 路径 | 随目标操作系统变化，不填入编辑器所在 Mac 的路径。 |
| WireGuard workers | 由目标 CPU 数量决定，不取编辑器 CPU 数量。 |
| Tailscale 主机名、系统接口、MTU、监听端口 | 根据目标平台及接口状态决定。 |
| OpenConnect reported_os / user_agent / DPD / 系统保活 | 根据目标平台、VPN flavor、协商及系统设置决定。 |
| 证书目录 | 根据目标环境 XDG_DATA_HOME/HOME 等决定，不将本机路径写入模板。 |
| TLS 算法、曲线与服务器名 | 由 TLS 实现、对端地址、握手配置决定，展示解释。 |
| gRPC keepalive | 标准 gRPC 与内置 gRPC 不同，提供说明，不从 JSON 示例提取默认值。 |
| DNS final / route final / selector default | 未设置时选对应列表首项；保留引用字段的未设置状态。 |

## 源码交叉核查

- 日志文档示例使用 info，但该版本 log/log.go 的 New 在未设置 level 时采用 LevelTrace。目录因此预览 trace；已有明确 info 配置保持 info。
- FakeIP 文档示例中的网段不当作默认值：dns/transport/fakeip/fakeip.go 明确要求至少设置一个范围。
- option/inbound.go 的 UDPTimeoutCompat 将整数解释为秒，因此兼容字段使用 300，Duration-only 字段保持 5m。
- OpenConnect 未设置 TCP 保活时保留操作系统时序，未用通用拨号的 5m/75s 覆盖。

来源：[固定版本配置文档](https://github.com/SagerNet/sing-box/tree/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration)、[日志初始化](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/log/log.go)、[UDPTimeout 兼容类型](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/option/inbound.go)。

## 验证

- 客户端模型：90 个固定目录条目的属性、类型、枚举范围；上下文默认、显式设置、null、空数组、撤销及浏览不落盘。
- 原生 SwiftUI：实际读取数值输入，确认 1500、1024 及未知动态整数保持空白；枚举与列表进行真实视图渲染及草稿不变回归。枚举语义另由模型断言覆盖。
- 原有类型切换、HTTP Client、开关、复杂集合、大列表、双语资源和 macOS Debug 构建回归。

这些值用于编辑预览；已有显式值始终优先。动态默认值不冒充固定数值，未从示例配置自动生成默认值。

验证结果：以上模型、原生 SwiftUI、集合、双语资源、文档审计回归和 macOS Debug 构建均通过；`git diff --check` 通过。

复查命令：

```sh
sh scripts/verify-subscription-client.sh
sh scripts/verify-template-picker-ui.sh
sh scripts/verify-template-collection-ui.sh
bash scripts/verify-localization.sh
python3 tests/template-documentation-audit.py
```
