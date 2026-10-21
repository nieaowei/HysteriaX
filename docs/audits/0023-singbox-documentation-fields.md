# 0023 sing-box 配置逐字段核查附录

由 `scripts/audit-template-documentation.py` 从固定提交生成。

基线：1.14.2 / `af6e64c3b69e6132ebaee0e1a3d24e93903f6709`；121 页，1173 个页内不重复字段/动作标题。

此附录核查文档与 Schema 的对应关系；`schema` 仅表示类型定义存在，不能等同于每项运行时语义或视觉验收均已验证。具体优化、测试和限制见主报告。

状态：schema=Schema 对应；policy=产品策略限制；historic=历史迁移页；migration=旧语法迁移；reference=说明或独立格式；discriminator=类型/动作标题；omitted=上游省略/废弃字段；upstream_gap=上游文档与 Schema 差异；unresolved=需审查差异。

## certificate

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/certificate/index.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `store` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `certificate` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `certificate_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `certificate_directory_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## dns/fakeip

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/dns/fakeip.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `enabled` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `inet4_range` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `inet6_range` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |

## dns

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/dns/index.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `final` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `strategy` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `disable_cache` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `disable_expire` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `independent_cache` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `cache_capacity` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `optimistic` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `enabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `timeout` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `reverse_mapping` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `client_subnet` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## dns/rule

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/dns/rule.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `inbound` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `ip_version` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `query_type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `query_client_subnet` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `query_dnssec` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `network` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `auth_user` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `protocol` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `domain` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `domain_suffix` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `domain_keyword` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `domain_regex` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `geosite` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `source_geoip` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `source_ip_cidr` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `source_ip_is_private` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `source_port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `source_port_range` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `port_range` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `process_name` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `process_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `process_path_regex` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `package_name` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `package_name_regex` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `user` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `user_id` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `clash_mode` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `network_type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `network_is_expensive` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `network_is_constrained` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `interface_address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `network_interface_address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `default_interface_address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `source_mac_address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `source_hostname` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `preferred_by` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `wifi_ssid` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `wifi_bssid` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `rule_set` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `rule_set_ipcidr_match_source` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `rule_set_ip_cidr_match_source` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `match_response` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `ip_accept_any` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `invert` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `outbound` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `action` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `server` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `disable_cache` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `rewrite_ttl` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `client_subnet` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `geoip` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `ip_cidr` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `ip_is_private` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `rule_set_ip_cidr_accept_empty` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `response_rcode` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `response_answer` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `response_ns` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `response_extra` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `mode` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `rules` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## dns/rule_action

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/dns/rule_action.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `action` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `race` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `route` | discriminator | 这是类型/动作取值标题；对应 Schema 联合类型，不是独立属性。 |
| `server` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `speculative` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `strategy` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `disable_cache` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `disable_optimistic_cache` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `rewrite_ttl` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `timeout` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `client_subnet` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `remove_client_subnet` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `evaluate` | discriminator | 这是类型/动作取值标题；对应 Schema 联合类型，不是独立属性。 |
| `tag` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `respond` | discriminator | 这是类型/动作取值标题；对应 Schema 联合类型，不是独立属性。 |
| `reject` | discriminator | 这是类型/动作取值标题；对应 Schema 联合类型，不是独立属性。 |
| `method` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `no_drop` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `predefined` | discriminator | 这是类型/动作取值标题；对应 Schema 联合类型，不是独立属性。 |
| `rcode` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `answer` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `ns` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `extra` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## dns/server/dhcp

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/dns/server/dhcp.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `interface` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## dns/server/fakeip

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/dns/server/fakeip.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `inet4_range` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `inet6_address` | upstream_gap | 文档标题为 inet6_address；同提交 Schema 和原生选项均为 inet6_range。编辑器遵循 Schema。 |

## dns/server/hosts

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/dns/server/hosts.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `predefined` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## dns/server/http3

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/dns/server/http3.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `server` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `server_port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `headers` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## dns/server/https

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/dns/server/https.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `server` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `server_port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `headers` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## dns/server

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/dns/server/index.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `type` | reference | 说明、动作名或独立规则集文件格式，不是订阅根配置的新字段。 |
| `tag` | reference | 说明、动作名或独立规则集文件格式，不是订阅根配置的新字段。 |

## dns/server/legacy

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/dns/server/legacy.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `tag` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `address` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `address_resolver` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `address_strategy` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `strategy` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `detour` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `client_subnet` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |

## dns/server/local

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/dns/server/local.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `prefer_go` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `neighbor_domain` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## dns/server/mdns

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/dns/server/mdns.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `interface` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## dns/server/openconnect

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/dns/server/openconnect.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `endpoint` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `accept_default_resolvers` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `accept_search_domain` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## dns/server/openvpn

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/dns/server/openvpn.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `endpoint` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `accept_default_resolvers` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `accept_search_domain` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## dns/server/quic

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/dns/server/quic.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `server` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `server_port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## dns/server/resolved

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/dns/server/resolved.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `service` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `accept_default_resolvers` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## dns/server/tailscale

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/dns/server/tailscale.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `endpoint` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `accept_default_resolvers` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `accept_search_domain` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## dns/server/tcp

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/dns/server/tcp.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `server` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `server_port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## dns/server/tls

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/dns/server/tls.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `server` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `server_port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## dns/server/udp

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/dns/server/udp.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `server` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `server_port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## endpoint

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/endpoint/index.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `tag` | reference | 说明、动作名或独立规则集文件格式，不是订阅根配置的新字段。 |

## endpoint/openconnect

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/endpoint/openconnect.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `system` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `name` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `server` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `flavor` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `username` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `password` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `auth_group` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `cookie` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `token` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `token.mode` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `token.secret` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `token.secret_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `token.pin` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `token.password` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `token.device_id` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `token.counter` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `reported_os` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `user_agent` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `version` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `local_hostname` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `mobile` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `mobile.platform_version` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `mobile.device_type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `mobile.device_unique_id` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `csd` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `csd.wrapper_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `hip` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `hip.wrapper_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tncc` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tncc.wrapper_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tncc.device_id` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tncc.user_agent` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tncc.machine_identification_enabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tncc.certificates` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tncc.certificates.certificate` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tncc.certificates.certificate_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `fortinet_host_check` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `fortinet_host_check.hostcheck` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `fortinet_host_check.check_virtual_desktop` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `no_udp` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `dtls_local_port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `compression_disabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `compression_mode` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `ipv6_disabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `http_keepalive_disabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `xml_post_disabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `external_auth_disabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `password_authentication_disabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tcp_keep_alive_enabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `pfs` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `mtu` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `base_mtu` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `dpd_interval` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `reconnect_timeout` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `trojan_interval` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `queue_length` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `allow_insecure_crypto` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.insecure` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.server_name` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.peer_fingerprint` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.system_trust_disabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.certificate_authority` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.certificate_authority_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.client_certificate` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.client_certificate_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.client_key` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.client_key_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.client_key_password` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.mca_certificate` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.mca_certificate_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.mca_key` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.mca_key_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.mca_key_password` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `form_entries` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `form_entries.form_id` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `form_entries.submission_key` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `form_entries.name` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `form_entries.value` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `form_entries.promote` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## endpoint/openvpn-client

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/endpoint/openvpn-client.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `mode` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `server` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `server_port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `servers` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `servers.server` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `servers.server_port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `servers.network` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `remote_random` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `network` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `peer_address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `peer_address_ipv6` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `topology` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `username` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `password` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `auth_retry` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `static_challenge` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `static_challenge_echo` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `static_key` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `static_key_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `key_direction` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.server_name` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.server_name_type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.certificate` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.certificate_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.client_certificate` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.client_certificate_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.client_key` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.client_key_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.peer_fingerprint` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.crl_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.remote_certificate_ku` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.remote_certificate_eku` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.remote_certificate_tls` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.certificate_profile` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.ns_certificate_type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.version_min` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.version_max` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.cipher` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.groups` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.control_wrap` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.control_wrap.type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.control_wrap.key` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.control_wrap.key_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.control_wrap.direction` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `cipher` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `data_ciphers` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `data_ciphers_fallback` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `auth` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `mss_fix` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `mss_fix_disabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `mss_fix_mode` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `fragment` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `replay_window` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `replay_window_time` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `compression` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `compression_lzo` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `allow_compression` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `route_no_pull` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `pull_filters` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `pull_filters.action` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `pull_filters.text` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `routes` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `route_gateway` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `route_metric` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `redirect_gateway` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `redirect_gateway_flags` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `redirect_private` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `block_ipv6` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `ping_interval` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `ping_restart` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `ping_restart_disabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `renegotiate_interval` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `renegotiate_disabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `renegotiate_bytes` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `renegotiate_packets` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls_timeout` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `handshake_window` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `explicit_exit_notify` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `system` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `name` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `mtu` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## endpoint/openvpn-server

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/endpoint/openvpn-server.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `system` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `name` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `mtu` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `mode` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `network` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `remote` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `remote_port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `max_clients` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `peer_address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `peer_address_ipv6` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `topology` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `duplicate_cn` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `users` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `users.username` | upstream_gap | 文档使用小写用户名/密码；上游 User 定义使用 Username/Password。保留固定 Schema，大小写差异单列，不能视为逐字一致。 |
| `users.password` | upstream_gap | 文档使用小写用户名/密码；上游 User 定义使用 Username/Password。保留固定 Schema，大小写差异单列，不能视为逐字一致。 |
| `static_key` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `static_key_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `key_direction` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.certificate` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.certificate_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.key` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.key_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.client_certificate` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.client_certificate_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.verify_client_certificate` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.client_name` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.client_name_type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.peer_fingerprint` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.crl_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.remote_certificate_ku` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.remote_certificate_eku` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.remote_certificate_tls` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.certificate_profile` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.ns_certificate_type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.version_min` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.version_max` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.cipher` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.groups` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.control_wrap` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.control_wrap.type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.control_wrap.key` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.control_wrap.key_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.control_wrap.direction` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls.control_wrap.force_cookie` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `cipher` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `data_ciphers` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `data_ciphers_fallback` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `auth` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `mss_fix` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `mss_fix_disabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `mss_fix_mode` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `replay_window` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `replay_window_time` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `push` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `push.routes` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `push.dns` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `push.dns_servers` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `push.search_domains` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `push.dhcp_options` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `push.redirect_gateway` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `push.redirect_gateway_flags` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `push.block_outside_dns` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `push.ping_interval` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `push.ping_restart` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `ping_interval` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `ping_restart` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `renegotiate_interval` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `renegotiate_disabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `renegotiate_bytes` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `renegotiate_packets` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `handshake_window` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## endpoint/tailscale

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/endpoint/tailscale.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `state_directory` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `auth_key` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `control_url` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `ephemeral` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `hostname` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `accept_routes` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `exit_node` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `exit_node_allow_lan_access` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `advertise_routes` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `advertise_exit_node` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `advertise_tags` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `listen_port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `relay_server_port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `relay_server_static_endpoints` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `system_interface` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `system_interface_name` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `system_interface_mtu` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `udp_timeout` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `ssh_server` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `ssh_server.enabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `ssh_server.disable_pty` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `ssh_server.disable_sftp` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `ssh_server.disable_forwarding` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `taildrop_directory` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## endpoint/wireguard

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/endpoint/wireguard.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `system` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `name` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `mtu` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `private_key` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `peers` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `peers.address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `peers.port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `peers.public_key` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `peers.pre_shared_key` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `peers.allowed_ips` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `peers.persistent_keepalive_interval` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `peers.reserved` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `workers` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## experimental/cache-file

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/experimental/cache-file.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `enabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `cache_id` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `store_fakeip` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `store_rdrc` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `rdrc_timeout` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `store_dns` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## experimental/clash-api

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/experimental/clash-api.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `external_controller` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `external_ui` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `external_ui_download_url` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `external_ui_download_detour` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `secret` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `default_mode` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `access_control_allow_origin` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `access_control_allow_private_network` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `store_mode` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `store_selected` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `store_fakeip` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `cache_file` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `cache_id` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |

