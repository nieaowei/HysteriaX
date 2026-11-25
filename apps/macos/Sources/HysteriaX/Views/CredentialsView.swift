import SwiftUI

struct CredentialsView: View {
    @Bindable var store: ManagementStore
    var onJump: (String, String) -> Void
    @State private var selection: String?
    @State private var pendingCreatedCA: String?
    @State private var search = ""
    @State private var type = ""
    @State private var category = CredentialCategory.operations
    @State private var showInternalProbes = false
    @State private var creating = false
    @State private var creatingCA = false
    @State private var publishingCA: CredentialDetail?
    @State private var editingCAPolicy: CredentialDetail?
    @State private var replacing: CredentialDetail?
    @State private var editing: CredentialDetail?
    @State private var deletingDetail: CredentialDetail?
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
    private var pageKey: String { store.listPageKey([String(page), String(pageSize), category.apiValue, String(showInternalProbes), type, query, sortField, sortDirection]) }
    private var pageRequestKey: String { "\(pageKey)|\(store.isConnected)|\(store.dataRefreshToken)|\(retryPageToken)" }
    private var currentPageResponse: CredentialsPage? { store.cachedListPage(pageKey, as: CredentialsPage.self) ?? (loadedPageKey == pageKey ? pageResponse : nil) }
    private var totalEntries: Int { store.isConnected ? (currentPageResponse?.total ?? 0) : snapshotEntries.count }
    private var pageCount: Int { max(1, (totalEntries + pageSize - 1) / pageSize) }
    private var currentPage: Int { store.isConnected ? page : min(page, pageCount) }

