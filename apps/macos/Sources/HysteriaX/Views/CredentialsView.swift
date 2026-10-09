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

    @State private var page = 1
    @State private var pageSize = 50
    @State private var sortOrder = [KeyPathComparator(\CredentialSummary.name)]
    @State private var pageResponse: CredentialsPage?
    @State private var loadedPageKey: String?
    @State private var isLoadingPage = false
    @State private var pageError: String?
    @State private var retryPageToken = UUID()

    private var query: String { search.trimmingCharacters(in: .whitespacesAndNewlines) }
    private var sortField: String {
        switch sortOrder.first?.keyPath {
        case \CredentialSummary.typeTitle: "kind"
        case \CredentialSummary.statusTitle: "status"
        default: "name"
        }
    }
    private var sortDirection: String { sortOrder.first?.order == .reverse ? "desc" : "asc" }
    private var pageKey: String { "\(store.serviceAddress)|\(page)|\(pageSize)|\(category.apiValue)|\(type)|\(query)|\(sortField)|\(sortDirection)" }
    private var pageRequestKey: String { "\(pageKey)|\(store.isConnected)|\(store.lastUpdated?.timeIntervalSince1970 ?? 0)|\(retryPageToken)" }
    private var currentPageResponse: CredentialsPage? { loadedPageKey == pageKey ? pageResponse : nil }
    private var totalEntries: Int { store.isConnected ? (currentPageResponse?.total ?? 0) : snapshotEntries.count }
    private var pageCount: Int { max(1, (totalEntries + pageSize - 1) / pageSize) }
    private var currentPage: Int { store.isConnected ? page : min(page, pageCount) }
    private var detailRequestKey: String { "\(store.serviceAddress)|\(selection ?? "")|\(store.lastUpdated?.timeIntervalSince1970 ?? 0)|\(store.isConnected)" }

    private var categoryEntries: [CredentialSummary] {
        store.credentials.filter { category.contains($0) }
    }
    private var snapshotEntries: [CredentialSummary] {
        categoryEntries.filter {
            (type.isEmpty || $0.kind == type) && (query.isEmpty || $0.name.localizedCaseInsensitiveContains(query))
        }.sorted(using: sortOrder)
    }
    private var entries: [CredentialSummary] {
        if store.isConnected { return currentPageResponse?.items ?? [] }
        return Array(snapshotEntries.dropFirst((currentPage - 1) * pageSize).prefix(pageSize))
    }
    private var selected: CredentialSummary? { entries.first { $0.id == selection } }

    var body: some View {
        credentialContent
        .searchable(text: $search, placement: .toolbar, prompt: L10n.text("搜索凭据"))
        .onChange(of: category) { _, _ in
            type = ""
            resetPage()
        }
        .onChange(of: search) { _, _ in resetPage() }
        .onChange(of: type) { _, _ in resetPage() }
        .onChange(of: sortOrder) { _, _ in resetPage() }
        .onChange(of: pageSize) { _, _ in resetPage() }
        .onChange(of: store.serviceAddress) { _, _ in
            type = ""
            resetPage()
            pageResponse = nil
            loadedPageKey = nil
        }
        .task(id: pageRequestKey) { await loadPage() }
        .toolbar {
            ToolbarItem(placement: .principal) {
                Picker(L10n.text("凭据分类"), selection: $category) {
                    Text(L10n.text("运营凭据")).tag(CredentialCategory.operations)
                    Text(L10n.text("用户凭据")).tag(CredentialCategory.user)
                }
                .pickerStyle(.segmented)
                .accessibilityIdentifier("credentials.tabs")
            }
            ToolbarItemGroup {
                Picker(L10n.text("类型"), selection: $type) {
                    Text(L10n.text("全部类型")).tag("")
                    ForEach(Array(Set(categoryEntries.map(\.kind))).sorted(), id: \.self) {
                        Text(CredentialDisplay.kind($0)).tag($0)
                    }
                }
                .accessibilityIdentifier("credentials.typeFilter")
                Button { creating = true } label: { Label(L10n.text("创建凭据"), systemImage: "plus") }
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
                resetPage()
            })
        }
        .sheet(item: $replacing) { CredentialEditorView(store: store, replacing: $0) }
        .sheet(item: $editing) { CredentialMetadataEditor(store: store, detail: $0) }
        .task(id: detailRequestKey) { await load() }
        .confirmationDialog(L10n.text("删除未被引用的凭据？"), isPresented: $deleting) {
            Button(L10n.text("删除"), role: .destructive) { if let detail { Task { do { try await store.deleteCredential(detail); selection = nil } catch { self.error = error.localizedDescription } } } }
        }
    }

    private var credentialContent: some View {
        VStack(alignment: .leading, spacing: 0) {
            MainVerticalSplitView(hasDetail: selected != nil) {
                VStack(spacing: 0) {
                    Table(entries, selection: $selection, sortOrder: $sortOrder) {
                        TableColumn(L10n.text("名称"), value: \.name)
                        TableColumn(L10n.text("类型"), value: \.typeTitle)
                        TableColumn(L10n.text("状态"), value: \.statusTitle)
                        TableColumn(L10n.text("当前引用对象数")) { entry in
                            Text(entry.isManaged ? String(entry.referenceCount ?? 0) : "—")
                                .help(L10n.text("统计当前引用对象，同一节点只计一次；不含历史配置和待执行更新批次。"))
                        }
                        TableColumn(L10n.text("到期")) { entry in Text(entry.expiresAt.map { DateDisplayText.local($0) } ?? L10n.text("未知")) }
                    }
                    .frame(minHeight: 180)
                    .accessibilityIdentifier("credentials.table")
                    .overlay {
                        if entries.isEmpty {
                            if isLoadingPage || (store.isConnected && currentPageResponse == nil && pageError == nil) {
                                ProgressView()
                            } else if pageError == nil {
                                ContentUnavailableView(
                                    query.isEmpty && type.isEmpty ? L10n.text("暂无{0}", String(describing: (category.title))) : L10n.text("没有匹配的凭据"),
                                    systemImage: "key.horizontal",
                                    description: Text(query.isEmpty && type.isEmpty
                                        ? (store.isConnected ? L10n.text("当前没有{0}。", String(describing: (category.title))) : L10n.text("连接服务后可查看{0}。", String(describing: (category.title))))
                                        : L10n.text("尝试其他搜索词或类型。"))
                                )
                            }
                        }
                    }
                    Divider()
                    paginationControls
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

    private var paginationControls: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let pageError {
                HStack {
                    Text(pageError).foregroundStyle(.red).textSelection(.enabled)
                    Button(L10n.text("重试")) { retryPageToken = UUID() }
                }.font(.caption)
            }
            HStack(spacing: 12) {
                if !store.isConnected { Label(L10n.text("离线快照"), systemImage: "wifi.slash").foregroundStyle(.secondary) }
                if currentPageResponse != nil || !store.isConnected {
                    Text(L10n.text("共 {0} 项凭据 · 第 {1} / {2} 页", String(totalEntries), String(currentPage), String(pageCount)))
                        .foregroundStyle(.secondary).monospacedDigit()
                }
                if isLoadingPage { ProgressView().controlSize(.small) }
                Spacer(minLength: 0)
                Picker(L10n.text("每页条数"), selection: $pageSize) {
                    ForEach([25, 50, 100], id: \.self) { Text(String($0)).tag($0) }
                }.fixedSize().accessibilityIdentifier("credentials.pageSize")
                Button(L10n.text("上一页")) { changePage(currentPage - 1) }
                    .disabled(currentPage <= 1 || isLoadingPage || (store.isConnected && currentPageResponse == nil))
                    .accessibilityIdentifier("credentials.previousPage")
                Button(L10n.text("下一页")) { changePage(currentPage + 1) }
                    .disabled(currentPage >= pageCount || isLoadingPage || (store.isConnected && currentPageResponse == nil))
                    .accessibilityIdentifier("credentials.nextPage")
            }.font(.callout)
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        .accessibilityIdentifier("credentials.pagination")
    }

    private func changePage(_ page: Int) { self.page = page; clearSelection() }
    private func resetPage() { changePage(1) }

    private func loadPage() async {
        let requestKey = pageRequestKey
        guard store.isConnected else { isLoadingPage = false; pageError = nil; return }
        isLoadingPage = true
        pageError = nil
        defer { if requestKey == pageRequestKey { isLoadingPage = false } }
        do {
            if !query.isEmpty { try await Task.sleep(for: .milliseconds(250)) }
            let response = try await store.credentialsPage(page: page, pageSize: pageSize, category: category.apiValue, kind: type, query: query, sort: sortField, order: sortDirection)
            guard !Task.isCancelled, requestKey == pageRequestKey else { return }
            page = response.page
            pageResponse = response
            loadedPageKey = pageKey
        } catch {
            guard !Task.isCancelled, requestKey == pageRequestKey else { return }
            pageError = error.localizedDescription
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
                DetailHeaderLayout {
                    detailTitle(entry)
                    detailActions
                }
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), alignment: .leading)], alignment: .leading, spacing: 12) {
                    detailMetric(L10n.text("类型"), value: entry.typeTitle)
                    if entry.isManaged {
                        detailMetric(L10n.text("最新版本"), value: "v\(detail?.latestVersion ?? entry.latestVersion)")
                        detailMetric(L10n.text("引用明细条数"), value: detail.map { String($0.references.count) } ?? "—")
                            .help(L10n.text("包含当前引用、历史配置和待执行更新批次；同一对象的不同引用位置分别计数。"))
                    }
                    detailMetric(L10n.text("到期时间"), value: entry.expiresAt.map { DateDisplayText.local($0) } ?? L10n.text("未知"))
                    if let userID = entry.ownerUserId {
                        detailMetric(L10n.text("所属用户"), value: store.users.first { $0.id == userID }?.name ?? userID)
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
                                Text(store.isConnected ? L10n.text("正在读取详情…") : L10n.text("连接服务后可读取引用与版本详情。"))
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
                .foregroundStyle(credentialStatusColor(entry.status))
                .padding(.horizontal, 8).padding(.vertical, 3)
                .background(credentialStatusColor(entry.status).opacity(0.12), in: Capsule())
        }
    }

    private func credentialStatusColor(_ status: String) -> Color {
        switch status {
        case "active": .green
        case "expiring": .orange
        case "expired", "quota_exhausted": .red
        default: .secondary
        }
    }

    @ViewBuilder private var detailActions: some View {
        if let detail, detail.id == selected?.id {
            HStack(spacing: 8) {
                Button(L10n.text("发布新版本")) { replacing = detail }
                    .disabled(detail.archived)
                Button(L10n.text("编辑信息")) { editing = detail }
                Button(L10n.text("删除凭据"), role: .destructive) { deleting = true }
                    .foregroundStyle(.red)
                    .disabled(!detail.references.isEmpty)
                    .accessibilityIdentifier("credential.delete")
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
            GroupBox(L10n.text("用户凭据管理")) {
                HStack {
                    Text(L10n.text("在用户页面管理连接凭据、轮换与订阅。"))
                        .font(.callout).foregroundStyle(.secondary)
                    Spacer()
                    Button(L10n.text("管理用户")) { onJump("user", user) }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.vertical, 4)
            }
        }
        if entry.kind == "admin_token", let id = entry.metadata["token_id"]?.stringValue,
           let token = store.adminTokens.first(where: { $0.id == id }) {
            GroupBox(L10n.text("管理员访问")) {
                HStack(alignment: .center, spacing: 16) {
                    VStack(alignment: .leading, spacing: 8) {
                        Text(id == store.currentAdminTokenID ? L10n.text("本 Mac 当前使用的 Token") : L10n.text("管理员访问令牌")).foregroundStyle(.secondary)
                        if let lastUsed = token.lastUsedAt { detailMetric(L10n.text("最近使用"), value: DateDisplayText.local(lastUsed)) }
                    }
                    Spacer()
                    Button(L10n.text("撤销 Token"), role: .destructive) { Task { do { try await store.revokeAdminToken(token) } catch { self.error = error.localizedDescription } } }
                        .disabled(!store.isConnected || token.revokedAt != nil || id == store.currentAdminTokenID)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.vertical, 4)
            }
        }
    }

    private func load() async {
        let requestKey = detailRequestKey
        guard let selected, selected.isManaged, store.isConnected else { detail = nil; return }
        if detail?.id != selected.id { detail = nil }
        do {
            let loaded = try await store.credentialDetail(selected.id)
            guard selection == loaded.id, !Task.isCancelled, requestKey == detailRequestKey else { return }
            detail = loaded; error = nil
        } catch {
            guard selection == selected.id, !Task.isCancelled, requestKey == detailRequestKey else { return }
            self.error = error.localizedDescription
        }
    }
}

private enum CredentialCategory: Hashable {
    case user, operations

    var apiValue: String { self == .user ? "user" : "operations" }
    var title: String { self == .user ? L10n.text("用户凭据") : L10n.text("运营凭据") }

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
            Text(L10n.text("凭据信息")).font(.title2.bold())
            TextField(L10n.text("名称"), text: $name)
            Toggle(L10n.text("归档（保留已有引用，禁止新增引用）"), isOn: $archived)
            Toggle(L10n.text("设置到期提醒"), isOn: $hasReminder)
            if hasReminder { DatePicker(L10n.text("提醒时间"), selection: $reminder) }
            if let error { Text(error).foregroundStyle(.red) }
            HStack {
                Button(L10n.text("取消")) { dismiss() }; Spacer()
                Button(L10n.text("保存")) { Task {
                    saving = true; defer { saving = false }
                    do { try await store.updateCredential(detail, name: name, archived: archived, reminderAt: hasReminder ? ISO8601DateFormatter().string(from: reminder) : nil); dismiss() }
                    catch { self.error = error.localizedDescription }
                } }.disabled(saving || !store.isConnected)
            }
        }.padding(24).frame(width: 500)
        .onAppear { name = detail.name; archived = detail.archived; hasReminder = detail.reminderAt != nil; reminder = detail.reminderAt.flatMap(DateDisplayText.parse) ?? Date() }
    }
}
