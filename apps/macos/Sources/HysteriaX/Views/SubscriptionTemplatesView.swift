import SwiftUI
import UniformTypeIdentifiers

struct TemplateTextFile: FileDocument {
    static var readableContentTypes: [UTType] { [.plainText, .json, .yaml] }
    var text: String
    init(_ text: String) { self.text = text }
    init(configuration: ReadConfiguration) throws { text = String(data: configuration.file.regularFileContents ?? Data(), encoding: .utf8) ?? "" }
    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper { FileWrapper(regularFileWithContents: Data(text.utf8)) }
}

struct SubscriptionTemplatesView: View {
    @Bindable var store: ManagementStore
    @State private var model: SubscriptionTemplateListStore
    @State private var selection: String?
    @State private var search = ""
    @State private var format = ""
    @State private var sort = [KeyPathComparator(\SubscriptionTemplate.name)]
    @State private var creating = false
    @State private var name = ""
    @State private var newFormat = "mihomo"
    @State private var page = 1
    @State private var pageSize = 50
    init(store: ManagementStore) { self.store = store; _model = State(initialValue: store.subscriptionTemplateList()) }
    private var rows: [SubscriptionTemplate] {
        let query = search.trimmingCharacters(in: .whitespacesAndNewlines)
        return model.templates.filter { (format.isEmpty || $0.format == format) && (query.isEmpty || $0.name.localizedCaseInsensitiveContains(query)) }.sorted(using: sort)
    }
    private var pageCount: Int { max(1, (rows.count + pageSize - 1) / pageSize) }
    private var currentPage: Int { min(page, pageCount) }
    private var visibleRows: [SubscriptionTemplate] { Array(rows.dropFirst((currentPage - 1) * pageSize).prefix(pageSize)) }
    var body: some View {
        VStack(spacing: 0) {
            templateTable
            Divider()
            paginationControls
        }
        .searchable(text: $search, placement: .toolbar, prompt: L10n.text("搜索模板名称"))
        .toolbar {
            ToolbarItemGroup {
                Picker(L10n.text("配置格式"), selection: $format) {
                    Text(L10n.text("全部")).tag("")
                    Text("Mihomo").tag("mihomo")
                    Text("sing-box").tag("singbox")
                }.accessibilityIdentifier("templates.formatFilter")
                Button(action: openSelected) { Label(L10n.text("编辑"), systemImage: "pencil") }
                    .disabled(selection == nil).keyboardShortcut(.return, modifiers: [])
                    .accessibilityIdentifier("templates.edit")
            }
            ToolbarItem(placement: .primaryAction) {
                Button { name = ""; creating = true } label: { Label(L10n.text("新建模板"), systemImage: "plus") }
                    .disabled(model.busy || !store.isConnected)
                    .keyboardShortcut("n", modifiers: .command)
                    .accessibilityIdentifier("templates.create")
            }
        }
        .accessibilityIdentifier("templates.page")
        .task { refresh() }
        .onChange(of: search) { _, _ in resetPage() }
        .onChange(of: format) { _, _ in resetPage() }
        .onChange(of: sort) { _, _ in resetPage() }
        .onChange(of: pageSize) { _, _ in resetPage() }
        .onChange(of: model.templates.map { $0.id + $0.updatedAt }) { _, _ in
            page = currentPage
            if let selection, !visibleRows.contains(where: { $0.id == selection }) { self.selection = nil }
        }
        .onChange(of: store.serviceAddress) { _, _ in creating = false; resetPage(); model = store.subscriptionTemplateList(); refresh() }
        .onChange(of: store.isConnected) { _, connected in if connected { refresh() } }
        .onChange(of: store.dataRefreshToken) { _, _ in refresh() }
        .onReceive(NotificationCenter.default.publisher(for: .templateListChanged)) { _ in refresh() }
        .sheet(isPresented: $creating) {
            VStack(alignment: .leading, spacing: 16) {
                Text(L10n.text("新建模板")).font(.title2)
                TextField(L10n.text("模板名称"), text: $name)
                Picker(L10n.text("配置格式"), selection: $newFormat) { Text("Mihomo").tag("mihomo"); Text("sing-box").tag("singbox") }
                HStack { Button(L10n.text("取消")) { creating = false }; Spacer(); Button(L10n.text("创建")) {
                    model.run { let item = try await model.create(store.requireConnectedAPI(), name: name, format: newFormat); creating = false; open(item) }
                }.disabled(model.busy || !store.isConnected || name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty) }
            }.padding(24).frame(width: 400)
        }
    }
    private var templateTable: some View {
        Table(visibleRows, selection: $selection, sortOrder: $sort) {
            TableColumn(L10n.text("名称"), value: \SubscriptionTemplate.name) { (item: SubscriptionTemplate) in Text(item.name).accessibilityIdentifier("templates.row.\(item.id)") }
            TableColumn(L10n.text("格式"), value: \.format) { item in Text(item.format == "singbox" ? "sing-box" : "Mihomo") }
            TableColumn(L10n.text("发布版本"), value: \.sortPublishedVersion) { item in Text(item.publishedVersion.map { "v\($0)" } ?? L10n.text("未发布")) }
            TableColumn(L10n.text("编辑状态")) { item in Text(store.templateWindows.status(service: store.serviceAddress, template: item.id)) }
            TableColumn(L10n.text("默认"), value: \.sortDefault) { item in Text(item.isDefault ? L10n.text("默认") : "") }
            TableColumn(L10n.text("更新时间"), value: \.updatedAt) { item in Text(DateDisplayText.local(item.updatedAt)) }
        }
        .frame(minHeight: 180)
        .accessibilityIdentifier("templates.table")
        .contextMenu(forSelectionType: String.self) { ids in
            if let id = ids.first, let item = model.templates.first(where: { $0.id == id }) {
                Button(L10n.text("编辑")) { open(item) }
                Button(L10n.text("复制模板")) { store.templateWindows.open(item, store: store, action: "copy") }
                Button(L10n.text("重命名模板")) { store.templateWindows.open(item, store: store, action: "rename") }
                Button(L10n.text("设为默认模板")) { store.templateWindows.open(item, store: store, action: "default") }.disabled(item.publishedVersion == nil)
                Button(L10n.text("导出完整模板…")) { store.templateWindows.open(item, store: store, action: "export") }
                Button(L10n.text("归档模板…"), role: .destructive) { store.templateWindows.open(item, store: store, action: "archive") }
            }
        } primaryAction: { ids in
            if let id = ids.first, let item = model.templates.first(where: { $0.id == id }) { open(item) }
        }
        .overlay {
            if rows.isEmpty {
                if model.busy { ProgressView() }
                else if model.error == nil {
                    ContentUnavailableView(
                        search.isEmpty && format.isEmpty ? L10n.text("暂无订阅模板") : L10n.text("没有匹配的订阅模板"),
                        systemImage: search.isEmpty && format.isEmpty ? "doc.text" : "magnifyingglass",
                        description: Text(store.isConnected ? L10n.text("新建模板，或调整搜索条件。") : L10n.text("连接服务后可查看订阅模板。"))
                    )
                }
            }
        }
    }
    private var paginationControls: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let error = model.error {
                HStack {
                    Text(error).foregroundStyle(.red).textSelection(.enabled)
                    Button(L10n.text("重试")) { refresh() }.disabled(model.busy || !store.isConnected)
                }.font(.caption)
            }
            HStack(spacing: 12) {
                if !store.isConnected { Label(L10n.text("服务未连接"), systemImage: "wifi.slash").foregroundStyle(.secondary) }
                Text(L10n.text("共 {0} 个模板 · 第 {1} / {2} 页", String(rows.count), String(currentPage), String(pageCount)))
                    .foregroundStyle(.secondary).monospacedDigit()
                if model.busy { ProgressView().controlSize(.small) }
                Spacer(minLength: 0)
                Picker(L10n.text("每页条数"), selection: $pageSize) {
                    ForEach([25, 50, 100], id: \.self) { Text(String($0)).tag($0) }
                }.fixedSize().accessibilityIdentifier("templates.pageSize")
                Button(L10n.text("上一页")) { page = currentPage - 1; selection = nil }
                    .disabled(currentPage <= 1 || model.busy).accessibilityIdentifier("templates.previousPage")
                Button(L10n.text("下一页")) { page = currentPage + 1; selection = nil }
                    .disabled(currentPage >= pageCount || model.busy).accessibilityIdentifier("templates.nextPage")
            }.font(.callout)
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        .accessibilityIdentifier("templates.pagination")
    }
    private func resetPage() { page = 1; selection = nil }
    private func refresh() {
        guard store.isConnected else { return }
        model.run { try await model.loadList(store.requireConnectedAPI()) }
    }
    private func openSelected() { if let item = rows.first(where: { $0.id == selection }) { open(item) } }
    private func open(_ item: SubscriptionTemplate) { store.templateWindows.open(item, store: store) }
}

extension SubscriptionTemplate {
    var sortPublishedVersion: Int { publishedVersion ?? 0 }
    var sortDefault: Int { isDefault ? 1 : 0 }
}
