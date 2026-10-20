# 0017 必填字段标记补齐

已实施，2026-10-20。

## 问题

上轮只读取 JSON Schema 的 required，而固定版本 Schema 的多数服务器和协议字段没有完整声明 required，因此 UI 仍有大量漏标。缺少 action/version 的草稿还可能在联合类型中选错分支。

## 实施

1. 对照 [sing-box v1.14.2 文档](https://github.com/SagerNet/sing-box/tree/v1.14.2/docs/configuration)，补充 74 组仅用于展示的必填元数据，覆盖 DNS、入站、出站、Endpoint、用户、Reality、规则集、服务和证书提供者。
2. 对照 Mihomo v1.19.31 的 adapter/outboundgroup/parser.go、rules/provider/parse.go、listener/inbound/base.go 与 tunnel.go，补充代理组、规则提供者和监听器的必填项。
3. 条件判断覆盖规则集 format 扩展名推断、Reality 等功能开关、Hysteria 带宽替代字段、Hysteria2 realm/server_ports 和代理组自动引用。
4. 未填写次级判别字段 action/version 时，保留与显式 type/provider 匹配的类型分支。星号继续通过统一字段标签展示，不修改配置或服务端验证约束。

## 验证

- 固定 Schema 全部 74 组展示元数据逐字段覆盖检查。
- DNS 类型切换、规则集扩展名、Mihomo 自动引用、嵌套用户与 WireGuard peer、Reality 开关及替代配置的回归检查。
- macOS 构建及真实 SwiftUI 编辑器类型切换回归。