    private var categoryEntries: [CredentialSummary] {
        store.credentials.filter { category.contains($0) }
    }
    private var snapshotEntries: [CredentialSummary] {
        categoryEntries.filter {
            (showInternalProbes || $0.metadata["internal_probe"]?.boolValue != true) && (type.isEmpty || $0.kind == type) && (query.isEmpty || $0.name.localizedCaseInsensitiveContains(query))
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
            if pendingCreatedCA == nil { type = "" }
            resetPage()
        }
        .onChange(of: search) { _, _ in resetPage() }
        .onChange(of: showInternalProbes) { _, _ in resetPage() }
        .onChange(of: type) { _, _ in resetPage() }
        .onChange(of: sortOrder) { _, _ in resetPage() }
        .onChange(of: pageSize) { _, _ in resetPage() }
        .onChange(of: store.serviceAddress) { _, _ in
            pendingCreatedCA = nil
            type = ""
            resetPage()
            pageResponse = nil
            loadedPageKey = nil
        }
        .task(id: pageRequestKey) { await loadPage() }
        .onDisappear { loadedPageKey = nil }
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
                    ForEach(category.kinds, id: \.self) {
                        Text(CredentialDisplay.kind($0)).tag($0)
                    }
                }
                .accessibilityIdentifier("credentials.typeFilter")
                Menu {
                    Toggle(L10n.text("显示内部探测凭据"), isOn: $showInternalProbes)
                        .accessibilityIdentifier("credentials.showInternalProbes")
                } label: { Label(L10n.text("筛选"), systemImage: "line.3.horizontal.decrease.circle") }
                    .labelStyle(.iconOnly).accessibilityIdentifier("credentials.filters")
                Button { creatingCA = true } label: { Label(L10n.text("创建 CA 身份"), systemImage: "plus.shield") }
                    .labelStyle(.iconOnly).help(L10n.text("创建 CA 身份"))
                    .disabled(!store.isConnected).accessibilityIdentifier("ca.create")
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
        .sheet(isPresented: $creatingCA) {
            CAIdentityEditorView(store: store) { id in
                pendingCreatedCA = id
                category = .operations; type = "ca_identity"; search = ""; resetPage()
            }
        }
        .sheet(item: $publishingCA) { CAIdentityEditorView(store: store, detail: $0, operation: .publish) }
        .sheet(item: $editingCAPolicy) { CAIdentityEditorView(store: store, detail: $0, operation: .policy) }
        .sheet(item: $replacing) { CredentialEditorView(store: store, replacing: $0) }
        .sheet(item: $editing) { CredentialMetadataEditor(store: store, detail: $0) }
        .confirmationDialog(L10n.text("删除未被引用的凭据？"), isPresented: $deleting) {
            Button(L10n.text("删除"), role: .destructive) {
                if let detail = deletingDetail {
                    Task {
                        do {
                            try await store.deleteCredential(detail)
                            selection = nil
                            deletingDetail = nil
                            store.toast.show(L10n.text("凭据已删除"), detail: detail.name, tone: .neutral)
                        } catch { self.error = error.localizedDescription }
                    }
                }
            }
        }
    }

    private var credentialContent: some View {
        VStack(alignment: .leading, spacing: 0) {
            MainVerticalSplitView(hasDetail: selected != nil) {
                VStack(spacing: 0) {
                    Table(entries, selection: $selection, sortOrder: $sortOrder) {
                        TableColumn(L10n.text("名称"), value: \.name)
                        TableColumn(L10n.text("类型"), value: \.typeTitle)
                        TableColumn(L10n.text("状态"), value: \.statusTitle) { entry in
                            StatusBadge(entry.statusTitle, tone: credentialStatusTone(entry.status), size: .compact)
                        }
                        TableColumn(L10n.text("当前引用对象数")) { entry in
                            Text(entry.isManaged || entry.kind == "user_credential" ? String(entry.referenceCount ?? 0) : "—")
                                .monospacedDigit()
                                .help(L10n.text("统计当前引用对象，同一节点只计一次；不含历史配置和待执行更新批次。"))
                        }
                        TableColumn(L10n.text("到期")) { entry in
                            Text(entry.expiresAt.map { DateDisplayText.local($0) } ?? L10n.text("未知"))
                                .monospacedDigit()
                        }
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
                    ListPaginationBar(
                        summary: (currentPageResponse != nil || !store.isConnected)
                            ? L10n.text("共 {0} 项凭据 · 第 {1} / {2} 页", String(totalEntries), String(currentPage), String(pageCount))
                            : nil,
                        page: currentPage,
                        pageCount: pageCount,
                        pageSize: $pageSize,
                        isLoading: isLoadingPage,
                        isOffline: !store.isConnected,
                        error: pageError,
                        onRetry: { loadedPageKey = nil; retryPageToken = UUID() },
                        canNavigate: !store.isConnected || currentPageResponse != nil,
                        identifierPrefix: "credentials",
                        onSelectPage: { changePage($0) }
                    )
                }
            } detail: {
                if let selected {
                    CredentialDetailPane(
                        store: store,
                        entry: selected,
                        onJump: onJump,
                        onPublishCA: { publishingCA = $0 },
                        onEditCAPolicy: { editingCAPolicy = $0 },
                        onReplace: { replacing = $0 },
                        onEdit: { editing = $0 },
                        onDelete: { deletingDetail = $0; deleting = true },
                        onError: { error = $0 },
                        onClose: { clearSelection() }
                    )
                    .id(selected.id)
                    .accessibilityIdentifier("credentials.detail")
                }
            }
            if let error { Text(error).foregroundStyle(.red).padding(12) }
        }
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
            let response = try await store.credentialsPage(page: page, pageSize: pageSize, category: category.apiValue, kind: type, query: query, sort: sortField, order: sortDirection, cacheKey: pageKey, revalidate: loadedPageKey != pageKey, includeInternal: showInternalProbes)
            guard !Task.isCancelled, requestKey == pageRequestKey else { return }
            page = response.page
            pageResponse = response
            loadedPageKey = pageKey
            if let id = pendingCreatedCA {
                if response.items.contains(where: { $0.id == id }) {
                    selection = id
                    pendingCreatedCA = nil
                } else if response.page * response.pageSize < response.total {
                    page = response.page + 1
                } else {
                    pendingCreatedCA = nil
                }
            }
        } catch {
            guard !Task.isCancelled, !isRequestCancellation(error), requestKey == pageRequestKey else { return }
            pageError = error.localizedDescription
        }
    }

    private func clearSelection() {
        selection = nil
        deletingDetail = nil
        error = nil
    }
}

