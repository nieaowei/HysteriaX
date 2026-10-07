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

    private var visibleRecords: [DNSRecord] {
        store.dnsRecords.filter { record in
            (zoneID.isEmpty || record.zoneId == zoneID) &&
            (connectionID.isEmpty || store.dnsZones.first(where: { $0.id == record.zoneId })?.connectionId == connectionID) &&
            (search.isEmpty || record.name.localizedCaseInsensitiveContains(search) || record.content.localizedCaseInsensitiveContains(search))
        }.sorted(using: sortOrder)
    }
    private var selected: DNSRecord? { store.dnsRecords.first { $0.id == selection } }

    var body: some View {
        if !store.supportsDNSManagement, store.isConnected {
            ContentUnavailableView("需要升级管理服务", systemImage: "network", description: Text("此服务尚不支持 DNS 记录管理。"))
        } else {
            VStack(spacing: 0) {
                MainVerticalSplitView(hasDetail: selected != nil) {
                    VStack(spacing: 0) {
                        filters.padding(12)
                        recordsTable
                    }
                } detail: {
                    if let selected {
                        detail(selected)
                    }
                }
                if let message = store.dnsError { Text(message).font(.callout).foregroundStyle(.orange).padding(8) }
                if !store.isConnected {
                    Text("离线快照 · \(store.dnsUpdatedAt?.formatted(date: .abbreviated, time: .shortened) ?? "尚未读取")").font(.caption).foregroundStyle(.secondary).padding(8)
                }
            }
            .searchable(text: $search, prompt: "搜索域名或目标")
            .toolbar {
                ToolbarItemGroup {
                    Button { showingConnections = true } label: {
                        Label("连接与域名区域", systemImage: "network")
                            .labelStyle(.iconOnly)
                    }
                    .help("连接与域名区域")
                    .disabled(!store.isConnected)
                    Button { creating = true } label: { Label("新增记录", systemImage: "plus") }
                        .disabled(!store.isConnected || !store.dnsZones.contains(where: \.enabled))
                }
            }
            .sheet(isPresented: $showingConnections) { DNSConnectionsView(store: store) }
            .sheet(isPresented: $creating) { DNSRecordEditorView(store: store, record: nil, initialZoneID: zoneID) }
            .sheet(item: $editing) { DNSRecordEditorView(store: store, record: $0, initialZoneID: $0.zoneId) }
            .confirmationDialog("删除 \(deleting?.name ?? "") 的 DNS 记录？", isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } })) {
                Button("删除远端记录", role: .destructive) { if let record = deleting { run { try await store.deleteDNSRecord(record) } }; deleting = nil }
            }
            .alert("DNS 操作失败", isPresented: Binding(get: { error != nil }, set: { if !$0 { error = nil } })) { Button("好", role: .cancel) { error = nil } } message: { Text(error ?? "") }
            .task { await store.refreshDNS(); openRequestedRecord() }
            .onChange(of: store.requestedDNSRecordID) { _, _ in openRequestedRecord() }
        }
    }
    private var recordsTable: some View {
        // Let the native table resize columns without rebuilding rows and sorting on every size change.
        let records = visibleRecords
        return Table(records, selection: $selection, sortOrder: $sortOrder) {
            TableColumn("域名 / 类型", value: \.name) { record in
                VStack(alignment: .leading, spacing: 2) {
                    Text(record.name)
                        .lineLimit(1).truncationMode(.middle)
                        .accessibilityIdentifier("dns.record.row.\(record.id)")
                    DNSRecordTypeBadge(record: record)
                }
                .help(record.name)
            }.width(min: 100, ideal: 240, max: .infinity)
            TableColumn("目标 / TTL", value: \.content) { record in
                VStack(alignment: .leading, spacing: 2) {
                    Text(record.content)
                        .lineLimit(1).truncationMode(.middle)
                        .textSelection(.enabled)
                    Text(record.ttl == 1 ? "TTL 自动" : "TTL \(record.ttl) 秒")
                        .font(.caption).foregroundStyle(.secondary)
                }
                .help(record.content)
            }.width(min: 100, ideal: 240, max: .infinity)
            TableColumn("节点") { record in
                let name = record.boundNodeId.flatMap { id in store.nodes.first { $0.id == id }?.name } ?? "—"
                Text(name).lineLimit(1).help(name)
            }.width(min: 60, ideal: 76)
            TableColumn("状态") { record in
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
            TableColumn("来源") { record in
                Text(record.origin == "hysteriax" ? "HysteriaX" : "已有记录")
                    .font(.caption2.weight(.medium)).lineLimit(1)
                    .foregroundStyle(record.originColor)
                    .padding(.horizontal, 4).padding(.vertical, 2)
                    .background(record.originColor.opacity(0.10), in: Capsule())
                    .help(record.origin == "hysteriax" ? "HysteriaX 创建" : "已有记录")
            }.width(min: 60, ideal: 66)
        }
        .scrollIndicators(.automatic, axes: .horizontal)
        .overlay {
            if records.isEmpty { ContentUnavailableView("暂无 DNS 记录", systemImage: "network", description: Text("配置连接、启用域名区域并刷新记录，或直接创建记录。")) }
        }
    }

    private var filters: some View {
        HStack {
            Picker("连接", selection: $connectionID) {
                Text("全部连接").tag("")
                ForEach(store.dnsConnections) { Text($0.name).tag($0.id) }
            }
            Picker("域名区域", selection: $zoneID) {
                Text("全部域名").tag("")
                ForEach(store.dnsZones.filter { connectionID.isEmpty || $0.connectionId == connectionID }) { Text($0.name).tag($0.id) }
            }
            if let zone = store.dnsZones.first(where: { $0.id == zoneID }) {
                Button("刷新远端记录") { run { try await store.refreshDNSZone(zone) } }.disabled(!store.isConnected || !zone.enabled || busy)
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
            onOpenJobs: { store.requestedSection = "jobs" },
            onOpenAudit: { store.requestedSection = "audit" },
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
        connectionID = ""; zoneID = ""; search = ""; selection = id; store.requestedDNSRecordID = nil
    }
}
