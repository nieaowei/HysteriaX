import SwiftUI

struct DNSRecordsView: View {
    @Bindable var store: ManagementStore
    var onOpenNode: (String) -> Void = { _ in }
    @State private var selection: String?
    @State private var connectionID = ""
    @State private var zoneID = ""
    @State private var search = ""
    @State private var showingConnections = false
    @State private var creating = false
    @State private var editing: DNSRecord?
    @State private var deleting: DNSRecord?
    @State private var error: String?
    @State private var busy = false
    @State private var sortOrder = [KeyPathComparator(\DNSRecord.name)]

    @State private var page = 1
    @State private var pageSize = 50
    @State private var pageResponse: DNSRecordsPage?
    @State private var loadedPageKey: String?
    @State private var isLoadingPage = false
    @State private var pageError: String?
    @State private var retryPageToken = UUID()

    private var query: String { search.trimmingCharacters(in: .whitespacesAndNewlines) }
    private var sortField: String { sortOrder.first?.keyPath == \DNSRecord.content ? "content" : "name" }
    private var sortDirection: String { sortOrder.first?.order == .reverse ? "desc" : "asc" }
    private var pageKey: String { "\(store.serviceAddress)|\(page)|\(pageSize)|\(connectionID)|\(zoneID)|\(query)|\(sortField)|\(sortDirection)" }
    private var pageRequestKey: String { "\(pageKey)|\(store.isConnected)|\(store.supportsDNSManagement)|\(store.dnsUpdatedAt?.timeIntervalSince1970 ?? 0)|\(retryPageToken)" }
    private var currentPageResponse: DNSRecordsPage? { loadedPageKey == pageKey ? pageResponse : nil }
    private var totalRecords: Int { store.isConnected ? (currentPageResponse?.total ?? 0) : snapshotRecords.count }
    private var pageCount: Int { max(1, (totalRecords + pageSize - 1) / pageSize) }
    private var currentPage: Int { store.isConnected ? page : min(page, pageCount) }

    private var snapshotRecords: [DNSRecord] {
        store.dnsRecords.filter { record in
            (zoneID.isEmpty || record.zoneId == zoneID) &&
            (connectionID.isEmpty || store.dnsZones.first(where: { $0.id == record.zoneId })?.connectionId == connectionID) &&
            (query.isEmpty || record.name.localizedCaseInsensitiveContains(query) || record.content.localizedCaseInsensitiveContains(query) || record.id.localizedCaseInsensitiveContains(query))
        }.sorted(using: sortOrder)
    }
    private var visibleRecords: [DNSRecord] {
        if store.isConnected { return currentPageResponse?.items ?? [] }
        return Array(snapshotRecords.dropFirst((currentPage - 1) * pageSize).prefix(pageSize))
    }
    private var selected: DNSRecord? {
        let cached = store.dnsRecords.first { $0.id == selection }
        let paged = currentPageResponse?.items.first { $0.id == selection }
        guard let cached else { return paged }
        if let paged, paged.revision > cached.revision || (paged.revision == cached.revision && (DateDisplayText.parse(paged.updatedAt) ?? .distantPast) > (DateDisplayText.parse(cached.updatedAt) ?? .distantPast)) { return paged }
        return cached
    }

