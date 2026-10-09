import SwiftUI

struct AuditView: View {
    @Bindable var store: ManagementStore
    @State private var searchText = ""
    @State private var sortOrder = [KeyPathComparator<AuditSummary>(\.createdAt, order: .reverse)]
    @State private var page = 1
    @State private var pageSize = 50
    @State private var pageResponse: AuditPage?
    @State private var loadedPageKey: String?
    @State private var isLoadingPage = false
    @State private var pageError: String?
    @State private var retryPageToken = UUID()

    private var query: String { searchText.trimmingCharacters(in: .whitespacesAndNewlines) }
    private var sortField: String {
        switch sortOrder.first?.keyPath {
        case \AuditSummary.localizedAction: "action"
        case \AuditSummary.localizedEntityType: "entity_type"
        case \AuditSummary.entityID: "entity_id"
        case \AuditSummary.localizedActor: "actor"
        default: "created_at"
        }
    }
    private var sortDirection: String { sortOrder.first?.order == .forward ? "asc" : "desc" }
    private var pageKey: String { "\(store.serviceAddress)|\(page)|\(pageSize)|\(query)|\(sortField)|\(sortDirection)|\(L10n.locale.identifier)" }
    private var pageRequestKey: String { "\(pageKey)|\(store.isConnected)|\(store.lastUpdated?.timeIntervalSince1970 ?? 0)|\(retryPageToken)" }
    private var currentPageResponse: AuditPage? { loadedPageKey == pageKey ? pageResponse : nil }
    private var totalRecords: Int { store.isConnected ? (currentPageResponse?.total ?? 0) : snapshotRecords.count }
    private var pageCount: Int { max(1, (totalRecords + pageSize - 1) / pageSize) }
    private var currentPage: Int { store.isConnected ? page : min(page, pageCount) }

    private var snapshotRecords: [AuditSummary] {
        store.auditRecords.filter { record in
            query.isEmpty || [record.id, record.localizedAction, record.localizedEntityType, record.localizedActor, record.action, record.actor, record.entityType, record.entityID]
                .contains { $0.localizedCaseInsensitiveContains(query) }
        }.sorted(using: sortOrder)
    }

    private var visibleRecords: [AuditSummary] {
        if store.isConnected { return currentPageResponse?.items ?? [] }
        return Array(snapshotRecords.dropFirst((currentPage - 1) * pageSize).prefix(pageSize))
    }

    var body: some View {
        VStack(spacing: 0) {
            Table(visibleRecords, sortOrder: $sortOrder) {
                TableColumn(L10n.text("操作"), value: \.localizedAction)
                TableColumn(L10n.text("对象"), value: \.localizedEntityType)
                TableColumn(L10n.text("对象 ID"), value: \.entityID)
                TableColumn(L10n.text("操作者"), value: \.localizedActor)
                TableColumn(L10n.text("时间"), value: \.createdAt) { record in Text(DateDisplayText.local(record.createdAt)) }
            }
            .accessibilityIdentifier("audit.table")
            .overlay {
                if visibleRecords.isEmpty {
                    if isLoadingPage || (store.isConnected && currentPageResponse == nil && pageError == nil) {
                        ProgressView()
                    } else if pageError == nil {
                        if query.isEmpty {
                            ContentUnavailableView(L10n.text("暂无审计记录"), systemImage: "clock.arrow.circlepath")
                        } else {
                            ContentUnavailableView(L10n.text("没有匹配的审计记录"), systemImage: "magnifyingglass")
                        }
                    }
                }
            }
            Divider()
            paginationControls
        }
        .searchable(text: $searchText, prompt: L10n.text("搜索审计"))
        .onChange(of: searchText) { _, _ in page = 1 }
        .onChange(of: sortOrder) { _, _ in page = 1 }
        .onChange(of: pageSize) { _, _ in page = 1 }
        .onChange(of: L10n.locale.identifier) { _, _ in page = 1 }
        .onChange(of: store.serviceAddress) { _, _ in
            page = 1
            pageResponse = nil
            loadedPageKey = nil
        }
        .task(id: pageRequestKey) { await loadPage() }
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
                if !store.isConnected {
                    Label(L10n.text("离线快照"), systemImage: "wifi.slash").foregroundStyle(.secondary)
                }
                if currentPageResponse != nil || !store.isConnected {
                    Text(L10n.text("共 {0} 条审计记录 · 第 {1} / {2} 页", String(totalRecords), String(currentPage), String(pageCount)))
                        .foregroundStyle(.secondary).monospacedDigit()
                }
                if isLoadingPage { ProgressView().controlSize(.small) }
                Spacer(minLength: 0)
                Picker(L10n.text("每页条数"), selection: $pageSize) {
                    ForEach([25, 50, 100], id: \.self) { Text(String($0)).tag($0) }
                }.fixedSize().accessibilityIdentifier("audit.pageSize")
                Button(L10n.text("上一页")) { page = currentPage - 1 }
                    .disabled(currentPage <= 1 || isLoadingPage || (store.isConnected && currentPageResponse == nil))
                    .accessibilityIdentifier("audit.previousPage")
                Button(L10n.text("下一页")) { page = currentPage + 1 }
                    .disabled(currentPage >= pageCount || isLoadingPage || (store.isConnected && currentPageResponse == nil))
                    .accessibilityIdentifier("audit.nextPage")
            }.font(.callout)
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        .accessibilityIdentifier("audit.pagination")
    }

    private func loadPage() async {
        let requestKey = pageRequestKey
        guard store.isConnected else { isLoadingPage = false; pageError = nil; return }
        isLoadingPage = true
        pageError = nil
        defer { if requestKey == pageRequestKey { isLoadingPage = false } }
        do {
            if !query.isEmpty { try await Task.sleep(for: .milliseconds(250)) }
            let response = try await store.auditPage(page: page, pageSize: pageSize, query: query, sort: sortField, order: sortDirection)
            guard !Task.isCancelled, requestKey == pageRequestKey else { return }
            page = response.page
            pageResponse = response
            loadedPageKey = pageKey
        } catch {
            guard !Task.isCancelled, requestKey == pageRequestKey else { return }
            pageError = error.localizedDescription
        }
    }
}

private extension AuditSummary {
    var localizedAction: String { AuditDisplayText.action(action) }
    var localizedEntityType: String { AuditDisplayText.entityType(entityType) }
    var localizedActor: String { AuditDisplayText.actor(actor) }
}
