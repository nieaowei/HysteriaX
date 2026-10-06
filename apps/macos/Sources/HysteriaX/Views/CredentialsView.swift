import SwiftUI

struct CredentialsView: View {
    @Bindable var store: ManagementStore
    var onJump: (String, String) -> Void
    @State private var selection: String?
    @State private var search = ""
    @State private var type = ""
    @State private var category = CredentialCategory.operations
    @State private var detail: CredentialDetail?
    @State private var creating = false
    @State private var replacing: CredentialDetail?
    @State private var editing: CredentialDetail?
    @State private var error: String?
    @State private var deleting = false

    private var categoryEntries: [CredentialSummary] {
        store.credentials.filter { category.contains($0) }
    }
    private var entries: [CredentialSummary] {
        let query = search.trimmingCharacters(in: .whitespacesAndNewlines)
        return categoryEntries.filter {
            (type.isEmpty || $0.kind == type) && (query.isEmpty || $0.name.localizedCaseInsensitiveContains(query))
        }
    }
    private var selected: CredentialSummary? { entries.first { $0.id == selection } }

    var body: some View {
        credentialContent
        .searchable(text: $search, placement: .toolbar, prompt: "搜索凭据")
        .onChange(of: category) { _, _ in
            type = ""
            clearSelection()
        }
        .onChange(of: search) { _, _ in clearSelection() }
        .onChange(of: type) { _, _ in clearSelection() }
        .toolbar {
            ToolbarItem(placement: .principal) {
                Picker("凭据分类", selection: $category) {
                    Text("运营凭据").tag(CredentialCategory.operations)
                    Text("用户凭据").tag(CredentialCategory.user)
                }
                .pickerStyle(.segmented)
                .accessibilityIdentifier("credentials.tabs")
            }
            ToolbarItemGroup {
                Picker("类型", selection: $type) {
                    Text("全部类型").tag("")
                    ForEach(Array(Set(categoryEntries.map(\.kind))).sorted(), id: \.self) {
                        Text(CredentialDisplay.kind($0)).tag($0)
                    }
                }
                .accessibilityIdentifier("credentials.typeFilter")
                Button { creating = true } label: { Label("创建凭据", systemImage: "plus") }
                    .disabled(!store.isConnected).keyboardShortcut("n", modifiers: .command)
                    .accessibilityIdentifier("credential.create")
            }
        }
        .sheet(isPresented: $creating) {
            CredentialEditorView(store: store, onCreated: { receipt in
                if let entry = store.credentials.first(where: { $0.id == receipt.id }) {
                    category = entry.ownerUserId == nil ? .operations : .user
                }
                search = ""
                type = ""
                clearSelection()
            })
        }
        .sheet(item: $replacing) { CredentialEditorView(store: store, replacing: $0) }
        .sheet(item: $editing) { CredentialMetadataEditor(store: store, detail: $0) }
        .task(id: "\(selection ?? ""): \(store.lastUpdated?.timeIntervalSince1970 ?? 0):\(store.isConnected)") { await load() }
        .confirmationDialog("删除未被引用的凭据？", isPresented: $deleting) {
            Button("删除", role: .destructive) { if let detail { Task { do { try await store.deleteCredential(detail); selection = nil } catch { self.error = error.localizedDescription } } } }
        }
    }

    private var credentialContent: some View {
        VStack(alignment: .leading, spacing: 0) {
            MainVerticalSplitView(hasDetail: selected != nil) {
                Table(entries, selection: $selection) {
                    TableColumn("名称", value: \.name)
                    TableColumn("类型", value: \.typeTitle)
                    TableColumn("状态", value: \.statusTitle)
                    TableColumn("引用") { entry in Text(entry.isManaged ? String(entry.referenceCount ?? 0) : "—") }
                    TableColumn("到期") { entry in Text(entry.expiresAt.map { DateDisplayText.local($0) } ?? "未知") }
                }
                .frame(minHeight: 180)
                .accessibilityIdentifier("credentials.table")
                .overlay {
                    if entries.isEmpty {
                        ContentUnavailableView(
                            categoryEntries.isEmpty ? "暂无\(category.title)" : "没有匹配的凭据",
                            systemImage: "key.horizontal",
                            description: Text(categoryEntries.isEmpty
                                ? (store.isConnected ? "当前没有\(category.title)。" : "连接服务后可查看\(category.title)。")
                                : "尝试其他搜索词或类型。")
                        )
                    }
                }
            } detail: {
                if let selected {
                    credentialDetailPane(selected)
                        .id(selected.id)
                        .accessibilityIdentifier("credentials.detail")
                }
            }
            if let error { Text(error).foregroundStyle(.red).padding(12) }
        }
    }

    private func clearSelection() {
        selection = nil
        detail = nil
        error = nil
    }

