import SwiftUI

struct DNSRecordEditorView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ManagementStore
    let record: DNSRecord?
    let initialZoneID: String
    @State private var loaded: DNSRecord?
    @State private var zoneID = ""
    @State private var name = ""
    @State private var recordType = "A"
    @State private var content = ""
    @State private var ttl = "1"
    @State private var proxied = false
    @State private var saving = false
    @State private var loading = false
    @State private var error: String?
    @State private var requestKey = UUID().uuidString

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(record == nil ? L10n.text("新增 DNS 记录") : L10n.text("编辑 DNS 记录")).font(.title2.bold())
            Form {
                Picker(L10n.text("域名区域"), selection: $zoneID) {
                    Text(L10n.text("选择域名区域")).tag("")
                    ForEach(store.dnsZones.filter(\.enabled)) { Text($0.name).tag($0.id) }
                }.disabled(record != nil)
                TextField(L10n.text("完整域名（根域名可填 @）"), text: $name).accessibilityIdentifier("dns.record.name")
                Picker(L10n.text("类型"), selection: $recordType) { ForEach(["A", "AAAA", "CNAME"], id: \.self) { Text($0).tag($0) } }
                TextField(recordType == "CNAME" ? L10n.text("目标域名") : L10n.text("目标 IP"), text: $content).accessibilityIdentifier("dns.record.content")
                TextField(L10n.text("TTL（1 表示自动）"), text: $ttl)
                Toggle(L10n.text("Cloudflare 代理"), isOn: $proxied)
                if record?.boundNodeId != nil {
                    Text(L10n.text("此记录关联节点。修改解析目标会影响连接；域名、类型和代理状态需通过节点重新分配修改。")).font(.caption).foregroundStyle(.secondary)
                }
            }.formStyle(.grouped).disabled(loading || saving)
            if let error { Text(error).foregroundStyle(.red).textSelection(.enabled) }
            HStack {
                Button(L10n.text("取消")) { dismiss() }.keyboardShortcut(.cancelAction)
                Spacer()
                if loading { ProgressView().controlSize(.small) }
                Button(saving ? L10n.text("正在提交…") : L10n.text("保存")) { save() }
                    .disabled(saving || loading || !store.isConnected || zoneID.isEmpty || (record != nil && loaded == nil))
                    .keyboardShortcut(.defaultAction)
            }
        }.padding(24).frame(width: 560, height: 500)
        .interactiveDismissDisabled(saving)
        .task {
            zoneID = record?.zoneId ?? initialZoneID
            guard let record else { return }
            loading = true
            defer { loading = false }
            do {
                let current = try await store.dnsRecordDetail(record.id)
                loaded = current; name = current.name; recordType = current.recordType
                content = current.content; ttl = String(current.ttl); proxied = current.proxied
            } catch { self.error = error.localizedDescription }
        }
    }
    private func save() {
        guard let ttlValue = Int(ttl) else { error = L10n.text("请输入有效的 TTL。"); return }
        saving = true
        Task {
            defer { saving = false }
            do {
                try await store.saveDNSRecord(loaded, zoneID: zoneID,
                    input: DNSRecordInput(name: name, recordType: recordType, content: content, ttl: ttlValue, proxied: proxied), idempotencyKey: requestKey)
                dismiss()
            } catch { self.error = error.localizedDescription }
        }
    }
}
