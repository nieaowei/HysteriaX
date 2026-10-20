import SwiftUI

struct SubscriptionTemplateInspector: View {
    @Bindable var store: ManagementStore
    @Bindable var model: SubscriptionTemplateStore
    var export: (String, String) -> Void
    @State private var users: [UserSummary] = []
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                VStack(alignment: .leading, spacing: 6) {
                Text(L10n.text("预览用户")).font(.caption).foregroundStyle(.secondary)
                Picker(L10n.text("预览用户"), selection: $model.previewUserID) {
                    Text(L10n.text("选择用户")).tag("")
                    ForEach(users) { Text($0.name).tag($0.id) }
                }.labelsHidden().frame(maxWidth: .infinity, alignment: .leading).accessibilityIdentifier("templates.preview.user")
                }
                VStack(alignment: .leading, spacing: 6) {
                Text(L10n.text("目标平台")).font(.caption).foregroundStyle(.secondary)
                Picker(L10n.text("目标平台"), selection: $model.previewPlatform) {
                    Text(L10n.text("明确选择目标平台")).tag("")
                    ForEach(TemplatePlatform.all, id: \.self) { Text(TemplatePlatform.title($0)).tag($0) }
                }.labelsHidden().frame(maxWidth: .infinity, alignment: .leading).accessibilityIdentifier("templates.preview.platform")
                }
                TextField(L10n.text("内核版本（可选）"), text: $model.previewKernelVersion).textFieldStyle(.roundedBorder)
                Toggle(L10n.text("显示完整凭据"), isOn: $model.previewReveal)
                Text(L10n.text("平台标签表示字段的适用平台；生成订阅时会自动排除目标平台不支持的字段。"))
                    .font(.caption).foregroundStyle(.secondary)
                if model.previewStale { Text(L10n.text("预览已过期，请重新生成。")).font(.caption).foregroundStyle(.secondary).accessibilityIdentifier("templates.preview.stale") }
                Picker(L10n.text("预览内容"), selection: $model.previewTab) {
                    ForEach(["最终配置", "字段来源", "平台适配"], id: \.self) { Text(L10n.text($0)).tag($0) }
                }.pickerStyle(.segmented)
                if model.previewTab == "最终配置", let source = model.preview["source"]?.stringValue {
                    Text(source).font(.system(.caption, design: .monospaced)).textSelection(.enabled)
                } else {
                    Text(JSONValue.object(model.preview.filter { model.previewTab == "字段来源" ? ["origins", "node_selection", "filtered_nodes"].contains($0.key) : $0.key == "differences" }).pretty).font(.system(.caption, design: .monospaced)).textSelection(.enabled)
                }
                Divider()
                SubscriptionTemplateDiagnostics(store: store, model: model)
            }.frame(maxWidth: .infinity, alignment: .leading).padding(16)
        }.accessibilityIdentifier("templates.inspector")
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                Button(L10n.text("生成预览"), systemImage: "play") {
                    model.run { try await model.render(store.requireConnectedAPI(), user: model.previewUserID, platform: model.previewPlatform, version: model.previewKernelVersion, reveal: model.previewReveal) }
                }
                .labelStyle(.iconOnly).help(L10n.text("生成预览"))
                .disabled(model.busy || model.suspended || model.previewUserID.isEmpty || model.previewPlatform.isEmpty || model.needsPlatformConversion)
                .accessibilityIdentifier("templates.preview")
                Button(model.preview["redacted"]?.boolValue == true ? L10n.text("导出脱敏预览…") : L10n.text("导出最终配置…"), systemImage: "square.and.arrow.up") {
                    guard let source = model.preview["source"]?.stringValue else { return }
                    export(source, "subscription.\(model.selected?.format == "mihomo" ? "yaml" : "json")")
                }
                .labelStyle(.iconOnly)
                .help(model.preview["redacted"]?.boolValue == true ? L10n.text("导出脱敏预览…") : L10n.text("导出最终配置…"))
                .disabled(model.busy || model.previewStale || model.preview["source"]?.stringValue == nil)
                .accessibilityIdentifier("templates.preview.export")
            }
        }
        .task {
            do {
                let api = try store.requireConnectedAPI()
                let items = try await api.collectPages { page in APIEndpoints.listUsers(page: page, pageSize: 200) }
                if !model.suspended && !model.closed && model.connectedAPI === api { users = items }
            }
            catch { if !model.suspended && !model.closed { model.error = error.localizedDescription } }
        }

    }
}
