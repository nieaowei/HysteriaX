# 0020 HTTP Client TLS 字段覆盖补齐

已实施，2026-10-20。

## 问题

HTTP Client 的专用 TLS 表单使用基础字段白名单和单个折叠区，没有复用通用编辑器的分组体验。engine、alpn、客户端身份及 ECH/uTLS/Reality 等字段不易发现，已有的未知 TLS 字段也没有提示。

## 实施

1. TLS 字段直接按基础设置、协议协商、证书校验、客户端身份、分片/伪装、内核设置、ECH、uTLS、Reality 分组展示。
2. ECH、uTLS、Reality 内联编辑，复用 enabled 控制字段显示的逻辑，不再另开对象编辑窗口。
3. 分组从 Schema 属性集合生成，剩余字段归入其他设置，避免专用表单遗漏未来字段。
4. 区分 HTTP Client 引擎与 tls.engine：Go 模式展示全部适用字段，系统 TLS 引擎按文档显示支持字段；已有冲突配置支持一次可撤销清理。
5. TLS.enabled 由 HTTP 客户端在 HTTPS 请求中自动启用，表单明确说明。已有未知 TLS 字段显示名称并保留原值。
6. certificate_sha256 是文档标明的 1.15.0 新增字段，当前 1.14.2 不把它声明为已支持。

## 验证

- Schema 全字段唯一覆盖与未来字段兜底测试。
- Apple/Windows TLS 支持范围、清理与有效字段保留测试。
- 共享与内联 HTTP 客户端的嵌套功能开关和 TLS 引擎切换回归。
- macOS Debug 构建、模型回归和 git diff --check。

来源：[HTTP Client](https://sing-box.sagernet.org/zh/configuration/shared/http-client/)、[TLS](https://sing-box.sagernet.org/zh/configuration/shared/tls/)，以及 sing-box v1.14.2 common/httpclient/client.go。
