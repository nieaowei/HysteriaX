import SwiftUI

struct DNSBindingEditorView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ManagementStore
    let nodeID: String
    @State private var detail: NodeDetail?
    @State private var draft = DNSAllocationDraft()
    @State private var externalHost = ""
    @State private var saving = false
    @State private var error: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("节点域名分配").font(.title2.bold())
            if let detail {
                Text(detail.name).foregroundStyle(.secondary)
                Form {
                    DNSAllocationFields(store: store, draft: $draft, sshHost: detail.ssh.host)
                    if draft.mode == "external" { TextField("替代公开地址", text: $externalHost) }
                    if let published = detail.publishedConnection {
                        LabeledContent("已发布地址", value: published.publicHost)
                    }
                    if detail.dnsBinding != nil { Text("更换或解除绑定后保留原 DNS 记录，可在 DNS 记录页清理。").font(.caption).foregroundStyle(.secondary) }
                }.formStyle(.grouped).disabled(saving)
            } else { ProgressView("读取节点…").frame(maxWidth: .infinity, maxHeight: .infinity) }
            if let error { Text(error).foregroundStyle(.red).textSelection(.enabled) }
            HStack {
                Button("取消") { dismiss() }.keyboardShortcut(.cancelAction)
                Spacer()
                Button(saving ? "正在提交…" : "保存域名分配") { save() }
                    .disabled(saving || detail == nil || !store.isConnected).keyboardShortcut(.defaultAction)
            }
        }.padding(24).frame(width: 620, height: 620).interactiveDismissDisabled(saving)
        .task {
            await store.refreshDNS()
            do {
                let loaded = try await store.nodeDetail(nodeID); detail = loaded; externalHost = loaded.connection.host
                if let binding = loaded.dnsBinding {
                    draft.mode = "existing"; draft.zoneID = binding.zoneId; draft.selectedHostname = binding.hostname
                } else { draft.mode = "auto" }
            } catch { self.error = error.localizedDescription }
        }
    }
    private func save() {
        guard let detail else { return }
        saving = true
        Task {
            defer { saving = false }
            do {
                if let allocation = try draft.allocation(zones: store.dnsZones, records: store.dnsRecords) {
                    try await store.setDNSBinding(nodeID: nodeID, revision: detail.revision, allocation: allocation)
                } else if detail.dnsBinding != nil {
                    try await store.removeDNSBinding(nodeID: nodeID, revision: detail.revision, publicHost: externalHost, idempotencyKey: draft.idempotencyKey)
                } else {
                    try await store.setExternalPublicHost(detail, host: externalHost)
                }
                dismiss()
            } catch { self.error = error.localizedDescription }
        }
    }
}

struct DNSRecordBindingPicker: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ManagementStore
    let record: DNSRecord
    @State private var nodeID = ""
    @State private var saving = false
    @State private var error: String?
    @State private var key = UUID().uuidString

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("绑定 \(record.name)").font(.title2.bold())
            Picker("节点", selection: $nodeID) {
                Text("选择节点").tag("")
                ForEach(store.nodes.filter { !["deleting", "delete_failed"].contains($0.state) }) { Text($0.name).tag($0.id) }
            }
            Text("保留现有解析目标。已部署节点会验证新域名后再发布到订阅；证书仍在代理配置中设置。").font(.callout).foregroundStyle(.secondary)
            if let error { Text(error).foregroundStyle(.red) }
            HStack {
                Button("取消") { dismiss() }.keyboardShortcut(.cancelAction)
                Spacer()
                Button("绑定") {
                    saving = true
                    Task {
                        defer { saving = false }
                        do {
                            let node = try await store.nodeDetail(nodeID)
                            let ids = store.dnsRecords.filter { $0.zoneId == record.zoneId && $0.name == record.name && $0.supportsEditing && !$0.proxied && $0.state == "synced" }.map(\.id)
                            try await store.setDNSBinding(nodeID: nodeID, revision: node.revision,
                                allocation: DNSAllocation(idempotencyKey: key, zoneId: record.zoneId, mode: "existing", recordIds: ids))
                            dismiss()
                        } catch { self.error = error.localizedDescription }
                    }
                }.disabled(nodeID.isEmpty || saving || !store.isConnected).keyboardShortcut(.defaultAction)
            }
        }.padding(24).frame(width: 520)
    }
}
