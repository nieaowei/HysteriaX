# 0014 模板下拉框空白与类型切换稳定性

日期：2026-10-20

## 目标与定位

排查模板编辑器的空白选项与切换闪退，使用实际崩溃日志和真实 SwiftUI 编辑器回归验证。

- 最新三份应用崩溃日志均指向 `TemplateStructuredFieldEditor.swift` 的字段定义强制解包。切换类型删除字段后，SwiftUI 仍可能刷新旧行。
- sing-box 的 DomainStrategy、规则 type/reject method、UDP mapping/filtering、wildcard_sni 等枚举，以及 mihomo 规则提供者 format 均包含合法的空字符串枚举值。
- 枚举类型判别字段（如 inline/default 的 type）在切换时没有被正确写入。
- 节点注入列表仍存在旧行下标读取，目标代理组选项未过滤空名称与重复名称。

## 实施步骤（已完成）

1. 让复杂表单及内联对象行持有字段定义快照，移除失效字段的强制解包。类型菜单捕获选项快照并校验事件下标。
2. 普通枚举改用值对应的稳定标识绑定；空字符串显示“未设置”，纯空白字符串显示 JSON 转义值，保留原始配置值。空枚举显示禁用占位项，现有未知值继续保留。
3. 修复枚举 type/provider 分支的匹配、类型切换与缺省分支清理；避免把可选 HTTP 客户端 version 枚举当作类型判别条件；为规则动作类型显示实际 action 名称。
4. 对节点注入行使用数据快照，过滤目标代理组选项中的空值与重复项，保护失效移动下标。

## 验证

- 修复前，真实 `TemplateStructuredFieldEditor` 在 remote/local/inline 切换测试中复现日志里的 nil 强制解包崩溃。
- `sh scripts/verify-template-picker-ui.sh`：真实 SwiftUI 表单中的 RuleSet、Outbound、DNSServer、V2RayTransport、DNSRuleAction 切换与撤销/重做，以及节点注入行删除。
- `sh scripts/verify-subscription-client.sh`：两种格式的枚举显示文本、规则集分支转换、缺省类型与未知扩展保留、HTTPClientReference 对象分支及原有客户端测试。
- macOS Debug 构建、`git diff --check`。

仅修改本地客户端，不涉及服务器部署，也不重启用户正在编辑的应用。
