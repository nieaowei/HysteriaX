import SwiftUI

struct DNSRecordsView: View {
    @Bindable var store: ManagementStore
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
                        Table(visibleRecords, selection: $selection, sortOrder: $sortOrder) {
                            TableColumn("域名", value: \.name) { Text($0.name).accessibilityIdentifier("dns.record.row.\($0.id)") }.width(min: 160, ideal: 250)
                            TableColumn("类型", value: \.recordType).width(60)
                            TableColumn("目标", value: \.content) { Text($0.content).textSelection(.enabled) }.width(min: 140, ideal: 230)
                            TableColumn("TTL") { Text($0.ttl == 1 ? "自动" : String($0.ttl)) }.width(60)
                            TableColumn("节点") { record in Text(record.boundNodeId.flatMap { id in store.nodes.first { $0.id == id }?.name } ?? "—") }.width(min: 90, ideal: 120)
                            TableColumn("状态") { record in VStack(alignment: .leading) { Text(record.stateLabel); Text(record.resolutionLabel).font(.caption).foregroundStyle(.secondary) } }.width(min: 100, ideal: 120)
                            TableColumn("来源") { Text($0.origin == "hysteriax" ? "HysteriaX 创建" : "已有记录") }.width(min: 100, ideal: 120)
                        }
                        .overlay {
                            if visibleRecords.isEmpty { ContentUnavailableView("暂无 DNS 记录", systemImage: "network", description: Text("配置连接、启用域名区域并刷新记录，或直接创建记录。")) }
                        }
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
                    Button("连接与域名…") { showingConnections = true }.disabled(!store.isConnected)
                    Button { creating = true } label: { Label("新增记录", systemImage: "plus") }
                        .disabled(!store.isConnected || !store.dnsZones.contains(where: \.enabled))
                    Button { Task { await store.refreshDNS() } } label: { Label("刷新列表", systemImage: "arrow.clockwise") }.disabled(store.dnsIsLoading)
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
            onOpenAudit: { store.requestedSection = "audit" }
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