    var body: some View {
        if !store.supportsDNSManagement, store.isConnected {
            ContentUnavailableView(L10n.text("需要升级管理服务"), systemImage: "network", description: Text(L10n.text("此服务尚不支持 DNS 记录管理。")))
        } else {
            VStack(spacing: 0) {
                MainVerticalSplitView(hasDetail: selected != nil) {
                    VStack(spacing: 0) {
                        filters.padding(12)
                        recordsTable
                        Divider()
                        paginationControls
                    }
                } detail: {
                    if let selected {
                        detail(selected)
                    }
                }
                if let message = store.dnsError { Text(message).font(.callout).foregroundStyle(.orange).padding(8) }
                if !store.isConnected {
                    Text(L10n.text("离线快照 · {0}", String(describing: (store.dnsUpdatedAt.map { L10n.date($0) } ?? L10n.text("尚未读取"))))).font(.caption).foregroundStyle(.secondary).padding(8)
                }
            }
            .searchable(text: $search, prompt: L10n.text("搜索域名或目标"))
            .toolbar {
                ToolbarItemGroup {
                    Button { showingConnections = true } label: {
                        Label(L10n.text("连接与域名区域"), systemImage: "network")
                            .labelStyle(.iconOnly)
                    }
                    .help(L10n.text("连接与域名区域"))
                    .disabled(!store.isConnected)
                    Button { creating = true } label: { Label(L10n.text("新增记录"), systemImage: "plus") }
                        .disabled(!store.isConnected || !store.dnsZones.contains(where: \.enabled))
                }
            }
            .sheet(isPresented: $showingConnections) { DNSConnectionsView(store: store) }
            .sheet(isPresented: $creating) { DNSRecordEditorView(store: store, record: nil, initialZoneID: zoneID) }
            .sheet(item: $editing) { DNSRecordEditorView(store: store, record: $0, initialZoneID: $0.zoneId) }
            .confirmationDialog(L10n.text("删除 {0} 的 DNS 记录？", String(describing: (deleting?.name ?? ""))), isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } })) {
                Button(L10n.text("删除远端记录"), role: .destructive) { if let record = deleting { run { try await store.deleteDNSRecord(record) } }; deleting = nil }
            }
            .alert(L10n.text("DNS 操作失败"), isPresented: Binding(get: { error != nil }, set: { if !$0 { error = nil } })) { Button(L10n.text("好"), role: .cancel) { error = nil } } message: { Text(error ?? "") }
            .task { await store.refreshDNS(); openRequestedRecord() }
            .task(id: pageRequestKey) { await loadPage() }
            .onChange(of: connectionID) { _, _ in resetPage() }
            .onChange(of: zoneID) { _, _ in resetPage() }
            .onChange(of: search) { _, _ in resetPage() }
            .onChange(of: sortOrder) { _, _ in resetPage() }
            .onChange(of: pageSize) { _, _ in resetPage() }
            .onChange(of: store.serviceAddress) { _, _ in
                connectionID = ""
                zoneID = ""
                search = ""
                resetPage()
                pageResponse = nil
                loadedPageKey = nil
            }
            .onChange(of: store.requestedDNSRecordID) { _, _ in openRequestedRecord() }
        }
    }
    private var recordsTable: some View {
        // Let the native table resize columns without rebuilding rows and sorting on every size change.
        let records = visibleRecords
        return Table(records, selection: $selection, sortOrder: $sortOrder) {
            TableColumn(L10n.text("域名 / 类型"), value: \.name) { record in
                VStack(alignment: .leading, spacing: 2) {
                    Text(record.name)
                        .lineLimit(1).truncationMode(.middle)
                        .accessibilityIdentifier("dns.record.row.\(record.id)")
                    DNSRecordTypeBadge(record: record)
                }
                .help(record.name)
            }.width(min: 100, ideal: 240, max: .infinity)
            TableColumn(L10n.text("目标 / TTL"), value: \.content) { record in
                VStack(alignment: .leading, spacing: 2) {
                    Text(record.content)
                        .lineLimit(1).truncationMode(.middle)
                        .textSelection(.enabled)
                    Text(record.ttl == 1 ? L10n.text("TTL 自动") : L10n.text("TTL {0} 秒", String(describing: (record.ttl))))
                        .font(.caption).foregroundStyle(.secondary)
                }
                .help(record.content)
            }.width(min: 100, ideal: 240, max: .infinity)
            TableColumn(L10n.text("节点")) { record in
                let name = record.boundNodeId.flatMap { id in store.nodes.first { $0.id == id }?.name } ?? "—"
                Text(name).lineLimit(1).help(name)
            }.width(min: 60, ideal: 76)
            TableColumn(L10n.text("状态")) { record in
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 5) {
                        Circle().fill(record.syncColor).frame(width: 5, height: 5)
                        Text(record.stateLabel).foregroundStyle(record.syncColor)
                    }
                    Text(record.resolutionLabel).font(.caption).foregroundStyle(record.resolutionColor)
                }
                .lineLimit(1)
                .help("\(record.stateLabel) · \(record.resolutionLabel)")
            }.width(min: 80, ideal: 88)
            TableColumn(L10n.text("来源")) { record in
                Text(record.origin == "hysteriax" ? "HysteriaX" : L10n.text("已有记录"))
                    .font(.caption2.weight(.medium)).lineLimit(1)
                    .foregroundStyle(record.originColor)
                    .padding(.horizontal, 4).padding(.vertical, 2)
                    .background(record.originColor.opacity(0.10), in: Capsule())
                    .help(record.origin == "hysteriax" ? L10n.text("HysteriaX 创建") : L10n.text("已有记录"))
            }.width(min: 60, ideal: 66)
        }
        .scrollIndicators(.automatic, axes: .horizontal)
        .overlay {
            if records.isEmpty {
                if isLoadingPage || (store.isConnected && currentPageResponse == nil && pageError == nil) {
                    ProgressView()
                } else if pageError == nil {
                    if query.isEmpty && connectionID.isEmpty && zoneID.isEmpty {
                        ContentUnavailableView(L10n.text("暂无 DNS 记录"), systemImage: "network", description: Text(L10n.text("配置连接、启用域名区域并刷新记录，或直接创建记录。")))
                    } else {
                        ContentUnavailableView(L10n.text("没有匹配的 DNS 记录"), systemImage: "magnifyingglass")
                    }
                }
            }
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
                if currentPageResponse != nil || !store.isConnected {
                    Text(L10n.text("共 {0} 条 DNS 记录 · 第 {1} / {2} 页", String(totalRecords), String(currentPage), String(pageCount)))
                        .foregroundStyle(.secondary).monospacedDigit()
                }
                if isLoadingPage { ProgressView().controlSize(.small) }
                Spacer(minLength: 0)
                Picker(L10n.text("每页条数"), selection: $pageSize) {
                    ForEach([25, 50, 100], id: \.self) { Text(String($0)).tag($0) }
                }.fixedSize().accessibilityIdentifier("dns.pageSize")
                Button(L10n.text("上一页")) { changePage(currentPage - 1) }
                    .disabled(currentPage <= 1 || isLoadingPage || (store.isConnected && currentPageResponse == nil))
                    .accessibilityIdentifier("dns.previousPage")
                Button(L10n.text("下一页")) { changePage(currentPage + 1) }
                    .disabled(currentPage >= pageCount || isLoadingPage || (store.isConnected && currentPageResponse == nil))
                    .accessibilityIdentifier("dns.nextPage")
            }.font(.callout)
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        .accessibilityIdentifier("dns.pagination")
    }

    private func changePage(_ page: Int) { self.page = page; selection = nil }
    private func resetPage() { changePage(1) }

    private func loadPage() async {
        let requestKey = pageRequestKey
        guard store.isConnected, store.supportsDNSManagement else { isLoadingPage = false; pageError = nil; return }
        isLoadingPage = true
        pageError = nil
        defer { if requestKey == pageRequestKey { isLoadingPage = false } }
        do {
            if !query.isEmpty { try await Task.sleep(for: .milliseconds(250)) }
            let response = try await store.dnsRecordsPage(page: page, pageSize: pageSize, connectionID: connectionID, zoneID: zoneID, query: query, sort: sortField, order: sortDirection)
            guard !Task.isCancelled, requestKey == pageRequestKey else { return }
            page = response.page
            pageResponse = response
            loadedPageKey = pageKey
        } catch {
            guard !Task.isCancelled, requestKey == pageRequestKey else { return }
            pageError = error.localizedDescription
        }
    }

    private var filters: some View {
        HStack {
            Picker(L10n.text("连接"), selection: $connectionID) {
                Text(L10n.text("全部连接")).tag("")
                ForEach(store.dnsConnections) { Text($0.name).tag($0.id) }
            }
            Picker(L10n.text("域名区域"), selection: $zoneID) {
                Text(L10n.text("全部域名")).tag("")
                ForEach(store.dnsZones.filter { connectionID.isEmpty || $0.connectionId == connectionID }) { Text($0.name).tag($0.id) }
            }
            if let zone = store.dnsZones.first(where: { $0.id == zoneID }) {
                Button(L10n.text("刷新远端记录")) { run { try await store.refreshDNSZone(zone) } }.disabled(!store.isConnected || !zone.enabled || busy)
            }
            Spacer()
            if store.dnsIsLoading || busy { ProgressView().controlSize(.small) }
        }.onChange(of: connectionID) { _, _ in zoneID = "" }
    }
    private func detail(_ record: DNSRecord) -> some View {
        let zone = store.dnsZones.first { $0.id == record.zoneId }
        let connection = store.dnsConnections.first { $0.id == zone?.connectionId }
        let failedJob = store.jobs.first { $0.resourceId == record.id && $0.status == "failed" && $0.retryJobId == nil }
        return DNSRecordDetailView(
            record: record, zone: zone, connection: connection,
            node: store.nodes.first { $0.id == record.boundNodeId }, failedJob: failedJob,
            isConnected: store.isConnected, busy: busy,
            onEdit: { editing = record },
            onCheck: { run { try await store.checkDNSRecord(record) } },
            onDelete: { deleting = record },
            onBind: { bindingRecord = record },
            onRetry: { if let failedJob { run { try await store.retryDNSJob(failedJob) } } },
            onOpenNode: onOpenNode
        )
        .sheet(item: $bindingRecord) { DNSRecordBindingPicker(store: store, record: $0) }
    }
    @State private var bindingRecord: DNSRecord?
    private func run(_ action: @escaping @MainActor () async throws -> Void) {
        busy = true
        Task { defer { busy = false }; do { try await action() } catch { self.error = error.localizedDescription } }
    }
    private func openRequestedRecord() {
        guard let id = store.requestedDNSRecordID else { return }
        selection = id; store.requestedDNSRecordID = nil
    }
}
