# 0029 整数／字符串联合字段固定编辑类型

日期：2026-10-20。依据用户指定规则：以 `mark` 结尾固定为字符串，其余整数／字符串联合固定为整数。

## 实施规则

`TemplateSchema.editorDefinition` 检查联合的分支类型：只有 `integer`、`string` 两种类型时，按字段名后缀选取一个分支，不再保留类型选择器。

- 以 `mark` 结尾：选择字符串分支，输入保存为 JSON 字符串，包括十六进制标记；已有整数标记以原值显示，打开表单不改写。
- 其余：选择整数分支，保留原有数值范围等 Schema 约束。该类控件仅将可解析整数写入文档，兼容 `Int.min` 和 `UInt64.max`，不经过浮点数转换。
- 不完整、带单位、带小数或溢出的输入留在本地输入草稿并给出提示，不写成 JSON 字符串，不覆盖最后有效配置。
- 已有字符串（例如时长、容量或端口范围）保留显示，并说明修改时应输入整数；不在查看表单时猜测单位或批量转换。
- 不改服务端 Schema、验证器、只读动态节点权限或已有未知字段。

这也统一了此前采用容量输入的 `stream_receive_window`、`connection_receive_window`：现在按同一规则使用整数输入，移除接受新单位字符串的专用路径。

## 字段范围

字符串编辑：`auto_redirect_input_mark`、`auto_redirect_output_mark`、`auto_redirect_reset_mark`、`routing_mark`、`default_mark`，以及以后出现且符合相同联合类型的 `mark` 后缀字段。

整数编辑包括：`udp_timeout`、Hysteria `up` / `down`、Naive `quic_session_receive_window`、`memory_limit`、`safety_margin`、`response_rcode`、`rcode`、两种接收窗口，以及 Mihomo 监听器 `port` 等相同联合定义。普通时长字符串及只含整数的原始定义不因字段名称而强制改成其他 Schema 类型。

`query_type` 顶层保留 `integer` / `array` 入口，不将整个字段收窄为整数。其列表项目使用的 `DNSQueryType` 本身属于整数／字符串联合，因此项目编辑器按通用规则固定为整数；已有字符串项目保留，编辑时遵循整数规则。

## 默认值处理

- DNS `predefined` 动作的 `rcode` 默认展示和显式设置使用整数 `0`，对应原来的 `NOERROR`。
- TUN 默认标记保持字符串（如 `0x2023`），与新的标记输入一致。
- 旧的 Naive 窗口默认元数据使用字符串 `15 MB`。整数编辑器不再将其预填成字符串或猜测成某个精确字节整数；未设置时显示空输入，并在帮助中保留文档默认窗口说明，显式填写使用整数字节数。这一项从固定字面值目录移到默认行为说明，目录条目由 90 变为 89。
- 打开表单、预览默认值和切换目标位置不补写数据；恢复、显式设置和用户编辑仍通过现有撤销记录操作。

## 验证

模型遍历当前两种 Schema 的联合定义，检查 sing-box 149 处、Mihomo 8 处纯整数／字符串联合位置均收敛为一个正确类型，保留整数／数组选择；数量包括复用定义及只读节点 Schema，不代表独立可编辑字段数。

原生回归新增实际文本编辑：旧 `5m` 保留显示且不改写；输入整数后保存为 JSON 整数；`5m`、`64 MB`、小数和溢出值不覆盖有效整数；`UInt64.max` 精确保留并支持撤销重做；旧整数标记可查看，编辑 `0x2024` 保存为字符串。既有接收窗口旧值、TLS、动作、引用、条件显现等回归继续执行。

验证命令：`sh scripts/verify-subscription-client.sh`、`sh scripts/verify-template-picker-ui.sh`、`bash scripts/verify-localization.sh`、macOS Debug `xcodebuild`、`git diff --check`。

以上检查全部通过。原生回归已完成真实文本输入的整数序列化、非法输入保护、UInt64 边界、旧值保留、字符串标记及撤销重做验证。
