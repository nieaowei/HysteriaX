import AppKit
import SwiftUI

struct UserSubscriptionTemplateSheet: View {
    @Bindable var store: ManagementStore
    let user: UserSummary
    @Environment(\.dismiss) private var dismiss
    @State private var templates: [SubscriptionTemplate] = []
    @State private var templateID = ""
    @AppStorage("subscriptionTemplates.userTargetPlatform") private var platform = ""
    @State private var error: String?
    @State private var status = ""
    @State private var busy = false
    @State private var exporting = false
    @State private var file = TemplateTextFile("")
    private var selected: SubscriptionTemplate? { templates.first { $0.id == templateID } }
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(L10n.text("订阅模板与目标平台")).font(.title2)
            Text(user.name).foregroundStyle(.secondary)
            Picker(L10n.text("选择模板"), selection: $templateID) {
                Text(L10n.text("选择模板")).tag("")
                ForEach(templates) { Text("\($0.name) (\($0.format))").tag($0.id) }
            }.accessibilityIdentifier("users.subscription.template")
            Picker(L10n.text("目标平台"), selection: $platform) {
                Text(L10n.text("明确选择目标平台")).tag("")
                ForEach(TemplatePlatform.all, id: \.self) { Text(TemplatePlatform.title($0)).tag($0) }
            }.accessibilityIdentifier("users.subscription.platform")
            Text(L10n.text("地址始终使用所选模板的最新发布版本。此选择不绑定用户或授权组。"))
                .font(.caption).foregroundStyle(.secondary)
            if let error { Text(error).foregroundStyle(.red) }
            if !status.isEmpty { Text(status).foregroundStyle(.secondary) }
            HStack {
                Button(L10n.text("完成")) { dismiss() }.keyboardShortcut(.cancelAction)
                Spacer()
                Button(L10n.text("复制订阅地址")) { run {
                    guard let selected, let format = SubscriptionFileFormat(rawValue: selected.format) else { return }
                    let url = try await store.currentSubscriptionURL(for: user, format: format, template: selected.id, platform: platform)
                    NSPasteboard.general.clearContents(); NSPasteboard.general.setString(url, forType: .string); status = L10n.text("订阅地址已复制。")
                } }.disabled(selected == nil || platform.isEmpty).accessibilityIdentifier("users.subscription.copyResolved")
                Button(L10n.text("导出最终配置…")) { run {
                    guard let selected, let format = SubscriptionFileFormat(rawValue: selected.format) else { return }
                    let data = try await store.subscriptionFile(for: user, format: format, template: selected.id, platform: platform)
                    file = TemplateTextFile(String(decoding: data, as: UTF8.self)); exporting = true
                } }.disabled(selected == nil || platform.isEmpty).accessibilityIdentifier("users.subscription.exportResolved")
            }
        }.padding(24).frame(width: 580).disabled(busy)
        .task { run {
            let all: [SubscriptionTemplate] = try await store.requireConnectedAPI().collectPages { page in APIEndpoints.listSubscriptionTemplates(page: page, pageSize: 200) }
            templates = all.filter { $0.publishedVersion != nil }
        } }
        .fileExporter(isPresented: $exporting, document: file, contentType: selected?.format == "mihomo" ? .yaml : .json, defaultFilename: "\(user.name).\(selected?.format == "mihomo" ? "yaml" : "json")") { result in
            if case .failure(let failure) = result { error = failure.localizedDescription }
        }
    }
    private func run(_ action: @escaping @MainActor () async throws -> Void) {
        busy = true; error = nil
        Task { defer { busy = false }; do { try await action() } catch { self.error = error.localizedDescription } }
    }
}
