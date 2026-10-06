import SwiftUI

struct DNSConnectionsView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ManagementStore
    @State private var name = ""
    @State private var credentialID = ""
    @State private var saving = false
    @State private var error: String?
    @State private var renaming: DNSConnection?
    @State private var renamedName = ""
    @State private var deleting: DNSConnection?

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack { Text("DNS 连接与域名区域").font(.title2.bold()); Spacer(); Button("完成") { dismiss() }.keyboardShortcut(.cancelAction) }
            Form {
                Section("新增 Cloudflare 连接") {
                    TextField("连接名称", text: $name)
                    CredentialPickerView(store: store, selection: $credentialID, kinds: ["dns"], dnsProvider: "cloudflare", title: "DNS 凭据")
                    Text("选择 Cloudflare 凭据。创建连接后验证访问权限，再启用需要管理的域名区域。").font(.caption).foregroundStyle(.secondary)
                    Button("创建连接") { run { try await store.createDNSConnection(name: name, credentialID: credentialID); name = "" } }
                        .disabled(name.isEmpty || credentialID.isEmpty)
                }
                ForEach(store.dnsConnections) { connection in
                    Section(connection.name) {
                        LabeledContent("凭据版本", value: "v\(connection.credentialVersion)")
                        LabeledContent("验证状态", value: connection.status == "verified" ? "已验证读取权限" : "尚未验证")
                        HStack {
                            Button("验证并读取域名") { run { try await store.verifyDNSConnection(connection) } }
                            Button("刷新域名") { run { try await store.verifyDNSConnection(connection, refresh: true) } }
                            Button("重命名…") { renamedName = connection.name; renaming = connection }
                            Spacer()
                            Button("删除…", role: .destructive) { deleting = connection }
                        }
                        ForEach(store.dnsZones.filter { $0.connectionId == connection.id }) { zone in
                            HStack {
                                Toggle(zone.name, isOn: Binding(get: { zone.enabled }, set: { enabled in run { try await store.setDNSZoneEnabled(zone, enabled: enabled) } }))
                                Button("刷新记录") { run { try await store.refreshDNSZone(zone) } }.disabled(!zone.enabled)
                            }
                        }
                    }
                }
            }.formStyle(.grouped).disabled(saving || !store.isConnected)
            if let error { Text(error).foregroundStyle(.red).textSelection(.enabled) }
            if saving { ProgressView().controlSize(.small) }
        }.padding(24).frame(width: 700, height: 640)
        .task { await store.refreshDNS() }
        .alert("连接名称", isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
            TextField("名称", text: $renamedName)
            Button("保存") { if let connection = renaming { run { try await store.renameDNSConnection(connection, name: renamedName) } }; renaming = nil }
            Button("取消", role: .cancel) { renaming = nil }
        }
        .confirmationDialog("删除 DNS 连接？", isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } })) {
            Button("删除", role: .destructive) { if let connection = deleting { run { try await store.deleteDNSConnection(connection) } }; deleting = nil }
        }
    }
    private func run(_ action: @escaping @MainActor () async throws -> Void) {
        saving = true
        Task { defer { saving = false }; do { try await action(); error = nil } catch { self.error = error.localizedDescription } }
    }
}