## experimental

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/experimental/index.md)

本页无可提取的配置字段标题；已纳入页面清单。

## experimental/v2ray-api

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/experimental/v2ray-api.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `listen` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `stats` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `stats.enabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `stats.inbounds` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `stats.outbounds` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `stats.users` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## inbound/anytls

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/inbound/anytls.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `users` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `padding_scheme` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## inbound/cloudflared

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/inbound/cloudflared.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `token` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `ha_connections` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `protocol` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `post_quantum` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `edge_ip_version` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `datagram_version` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `grace_period` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `region` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `control_dialer` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tunnel_dialer` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## inbound/direct

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/inbound/direct.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `network` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `override_address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `override_port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## inbound/http

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/inbound/http.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `tls` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `users` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `set_system_proxy` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## inbound/hysteria

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/inbound/hysteria.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `up` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `down` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `up_mbps` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `down_mbps` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `obfs` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `users` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `users.auth` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `users.auth_str` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `recv_window_conn` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `recv_window_client` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `max_conn_client` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `disable_mtu_discovery` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |

## inbound/hysteria2

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/inbound/hysteria2.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `up_mbps` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `down_mbps` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `obfs.type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `obfs.password` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `obfs.min_packet_size` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `obfs.max_packet_size` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `users` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `users.password` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `ignore_client_bandwidth` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `masquerade` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `masquerade.type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `masquerade.directory` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `masquerade.url` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `masquerade.rewrite_host` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `masquerade.status_code` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `masquerade.headers` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `masquerade.content` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `bbr_profile` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `brutal_debug` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `realm` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `realm.server_url` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `realm.token` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `realm.realm_id` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `realm.stun_servers` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `realm.stun_domain_resolver` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `realm.ip_version` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `realm.port_mapping` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `realm.port_mapping.enabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `realm.port_mapping.timeout` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `realm.port_mapping.lifetime` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `realm.http_client` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## inbound

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/inbound/index.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `tag` | reference | 说明、动作名或独立规则集文件格式，不是订阅根配置的新字段。 |

## inbound/mixed

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/inbound/mixed.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `users` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `set_system_proxy` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## inbound/naive

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/inbound/naive.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `network` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `users` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `quic_congestion_control` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## inbound/redirect

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/inbound/redirect.md)

本页无可提取的配置字段标题；已纳入页面清单。

## inbound/shadowsocks

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/inbound/shadowsocks.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `network` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `method` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `password` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `managed` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `multiplex` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## inbound/shadowtls

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/inbound/shadowtls.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `version` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `password` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `users` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `handshake` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `handshake_for_server_name` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `strict_mode` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `wildcard_sni` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## inbound/snell

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/inbound/snell.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `version` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `psk` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `users` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `obfs_mode` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `mode` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## inbound/socks

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/inbound/socks.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `users` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## inbound/tproxy

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/inbound/tproxy.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `network` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## inbound/trojan

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/inbound/trojan.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `users` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `fallback` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `fallback_for_alpn` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `multiplex` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `transport` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## inbound/tuic

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/inbound/tuic.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `users` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `users.uuid` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `users.password` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `congestion_control` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `auth_timeout` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `zero_rtt_handshake` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `heartbeat` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## inbound/tun

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/inbound/tun.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `interface_name` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `netns` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `inet4_address` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `inet6_address` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `mtu` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `dns_mode` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `dns_address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `gso` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `auto_route` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `iproute2_table_index` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `iproute2_rule_index` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `auto_redirect` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `auto_redirect_input_mark` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `auto_redirect_output_mark` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `auto_redirect_reset_mark` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `auto_redirect_nfqueue` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `auto_redirect_iproute2_fallback_rule_index` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `exclude_mptcp` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `loopback_address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `strict_route` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `route_address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `inet4_route_address` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `inet6_route_address` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `route_exclude_address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `inet4_route_exclude_address` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `inet6_route_exclude_address` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `route_address_set` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `route_exclude_address_set` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `endpoint_independent_nat` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `stack` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `include_interface` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `exclude_interface` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `include_uid` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `include_uid_range` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `exclude_uid` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `exclude_uid_range` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `include_android_user` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `include_package` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `exclude_package` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `include_mac_address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `exclude_mac_address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `platform` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `platform.http_proxy` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `platform.http_proxy.enabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `platform.http_proxy.server` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `platform.http_proxy.server_port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `platform.http_proxy.bypass_domain` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `platform.http_proxy.match_domain` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## inbound/vless

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/inbound/vless.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `users` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `users.uuid` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `users.flow` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `multiplex` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `transport` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## inbound/vmess

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/inbound/vmess.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `users` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `multiplex` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `transport` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## index

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/index.md)

本页无可提取的配置字段标题；已纳入页面清单。

## log

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/log/index.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `disabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `level` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `output` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `timestamp` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## network-namespace/default

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/network-namespace/default.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## network-namespace

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/network-namespace/index.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tag` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## network-namespace/unshare

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/network-namespace/unshare.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `pid_file` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## ntp

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/ntp/index.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `enabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `server` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `server_port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `interval` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## outbound/anytls

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/outbound/anytls.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `server` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `server_port` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `password` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `idle_session_check_interval` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `idle_session_timeout` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `min_idle_session` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `client_metadata` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `tls` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |

## outbound/block

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/outbound/block.md)

本页无可提取的配置字段标题；已纳入页面清单。

## outbound/bridge

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/outbound/bridge.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `interface` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `bridge_name` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `iproute2_table_index` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `iproute2_rule_index` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |

## outbound/direct

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/outbound/direct.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `override_address` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `override_port` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |

## outbound/dns

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/outbound/dns.md)

本页无可提取的配置字段标题；已纳入页面清单。

## outbound/http

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/outbound/http.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `server` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `server_port` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `username` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `password` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `path` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `headers` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `tls` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |

## outbound/hysteria

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/outbound/hysteria.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `server` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `server_port` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `server_ports` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `hop_interval` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `up` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `down` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `up_mbps` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `down_mbps` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `obfs` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `auth` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `auth_str` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `network` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `tls` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `recv_window_conn` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `recv_window` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `disable_mtu_discovery` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |

## outbound/hysteria2

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/outbound/hysteria2.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `server` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `server_port` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `server_ports` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `hop_interval` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `hop_interval_max` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `up_mbps` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `down_mbps` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `obfs.type` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `obfs.password` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `obfs.min_packet_size` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `obfs.max_packet_size` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `password` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `network` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `tls` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `bbr_profile` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `brutal_debug` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `disable_chrome_parrot` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `realm` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `realm.server_url` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `realm.token` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `realm.realm_id` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `realm.stun_servers` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `realm.ip_version` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `realm.port_mapping` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `realm.port_mapping.enabled` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `realm.port_mapping.timeout` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `realm.port_mapping.lifetime` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `realm.http_client` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |

## outbound

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/outbound/index.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `tag` | reference | 说明、动作名或独立规则集文件格式，不是订阅根配置的新字段。 |

## outbound/naive

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/outbound/naive.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `server` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `server_port` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `username` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `password` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `insecure_concurrency` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `extra_headers` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `stream_receive_window` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `udp_over_tcp` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `quic` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `quic_congestion_control` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `quic_session_receive_window` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `tls` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |

## outbound/selector

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/outbound/selector.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `outbounds` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `default` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `interrupt_exist_connections` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## outbound/shadowsocks

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/outbound/shadowsocks.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `server` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `server_port` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `method` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `password` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `plugin` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `plugin_opts` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `network` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `udp_over_tcp` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `multiplex` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |

## outbound/shadowtls

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/outbound/shadowtls.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `server` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `server_port` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `version` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `password` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `tls` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |

## outbound/snell

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/outbound/snell.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `server` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `server_port` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `version` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `psk` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `userkey` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `reuse` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `network` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `obfs_mode` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `obfs_host` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `mode` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |

## outbound/socks

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/outbound/socks.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `server` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `server_port` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `version` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `username` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `password` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `network` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `udp_over_tcp` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |

## outbound/ssh

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/outbound/ssh.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `server` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `server_port` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `user` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `password` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `private_key` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `private_key_path` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `private_key_passphrase` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `host_key` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `host_key_algorithms` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `client_version` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `cipher` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `mac` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `kex_algorithm` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |

## outbound/tor

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/outbound/tor.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `executable_path` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `extra_args` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `data_directory` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `torrc` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |

## outbound/trojan

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/outbound/trojan.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `server` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `server_port` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `password` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `network` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `tls` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `multiplex` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `transport` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |

## outbound/tuic

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/outbound/tuic.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `server` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `server_port` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `uuid` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `password` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `congestion_control` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `udp_relay_mode` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `udp_over_stream` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `network` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `tls` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |

## outbound/urltest

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/outbound/urltest.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `outbounds` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `url` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `interval` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tolerance` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `idle_timeout` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `interrupt_exist_connections` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## outbound/vless

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/outbound/vless.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `server` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `server_port` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `uuid` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `flow` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `network` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `tls` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `packet_encoding` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `multiplex` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `transport` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |

## outbound/vmess

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/outbound/vmess.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `server` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `server_port` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `uuid` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `security` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `alter_id` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `global_padding` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `authenticated_length` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `network` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `tls` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `packet_encoding` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `multiplex` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |
| `transport` | policy | 产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。 |

## outbound/wireguard

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/outbound/wireguard.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `server` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `server_port` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `system_interface` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `interface_name` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `gso` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `local_address` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `private_key` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `peers` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `peers.allowed_ips` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `peers.reserved` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `peer_public_key` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `pre_shared_key` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `reserved` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `workers` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `mtu` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `network` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |

## route/geoip

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/route/geoip.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `path` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `download_url` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `download_detour` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |

## route/geosite

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/route/geosite.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `path` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `download_url` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |
| `download_detour` | historic | 上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。 |

## route

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/route/index.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `rules` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `rule_set` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `final` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `auto_detect_interface` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `override_android_vpn` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `default_interface` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `default_mark` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `find_process` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `find_neighbor` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `dhcp_lease_files` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `default_http_client` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `default_domain_resolver` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `default_network_strategy` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `default_network_type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `default_fallback_network_type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `default_fallback_delay` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## route/rule

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/route/rule.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `inbound` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `ip_version` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `auth_user` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `protocol` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `client` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `network` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `domain` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `domain_suffix` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `domain_keyword` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `domain_regex` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `geosite` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `source_geoip` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `geoip` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `source_ip_cidr` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `ip_is_private` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `ip_cidr` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `source_ip_is_private` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `source_port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `source_port_range` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `port_range` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `process_name` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `process_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `process_path_regex` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `package_name` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `package_name_regex` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `user` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `user_id` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `clash_mode` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `network_type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `network_is_expensive` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `network_is_constrained` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `interface_address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `network_interface_address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `default_interface_address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `wifi_ssid` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `wifi_bssid` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `preferred_by` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `source_mac_address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `source_hostname` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `rule_set` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `rule_set_ipcidr_match_source` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `rule_set_ip_cidr_match_source` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `invert` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `action` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `outbound` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `mode` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `rules` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## route/rule_action

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/route/rule_action.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `route` | discriminator | 这是类型/动作取值标题；对应 Schema 联合类型，不是独立属性。 |
| `outbound` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `bypass` | discriminator | 这是类型/动作取值标题；对应 Schema 联合类型，不是独立属性。 |
| `reject` | discriminator | 这是类型/动作取值标题；对应 Schema 联合类型，不是独立属性。 |
| `method` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `no_drop` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `override_address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `override_port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `network_strategy` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `network_type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `fallback_network_type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `fallback_delay` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `udp_disable_domain_unmapping` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `udp_connect` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `udp_timeout` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls_fragment` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls_fragment_fallback_delay` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls_record_fragment` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls_spoof` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls_spoof_method` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `sniff` | discriminator | 这是类型/动作取值标题；对应 Schema 联合类型，不是独立属性。 |
| `sniffer` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `timeout` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `resolve` | discriminator | 这是类型/动作取值标题；对应 Schema 联合类型，不是独立属性。 |
| `server` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `strategy` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `disable_cache` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `disable_optimistic_cache` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `rewrite_ttl` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `client_subnet` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## route/sniff

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/route/sniff.md)