private struct CredentialDetailPane: View {
    @Bindable var store: ManagementStore
    let entry: CredentialSummary
    var onJump: (String, String) -> Void
    var onPublishCA: (CredentialDetail) -> Void
    var onEditCAPolicy: (CredentialDetail) -> Void
    var onReplace: (CredentialDetail) -> Void
    var onEdit: (CredentialDetail) -> Void
    var onDelete: (CredentialDetail) -> Void
    var onError: (String) -> Void
    var onClose: () -> Void

    @State private var detail: CredentialDetail?

    private var detailRequestKey: String {
        "\(store.serviceAddress)|\(entry.id)|\(store.dataRefreshToken)|\(store.isConnected)"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            DetailHeaderBar {
                VStack(alignment: .leading, spacing: 14) {
                    DetailHeaderLayout {
                        detailTitle
                        detailActions
                    }
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), alignment: .leading)], alignment: .leading, spacing: 12) {
                        detailMetric(L10n.text("类型"), value: entry.typeTitle)
                        if entry.isManaged {
                            detailMetric(L10n.text("最新版本"), value: "v\(detail?.latestVersion ?? entry.latestVersion)")
                            detailMetric(L10n.text("引用明细条数"), value: detail.map { String($0.references.count) } ?? "—")
                                .help(L10n.text("包含当前引用、历史配置和待执行更新批次；同一对象的不同引用位置分别计数。"))
                        }
                        detailMetric(entry.kind == "ca_identity" ? L10n.text("当前发布 CA 到期") : L10n.text("到期时间"), value: entry.expiresAt.map { DateDisplayText.local($0) } ?? L10n.text("未知"))
                        if entry.kind == "user_credential" {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(L10n.text("已分配节点数")).font(.caption).foregroundStyle(.secondary)
                                Text(entry.referenceCount.map(String.init) ?? "—").font(.callout).textSelection(.enabled)
                                    .accessibilityIdentifier("credentials.user.nodeCount")
                            }
                        }
                        if let userID = entry.ownerUserId {
                            detailMetric(L10n.text("所属用户"), value: store.user(for: userID)?.name ?? userID)
                        }
                    }
                }
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    if entry.isManaged {
                        if let detail, detail.id == entry.id {
                            CredentialManagedDetailView(detail: detail, isConnected: store.isConnected, onJump: onJump, onRetry: { id in
                                Task {
                                    do {
                                        try await store.retryCredentialBatch(id)
                                        await load()
                                        store.toast.show(L10n.text("已提交重试任务"), tone: .active)
                                    } catch { onError(error.localizedDescription) }
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
                        businessDetail
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(16)
            }
        }
        .task(id: detailRequestKey) { await load() }
    }

    private var detailTitle: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text(entry.name).font(.headline).lineLimit(2).textSelection(.enabled)
            StatusBadge(entry.statusTitle, tone: credentialStatusTone(entry.status))
        }
    }

    private var detailActions: some View {
        HStack(spacing: 8) {
            if let detail, detail.id == entry.id {
                HStack(spacing: 8) {
                    Button(L10n.text("发布新版本")) {
                        if detail.kind == "ca_identity" { onPublishCA(detail) } else { onReplace(detail) }
                    }
                        .disabled(detail.archived || detail.metadata["managed"]?.boolValue == true || detail.ownerUserId != nil)
                    if detail.ca != nil {
                        Button(L10n.text("客户端证书策略")) { onEditCAPolicy(detail) }
                            .disabled(detail.archived).accessibilityIdentifier("ca.policy")
                    }
                    Button(L10n.text("编辑信息")) { onEdit(detail) }
                        .disabled(detail.metadata["managed"]?.boolValue == true || detail.ownerUserId != nil)
                    Button(L10n.text("删除凭据"), role: .destructive) { onDelete(detail) }
                        .foregroundStyle(.red)
                        .disabled(!detail.references.isEmpty)
                        .accessibilityIdentifier("credential.delete")
                }
                .disabled(!store.isConnected)
                Divider().frame(height: 16)
            }
            DetailCloseButton(action: onClose)
        }
        .fixedSize()
    }

    private func detailMetric(_ title: String, value: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            Text(value).font(.callout).textSelection(.enabled)
        }
    }

    @ViewBuilder private var businessDetail: some View {
        if entry.kind == "user_credential" {
            Text(L10n.text("该用户的全部授权节点共用一个连接密码，授权组变更不会轮换密码。"))
                .font(.callout).foregroundStyle(.secondary)
        }
        if let user = entry.ownerUserId {
            GroupBox(L10n.text("用户凭据管理")) {
                HStack {
                    Text(L10n.text("在用户页面管理连接凭据、轮换与订阅。"))
                        .font(.callout).foregroundStyle(.secondary)
                    Spacer()
                    Button(L10n.text("管理用户")) { onJump("user", user) }
                        .accessibilityIdentifier("credentials.manageUser")
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
                    Button(L10n.text("撤销 Token"), role: .destructive) {
                        Task {
                            do {
                                try await store.revokeAdminToken(token)
                                store.toast.show(L10n.text("已撤销管理员 Token"), detail: token.label, tone: .warning)
                            } catch { onError(error.localizedDescription) }
                        }
                    }
                        .disabled(!store.isConnected || token.revokedAt != nil || id == store.currentAdminTokenID)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.vertical, 4)
            }
        }
    }

    private func load() async {
        let requestKey = detailRequestKey
        guard entry.isManaged, store.isConnected else { detail = nil; return }
        if detail?.id != entry.id { detail = nil }
        do {
            let loaded = try await store.credentialDetail(entry.id)
            guard entry.id == loaded.id, !Task.isCancelled, requestKey == detailRequestKey else { return }
            detail = loaded
        } catch {
            guard !Task.isCancelled, requestKey == detailRequestKey else { return }
            onError(error.localizedDescription)
        }
    }
}

