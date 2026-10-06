import SwiftUI

struct CredentialPickerView: View {
    @Bindable var store: ManagementStore
    @Binding var selection: String
    var kinds: [String]
    var dnsProvider: String? = nil
    var ownerUserID: String? = nil
    var title = "凭据"
    @State private var creating = false
    private var entries: [CredentialSummary] {
        store.credentials.filter { kinds.contains($0.kind) && (dnsProvider == nil || $0.metadata["provider"]?.stringValue == dnsProvider) && $0.ownerUserId == ownerUserID && (!$0.archived || $0.id == selection) }
    }
    var body: some View {
        HStack {
            Picker(title, selection: $selection) {
                Text("选择凭据").tag("")
                ForEach(entries) { Text("\($0.name) · v\($0.latestVersion)").tag($0.id) }
            }
            .accessibilityIdentifier("credential.picker")
            Button("创建…") { creating = true }.disabled(!store.isConnected)
        }
        .sheet(isPresented: $creating) {
            CredentialEditorView(store: store, initialKind: kinds.first ?? "ssh_private_key", initialOwner: ownerUserID) { selection = $0.id }
        }
    }
}

struct ManagedDNSCredentialFields: View {
    @Bindable var store: ManagementStore
    @Binding var draft: ACMEDNSDraft
    private var selection: Binding<String> {
        Binding(get: {
            guard let value = draft.values.values.first, value.hasPrefix("credential://") else { return "" }
            return String(value.dropFirst("credential://".count).split(separator: "/").first ?? "")
        }, set: { id in
            guard let entry = store.credentials.first(where: { $0.id == id }),
                  let provider = entry.metadata["provider"]?.stringValue else { draft = ACMEDNSDraft(); return }
            let fields = entry.metadata["fields"]?.arrayValue?.compactMap(\.stringValue) ?? []
            draft = ACMEDNSDraft(provider: provider, config: Dictionary(uniqueKeysWithValues: fields.map { ($0, .string(entry.reference($0))) }))
        })
    }
    var body: some View {
        CredentialPickerView(store: store, selection: selection, kinds: ["dns"], title: "DNS 凭据")
        if !draft.provider.isEmpty { LabeledContent("DNS 服务商", value: draft.definition?.title ?? draft.provider) }
        Text("凭据版本发布后会自动更新全部引用节点。").font(.caption).foregroundStyle(.secondary)
    }
}
