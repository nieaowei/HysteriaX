# 0028 仍使用 integer / string 切换的字段

日期：2026-10-20。范围：当前订阅编辑器、sing-box 1.14.2 与 Mihomo 1.19.31 Schema。本项定位字段与渲染条件。

后续已按用户规则固定编辑类型，见 [0029 固定编辑类型](0029-fixed-scalar-editor-types.md)。以下内容保留定位时的切换器清单。

## sing-box

按字段自身显示的类型选择器统计，字段名去重后共 11 个。`query_type` 顶层是 `integer` / `array`，不计入主清单；其列表项目的内部切换另行说明。

| 字段 | 位置 |
| --- | --- |
| `udp_timeout` | 各入站；OpenVPN 客户端／服务端、Tailscale 端点；API、CCM、DERP、Hysteria Realm、OCM、Resolved、SSM API、USB/IP Server 服务；DERP 的对象形式 `stun` 下也有该字段 |
| `auto_redirect_input_mark` | TUN 入站 |
| `auto_redirect_output_mark` | TUN 入站 |
| `auto_redirect_reset_mark` | TUN 入站 |
| `up` | Hysteria 入站、出站 |
| `down` | Hysteria 入站、出站 |
| `quic_session_receive_window` | Naive 出站 |
| `memory_limit` | `experimental.debug`、`type=oom-killer` 的服务 |
| `safety_margin` | `type=oom-killer` 的服务 |
| `response_rcode` | 普通 DNS 规则及逻辑规则中的嵌套 DNS 匹配规则 |
| `rcode` | `action=predefined` 的 DNS 规则动作 |

`udp_timeout` 的编辑 Schema 只是把整数分支排在前面；TUN 三种标记只是把字符串分支排在前面。它们仍保留另一分支，因此仍有切换器。路由动作中的 `udp_timeout` 使用普通时长字符串，不属于此处的 `UDPTimeout` 联合。

### 单列：query_type 列表项

DNS 规则、嵌套 DNS 匹配规则、规则集中的 HeadlessRule，其 `query_type` 顶层分支标题为 `integer` / `array`。进入数组后，列表项目编辑器使用 `DNSQueryType`，内部选项才是 `integer` / `string`。这是列表项的切换，不能归为字段自身的整数／字符串切换。

## Mihomo

`listeners[*].port` 仍显示 `string` / `integer` 切换，覆盖 http、mixed、redir、socks、tproxy、tun、tunnel 七种监听器。

`proxies[*].hop-interval` 的 Hysteria2 Schema 也保留字符串／整数联合，但动态代理节点在订阅编辑器中只读，主模块不会渲染其可编辑字段，不列为当前可操作切换入口。

## 已使用统一输入的字段

| 字段 | 当前处理 |
| --- | --- |
| `routing_mark`、`default_mark` | `TemplateSchema.editorDefinition` 将编辑定义归一为字符串输入，不再保留类型切换 |
| `stream_receive_window`、`connection_receive_window` | 使用统一容量输入框，按输入保存整数或字符串，绕过通用类型选择器 |

尚未采用容量统一输入的字段包括 `quic_session_receive_window`、`memory_limit`、`safety_margin`，因为当前专用容量输入仅匹配上表两个接收窗口字段名。

## 核查方式与边界

递归检查 Schema 的属性、数组项目、额外属性、联合分支和本地引用，找到 sing-box 152 个、Mihomo 8 个包含整数／字符串可能性的 Schema 位置；这包含命名定义、联合内部节点及复用结构，不等于界面字段数。

临时 Swift 探针调用当前 `TemplateSchema.editorDefinition`、`variants`、`scalarListItemDefinition`、`variantTitle`，结合 `SubscriptionTemplateFields` 的原生控件、容量控件和类型选择器条件判断。排除仅属于 Schema 内部的联合分支与命名定义后，sing-box 有 46 个直接属性定义位置、3 个数组项目定义位置可生成这种选择器；Mihomo 有 7 个监听器端口定义位置，另有只读代理 Schema 一处。

探针编译和运行通过；没有将未执行的真实鼠标操作或逐页截图作为验证结果。字段是否实际出现和可操作，仍取决于已选协议、条件显示、平台能力与产品对动态连接参数的管理规则。

源码入口：

- `SubscriptionTemplateDocument.swift`：编辑定义归一、变体枚举及分支标题。
- `SubscriptionTemplateFields.swift:65`：容量控件白名单；`:68`：原生输入绕过类型选择；`:254`：通用类型选择器。
- `SubscriptionTemplateModules.swift`：动态代理只读入口与模块字段展开。