private func credentialStatusTone(_ status: String) -> StatusTone {
    switch status {
    case "active": .positive
    case "expiring": .warning
    case "expired", "quota_exhausted": .danger
    default: .neutral
    }
}

private enum CredentialCategory: Hashable {
    case user, operations

    var apiValue: String { self == .user ? "user" : "operations" }
    var title: String { self == .user ? L10n.text("用户凭据") : L10n.text("运营凭据") }

    var kinds: [String] {
        switch self {
        case .operations:
            ["ssh_private_key", "ssh_password", "tls_identity", "ca_identity", "ca_certificate", "ech_key", "dns", "api_token", "admin_token"]
        case .user:
            ["tls_identity", "subscription_token", "user_credential"]
        }
    }

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
                Button(L10n.text("取消")) { dismiss() }.keyboardShortcut(.cancelAction)
                Spacer()
                Button(L10n.text("保存")) { Task {
                    saving = true; defer { saving = false }
                    do { try await store.updateCredential(detail, name: name, archived: archived, reminderAt: hasReminder ? ISO8601DateFormatter().string(from: reminder) : nil); dismiss() }
                    catch { self.error = error.localizedDescription }
                } }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(saving || !store.isConnected)
            }
        }.padding(24).frame(width: 500)
        .onAppear { name = detail.name; archived = detail.archived; hasReminder = detail.reminderAt != nil; reminder = detail.reminderAt.flatMap(DateDisplayText.parse) ?? Date() }
    }
}