    private func credentialDetailPane(_ entry: CredentialSummary) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 14) {
                ViewThatFits(in: .horizontal) {
                    HStack(alignment: .top, spacing: 16) {
                        detailTitle(entry)
                        Spacer(minLength: 20)
                        detailActions
                    }
                    VStack(alignment: .leading, spacing: 10) {
                        detailTitle(entry)
                        detailActions
                    }
                }
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), alignment: .leading)], alignment: .leading, spacing: 12) {
                    detailMetric("类型", value: entry.typeTitle)
                    if entry.isManaged {
                        detailMetric("最新版本", value: "v\(detail?.latestVersion ?? entry.latestVersion)")
                        detailMetric("引用", value: String(detail?.references.count ?? entry.referenceCount ?? 0))
                    }
                    detailMetric("到期时间", value: entry.expiresAt.map { DateDisplayText.local($0) } ?? "未知")
                    if let userID = entry.ownerUserId {
                        detailMetric("所属用户", value: store.users.first { $0.id == userID }?.name ?? userID)
                    }
                }
            }
            .padding(16)
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    if entry.isManaged {
                        if let detail, detail.id == entry.id {
                            CredentialManagedDetailView(detail: detail, isConnected: store.isConnected, onJump: onJump, onRetry: { id in
                                Task {
                                    do { try await store.retryCredentialBatch(id); await load() }
                                    catch { self.error = error.localizedDescription }
                                }
                            })
                        } else {
                            HStack(spacing: 8) {
                                if store.isConnected { ProgressView().controlSize(.small) }
                                Text(store.isConnected ? "正在读取详情…" : "连接服务后可读取引用与版本详情。")
                                    .foregroundStyle(.secondary)
                            }
                        }
                    } else {
                        businessDetail(entry)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(16)
            }
        }
    }

    private func detailTitle(_ entry: CredentialSummary) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text(entry.name).font(.headline).lineLimit(2).textSelection(.enabled)
            Text(entry.statusTitle)
                .font(.caption.weight(.medium))
                .padding(.horizontal, 8).padding(.vertical, 3)
                .background(.quaternary, in: Capsule())
        }
    }

    @ViewBuilder private var detailActions: some View {
        if let detail, detail.id == selected?.id {
            HStack(spacing: 8) {
                Button("发布新版本") { replacing = detail }
                    .disabled(detail.archived)
                Button("编辑信息") { editing = detail }
                Menu {
                    Button("删除凭据", role: .destructive) { deleting = true }
                        .disabled(!detail.references.isEmpty)
                } label: { Image(systemName: "ellipsis") }
                .menuIndicator(.hidden)
                .help("更多操作")
                .accessibilityLabel("更多凭据操作")
            }
            .fixedSize()
            .disabled(!store.isConnected)
        }
    }

    private func detailMetric(_ title: String, value: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            Text(value).font(.callout).textSelection(.enabled)
        }
    }

    @ViewBuilder private func businessDetail(_ entry: CredentialSummary) -> some View {
        if let user = entry.ownerUserId {
            GroupBox("用户凭据管理") {
                HStack {
                    Text("在用户页面管理连接凭据、轮换与订阅。")
                        .font(.callout).foregroundStyle(.secondary)
                    Spacer()
                    Button("管理用户") { onJump("user", user) }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.vertical, 4)
            }
        }
        if entry.kind == "admin_token", let id = entry.metadata["token_id"]?.stringValue,
           let token = store.adminTokens.first(where: { $0.id == id }) {
            GroupBox("管理员访问") {
                HStack(alignment: .center, spacing: 16) {
                    VStack(alignment: .leading, spacing: 8) {
                        Text(id == store.currentAdminTokenID ? "本 Mac 当前使用的 Token" : "管理员访问令牌").foregroundStyle(.secondary)
                        if let lastUsed = token.lastUsedAt { detailMetric("最近使用", value: DateDisplayText.local(lastUsed)) }
                    }
                    Spacer()
                    Button("撤销 Token", role: .destructive) { Task { do { try await store.revokeAdminToken(token) } catch { self.error = error.localizedDescription } } }
                        .disabled(!store.isConnected || token.revokedAt != nil || id == store.currentAdminTokenID)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.vertical, 4)
            }
        }
    }

    private func load() async {
        guard let selected, selected.isManaged, store.isConnected else { detail = nil; return }
        if detail?.id != selected.id { detail = nil }
        do {
            let loaded = try await store.credentialDetail(selected.id)
            guard selection == loaded.id else { return }
            detail = loaded; error = nil
        } catch {
            guard selection == selected.id, !Task.isCancelled else { return }
            self.error = error.localizedDescription
        }
    }
}

private enum CredentialCategory: Hashable {
    case user, operations

    var title: String { self == .user ? "用户凭据" : "运营凭据" }

    func contains(_ entry: CredentialSummary) -> Bool {
        (entry.ownerUserId != nil) == (self == .user)
    }
}

private struct CredentialMetadataEditor: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ManagementStore
    let detail: CredentialDetail
    @State private var name = ""
    @State private var archived = false
    @State private var hasReminder = false
    @State private var reminder = Date()
    @State private var error: String?
    @State private var saving = false
    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("凭据信息").font(.title2.bold())
            TextField("名称", text: $name)
            Toggle("归档（保留已有引用，禁止新增引用）", isOn: $archived)
            Toggle("设置到期提醒", isOn: $hasReminder)
            if hasReminder { DatePicker("提醒时间", selection: $reminder) }
            if let error { Text(error).foregroundStyle(.red) }
            HStack {
                Button("取消") { dismiss() }; Spacer()
                Button("保存") { Task {
                    saving = true; defer { saving = false }
                    do { try await store.updateCredential(detail, name: name, archived: archived, reminderAt: hasReminder ? ISO8601DateFormatter().string(from: reminder) : nil); dismiss() }
                    catch { self.error = error.localizedDescription }
                } }.disabled(saving || !store.isConnected)
            }
        }.padding(24).frame(width: 500)
        .onAppear { name = detail.name; archived = detail.archived; hasReminder = detail.reminderAt != nil; reminder = detail.reminderAt.flatMap(DateDisplayText.parse) ?? Date() }
    }
}