本页无可提取的配置字段标题；已纳入页面清单。

## rule-set/adguard

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/rule-set/adguard.md)

本页无可提取的配置字段标题；已纳入页面清单。

## rule-set/headless-rule

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/rule-set/headless-rule.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `query_type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `network` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `domain` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `domain_suffix` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `domain_keyword` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `domain_regex` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `source_ip_cidr` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `ip_cidr` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `source_port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `source_port_range` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `port_range` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `process_name` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `process_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `process_path_regex` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `package_name` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `package_name_regex` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `network_type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `network_is_expensive` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `network_is_constrained` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `network_interface_address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `default_interface_address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `wifi_ssid` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `wifi_bssid` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `invert` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `mode` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `rules` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## rule-set

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/rule-set/index.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tag` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `rules` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `format` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `url` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `initial_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `http_client` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `update_interval` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `download_detour` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |

## rule-set/source-format

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/rule-set/source-format.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `version` | reference | 说明、动作名或独立规则集文件格式，不是订阅根配置的新字段。 |
| `rules` | reference | 说明、动作名或独立规则集文件格式，不是订阅根配置的新字段。 |

## schema

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/schema.md)

本页无可提取的配置字段标题；已纳入页面清单。

## service/api

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/service/api.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `secret` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `access_control_allow_origin` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `access_control_allow_private_network` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `dashboard` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `enabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `download_url` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `http_client` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `update_interval` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## service/ccm

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/service/ccm.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `credential_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `usages_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `users` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `headers` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `detour` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## service/derp

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/service/derp.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `tls` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `config_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `verify_client_endpoint` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `verify_client_url` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `home` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `mesh_with` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `mesh_psk` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `mesh_psk_file` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `stun` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## service/hysteria-realm

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/service/hysteria-realm.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `tls` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `users` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `users.name` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `users.token` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `users.max_realms` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## service

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/service/index.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `tag` | reference | 说明、动作名或独立规则集文件格式，不是订阅根配置的新字段。 |

