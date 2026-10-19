import SwiftUI

struct SubscriptionTemplateInspector: View {
    @Bindable var store: ManagementStore
    @Bindable var model: SubscriptionTemplateStore
    var export: (String, String) -> Void
    var showsTitle = true
    @State private var users: [UserSummary] = []
    @State private var userID = ""
    @AppStorage("subscriptionTemplates.previewPlatform") private var platform = ""
    @State private var version = ""
    @State private var reveal = false
    @State private var rollbackVersion: Int?
    @State private var confirmRollback = false
    @State private var historyExpanded = false
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                if showsTitle { Text(L10n.text("预览与诊断")).font(.headline) }
                VStack(alignment: .leading, spacing: 6) {
                Text(L10n.text("预览用户")).font(.caption).foregroundStyle(.secondary)
                Picker(L10n.text("预览用户"), selection: $userID) {
                    Text(L10n.text("选择用户")).tag("")
                    ForEach(users) { Text($0.name).tag($0.id) }
                }.labelsHidden().frame(maxWidth: .infinity, alignment: .leading).accessibilityIdentifier("templates.preview.user")
                }
                VStack(alignment: .leading, spacing: 6) {
                Text(L10n.text("目标平台")).font(.caption).foregroundStyle(.secondary)
                Picker(L10n.text("目标平台"), selection: $platform) {
                    Text(L10n.text("明确选择目标平台")).tag("")
                    ForEach(TemplatePlatform.all, id: \.self) { Text(TemplatePlatform.title($0)).tag($0) }
                }.labelsHidden().frame(maxWidth: .infinity, alignment: .leading).accessibilityIdentifier("templates.preview.platform")
                }
                TextField(L10n.text("内核版本（可选）"), text: $version).textFieldStyle(.roundedBorder)
                Toggle(L10n.text("显示完整凭据"), isOn: $reveal)
                Button(L10n.text("生成预览")) { model.run { try await model.render(store.requireConnectedAPI(), user: userID, platform: platform, version: version, reveal: reveal) } }
                    .disabled(userID.isEmpty || platform.isEmpty).accessibilityIdentifier("templates.preview")
                Text(L10n.text("校验覆盖配置结构和基线平台能力，不代表已验证所有平台的真实运行。"))
                    .font(.caption).foregroundStyle(.secondary)
                if let source = model.preview["source"]?.stringValue {
                    DisclosureGroup(L10n.text("最终配置")) {
                        Text(source).font(.system(.caption, design: .monospaced)).textSelection(.enabled)
                        Button(model.preview["redacted"]?.boolValue == true ? L10n.text("导出脱敏预览…") : L10n.text("导出最终配置…")) { export(source, "subscription.\(model.selected?.format == "mihomo" ? "yaml" : "json")") }
                    }
                    DisclosureGroup(L10n.text("字段来源与平台差异")) {
                        Text(JSONValue.object(model.preview.filter { ["origins", "differences", "node_selection", "filtered_nodes"].contains($0.key) }).pretty)
                            .font(.system(.caption, design: .monospaced)).textSelection(.enabled)
                    }
                }
                Divider()
                ForEach(Array(model.diagnostics.enumerated()), id: \.offset) { _, diagnostic in
                    VStack(alignment: .leading, spacing: 3) {
                        Button("\(diagnostic["layer"].stringValue ?? "")\(diagnostic["path"].stringValue ?? "")") { model.run { try await model.focus(diagnostic, api: store.requireConnectedAPI()) } }
                            .font(.system(.caption, design: .monospaced)).buttonStyle(.link).accessibilityLabel(L10n.text("定位诊断字段"))
                        Text(diagnostic["message"].stringValue ?? "")
                    }.foregroundStyle(diagnostic["severity"].stringValue == "error" ? .red : .orange)
                }
                if let comparison = model.comparison { DisclosureGroup(L10n.text("历史版本与当前草稿")) { Text(comparison.pretty).font(.system(.caption, design: .monospaced)).textSelection(.enabled); Divider(); Text(model.document.pretty).font(.system(.caption, design: .monospaced)).textSelection(.enabled) } }
                Divider()
                Button { historyExpanded.toggle() } label: {
                    Label(L10n.text("发布历史（{0}）", String(model.history.count)), systemImage: historyExpanded ? "chevron.down" : "chevron.right")
                        .frame(maxWidth: .infinity, alignment: .leading).contentShape(Rectangle())
                }.buttonStyle(.plain).accessibilityIdentifier("templates.history.toggle")
                if historyExpanded {
                    ForEach(model.history, id: \.pretty) { entry in
                        HStack {
                            Text("v\(entry["version"].integerValue ?? 0)").monospacedDigit()
                            Text(entry["created_at"].stringValue ?? "").font(.caption).foregroundStyle(.secondary)
                            Spacer()
                            Button(L10n.text("对比")) { model.run { try await model.compare(store.requireConnectedAPI(), version: entry["version"].integerValue ?? 0) } }
                            Button(L10n.text("回滚")) { rollbackVersion = entry["version"].integerValue; confirmRollback = true }.accessibilityIdentifier("templates.rollback.\(entry["version"].integerValue ?? 0)")
                        }
                    }
                }
            }.frame(maxWidth: .infinity, alignment: .leading).padding(16)
        }.accessibilityIdentifier("templates.inspector")
        .onChange(of: userID) { _, _ in model.preview = [:] }
        .onChange(of: platform) { _, _ in model.preview = [:] }
        .onChange(of: version) { _, _ in model.preview = [:] }
        .onChange(of: reveal) { _, _ in model.preview = [:] }
        .onChange(of: model.source) { _, _ in model.preview = [:] }
        .task {
            do { users = try await store.requireConnectedAPI().collectPages { page in APIEndpoints.listUsers(page: page, pageSize: 200) } }
            catch { model.error = error.localizedDescription }
        }
        .confirmationDialog(L10n.text("回滚会重新校验历史内容并发布为新版本，当前未保存修改将被替换。"), isPresented: $confirmRollback) {
            Button(L10n.text("回滚并发布"), role: .destructive) { model.run { if let rollbackVersion { try await model.rollback(store.requireConnectedAPI(), version: rollbackVersion) } } }
            Button(L10n.text("取消"), role: .cancel) {}
        }
    }
}
