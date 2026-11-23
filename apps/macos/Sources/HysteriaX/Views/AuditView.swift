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
    private var pageKey: String { store.listPageKey([String(page), String(pageSize), query, sortField, sortDirection]) }
    private var pageRequestKey: String { "\(pageKey)|\(store.isConnected)|\(store.dataRefreshToken)|\(retryPageToken)" }
    private var currentPageResponse: AuditPage? { store.cachedListPage(pageKey, as: AuditPage.self) ?? (loadedPageKey == pageKey ? pageResponse : nil) }
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
                TableColumn(L10n.text("对象 ID"), value: \.entityID) { record in
                    Text(record.entityID)
                        .font(.callout.monospaced())
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .textSelection(.enabled)
                }
                TableColumn(L10n.text("操作者"), value: \.localizedActor)
                TableColumn(L10n.text("时间"), value: \.createdAt) { record in
                    Text(DateDisplayText.local(record.createdAt))
                        .monospacedDigit()
                }
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
            ListPaginationBar(
                summary: (currentPageResponse != nil || !store.isConnected)
                    ? L10n.text("共 {0} 条审计记录 · 第 {1} / {2} 页", String(totalRecords), String(currentPage), String(pageCount))
                    : nil,
                page: currentPage,
                pageCount: pageCount,
                pageSize: $pageSize,
                isLoading: isLoadingPage,
                isOffline: !store.isConnected,
                error: pageError,
                onRetry: { loadedPageKey = nil; retryPageToken = UUID() },
                canNavigate: !store.isConnected || currentPageResponse != nil,
                identifierPrefix: "audit",
                onSelectPage: { page = $0 }
            )
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
        .onDisappear { loadedPageKey = nil }
    }

    private func loadPage() async {
        let requestKey = pageRequestKey
        guard store.isConnected else { isLoadingPage = false; pageError = nil; return }
        isLoadingPage = true
        pageError = nil
        defer { if requestKey == pageRequestKey { isLoadingPage = false } }
        do {
            if !query.isEmpty { try await Task.sleep(for: .milliseconds(250)) }
            let response = try await store.auditPage(page: page, pageSize: pageSize, query: query, sort: sortField, order: sortDirection, cacheKey: pageKey, revalidate: loadedPageKey != pageKey)
            guard !Task.isCancelled, requestKey == pageRequestKey else { return }
            page = response.page
            pageResponse = response
            loadedPageKey = pageKey
        } catch {
            guard !Task.isCancelled, !isRequestCancellation(error), requestKey == pageRequestKey else { return }
            pageError = error.localizedDescription
        }
    }
}

private extension AuditSummary {
    var localizedAction: String { AuditDisplayText.action(action) }
    var localizedEntityType: String { AuditDisplayText.entityType(entityType) }
    var localizedActor: String { AuditDisplayText.actor(actor) }
}
