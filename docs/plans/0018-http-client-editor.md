# 0018 HTTP Client 编辑优化

已实施，2026-10-20。

依据 [HTTP Client 文档](https://sing-box.sagernet.org/zh/configuration/shared/http-client/) 与固定版本 sing-box v1.14.2 的 common/httpclient 源码。

## 实施

1. 顶层共享客户端和 HTTPClientReference 的对象配置共用专用表单，分为基础信息、请求标头、HTTP/2 或 QUIC 参数、TLS、拨号高级设置。
2. HTTP 版本使用明确的默认 HTTP/2、HTTP/1.1、HTTP/2、HTTP/3 选项。HTTP/1.1 隐藏扩展参数，HTTP/3 显示 QUIC 和共享流控字段，Apple 引擎由系统管理版本。
3. 接收窗口以单个输入框同时支持整数和 64 MB 等内存大小字符串。headers 直接使用键值列表和现有标签编辑器。
4. TLS 常用字段直接展开，其他 TLS 字段折叠；HTTP 客户端在 HTTPS 中自动使用 TLS，因此不展示无效的 tls.enabled 开关。
5. Apple 引擎仅提供支持的 TLS 字段；已有冲突参数单独展示，并提供一次可撤销的清理操作，不因切换引擎自动删除用户值。
6. 引用模式显示为共享客户端、自定义配置；客户端列表摘要展示引擎、HTTP 版本和 detour。

## 验证

- 模型测试验证版本可见性、内存大小输入、Apple 不兼容字段清理及未知字段保留。
- 实际 SwiftUI 编辑器回归覆盖共享与内联对象、版本与引擎切换、headers 保留、清理及撤销重做。
- macOS Debug 构建、字段分类覆盖检查、git diff --check。