## service/ocm

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/service/ocm.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `credential_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `usages_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `users` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `headers` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `detour` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## service/resolved

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/service/resolved.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `listen` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `listen_port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## service/ssm-api

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/service/ssm-api.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `servers` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `cache_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tls` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## service/usbip-client

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/service/usbip-client.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `server` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `server_port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `devices` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## service/usbip-server

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/service/usbip-server.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `provider` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `devices` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## shared/certificate-provider/acme

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/shared/certificate-provider/acme.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `domain` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `data_directory` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `default_server_name` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `email` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `provider` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `account_key` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `disable_http_challenge` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `disable_tls_alpn_challenge` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `alternative_http_port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `alternative_tls_port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `external_account` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `external_account.key_id` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `external_account.mac_key` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `dns01_challenge` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `key_type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `profile` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `http_client` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## shared/certificate-provider/cloudflare-origin-ca

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/shared/certificate-provider/cloudflare-origin-ca.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `domain` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `data_directory` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `api_token` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `origin_ca_key` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `request_type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `requested_validity` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `http_client` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## shared/certificate-provider

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/shared/certificate-provider/index.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `tag` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## shared/certificate-provider/tailscale

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/shared/certificate-provider/tailscale.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `endpoint` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## shared/dial

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/shared/dial.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `detour` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `bind_interface` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `inet4_bind_address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `inet6_bind_address` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `bind_address_no_port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `routing_mark` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `reuse_addr` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `netns` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `connect_timeout` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tcp_fast_open` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tcp_multi_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `disable_tcp_keep_alive` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tcp_keep_alive` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tcp_keep_alive_interval` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `udp_fragment` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `domain_resolver` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `network_strategy` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `network_type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `fallback_network_type` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `fallback_delay` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `domain_strategy` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |

## shared/dns01_challenge

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/shared/dns01_challenge.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `ttl` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `propagation_delay` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `propagation_timeout` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `resolvers` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `override_domain` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `provider` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `security_token` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `zone_token` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## shared/http-client

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/shared/http-client.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `engine` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `version` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `disable_version_fallback` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `headers` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## shared/http2

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/shared/http2.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `idle_timeout` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `keep_alive_period` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `stream_receive_window` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `connection_receive_window` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `max_concurrent_streams` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## shared/listen

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/shared/listen.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `listen` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `listen_port` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `bind_interface` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `routing_mark` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `reuse_addr` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `netns` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tcp_fast_open` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tcp_multi_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `disable_tcp_keep_alive` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tcp_keep_alive` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `tcp_keep_alive_interval` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `udp_fragment` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `udp_timeout` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `detour` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `sniff` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `sniff_override_destination` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `sniff_timeout` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `domain_strategy` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `udp_disable_domain_unmapping` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |

## shared/multiplex

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/shared/multiplex.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `enabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `padding` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `brutal` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `protocol` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `max_connections` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `min_streams` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `max_streams` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## shared/neighbor

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/shared/neighbor.md)

本页无可提取的配置字段标题；已纳入页面清单。

## shared/pre-match

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/shared/pre-match.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `reject` | reference | 说明、动作名或独立规则集文件格式，不是订阅根配置的新字段。 |
| `route` | reference | 说明、动作名或独立规则集文件格式，不是订阅根配置的新字段。 |
| `sniff` | reference | 说明、动作名或独立规则集文件格式，不是订阅根配置的新字段。 |
| `bypass` | reference | 说明、动作名或独立规则集文件格式，不是订阅根配置的新字段。 |

## shared/quic

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/shared/quic.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `initial_packet_size` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `disable_path_mtu_discovery` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## shared/tcp-brutal

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/shared/tcp-brutal.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `enabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `up_mbps` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `down_mbps` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## shared/tls

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/shared/tls.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `enabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `engine` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `disable_sni` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `server_name` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `insecure` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `alpn` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `min_version` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `max_version` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `cipher_suites` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `curve_preferences` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `certificate` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `certificate_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `certificate_public_key_sha256` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `client_certificate` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `client_certificate_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `client_key` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `client_key_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `key` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `key_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `client_authentication` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `client_certificate_public_key_sha256` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `kernel_tx` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `kernel_rx` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `handshake_timeout` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `certificate_provider` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `utls` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `pq_signature_schemes_enabled` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `dynamic_record_sizing_disabled` | omitted | 文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。 |
| `config` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `config_path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `query_server_name` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `fragment` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `fragment_fallback_delay` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `record_fragment` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `spoof` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `spoof_method` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `domain` | migration | 旧内联 ACME 段落；当前 Schema 使用共享 certificate_providers，不能重新加入旧 TLS 表单。 |
| `data_directory` | migration | 旧内联 ACME 段落；当前 Schema 使用共享 certificate_providers，不能重新加入旧 TLS 表单。 |
| `default_server_name` | migration | 旧内联 ACME 段落；当前 Schema 使用共享 certificate_providers，不能重新加入旧 TLS 表单。 |
| `email` | migration | 旧内联 ACME 段落；当前 Schema 使用共享 certificate_providers，不能重新加入旧 TLS 表单。 |
| `provider` | migration | 旧内联 ACME 段落；当前 Schema 使用共享 certificate_providers，不能重新加入旧 TLS 表单。 |
| `disable_http_challenge` | migration | 旧内联 ACME 段落；当前 Schema 使用共享 certificate_providers，不能重新加入旧 TLS 表单。 |
| `disable_tls_alpn_challenge` | migration | 旧内联 ACME 段落；当前 Schema 使用共享 certificate_providers，不能重新加入旧 TLS 表单。 |
| `alternative_http_port` | migration | 旧内联 ACME 段落；当前 Schema 使用共享 certificate_providers，不能重新加入旧 TLS 表单。 |
| `alternative_tls_port` | migration | 旧内联 ACME 段落；当前 Schema 使用共享 certificate_providers，不能重新加入旧 TLS 表单。 |
| `external_account` | migration | 旧内联 ACME 段落；当前 Schema 使用共享 certificate_providers，不能重新加入旧 TLS 表单。 |
| `external_account.key_id` | migration | 旧内联 ACME 段落；当前 Schema 使用共享 certificate_providers，不能重新加入旧 TLS 表单。 |
| `external_account.mac_key` | migration | 旧内联 ACME 段落；当前 Schema 使用共享 certificate_providers，不能重新加入旧 TLS 表单。 |
| `dns01_challenge` | migration | 旧内联 ACME 段落；当前 Schema 使用共享 certificate_providers，不能重新加入旧 TLS 表单。 |
| `handshake` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `private_key` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `public_key` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `short_id` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `max_time_difference` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## shared/udp-nat

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/shared/udp-nat.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `udp_timeout` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `udp_mapping` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `udp_filtering` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `udp_nat_max` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## shared/udp-over-tcp

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/shared/udp-over-tcp.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `enabled` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `version` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## shared/v2ray-transport

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/shared/v2ray-transport.md)

| 字段/标题 | 核查结果 | 说明 |
| --- | --- | --- |
| `host` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `path` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `method` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `headers` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `idle_timeout` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `ping_timeout` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `max_early_data` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `early_data_header_name` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `gRPC` | discriminator | 这是类型/动作取值标题；对应 Schema 联合类型，不是独立属性。 |
| `service_name` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |
| `permit_without_stream` | schema | 固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。 |

## shared/wifi-state

[固定版本原文](https://github.com/SagerNet/sing-box/blob/af6e64c3b69e6132ebaee0e1a3d24e93903f6709/docs/configuration/shared/wifi-state.md)

本页无可提取的配置字段标题；已纳入页面清单。
