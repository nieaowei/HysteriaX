import SwiftUI

struct ManagedMTLSFields: View {
    @Bindable var store: ManagementStore
    @Binding var draft: ManagedMTLSDraft
    let manualTLS: Bool
    @State private var creating = false
    @State private var authority: CredentialDetail?
    @State private var loading = false
    @State private var error: String?

    private var entries: [CredentialSummary] {
        store.credentials.filter { $0.kind == "ca_identity" && (!$0.archived || $0.id == draft.caID) }
    }
    private var selection: Binding<String> {
        Binding(get: { draft.caID }, set: { id in draft.caID = id; draft.caVersion = nil; authority = nil })
    }
    var body: some View {
        Section("mTLS") {
            Picker(L10n.text("客户端证书验证"), selection: $draft.mode) {
                Text(L10n.text("关闭")).tag("off")
                Text(L10n.text("凭据中心系统托管")).tag("managed")
            }.pickerStyle(.segmented).accessibilityIdentifier("node.config.mtls.mode")
            if draft.mode == "managed" {
                HStack {
                    Picker(L10n.text("CA 身份"), selection: selection) {
                        Text(L10n.text("选择凭据")).tag("")
                        ForEach(entries) { Text($0.name).tag($0.id) }
                        if !draft.caID.isEmpty && !entries.contains(where: { $0.id == draft.caID }) {
                            Text(draft.caID).tag(draft.caID)
                        }
                    }.accessibilityIdentifier("node.config.mtls.ca")
                    Button(L10n.text("创建 CA 身份")) { creating = true }.disabled(!store.isConnected)
                    if loading { ProgressView().controlSize(.small) }
                }
                if let authority, let ca = authority.ca {
                    LabeledContent(L10n.text("当前发布版本"), value: "v\(ca.publishedVersion)")
                    if let version = authority.versions.first(where: { $0.version == ca.publishedVersion }), let expiry = version.metadata["expires_at"]?.stringValue {
                        LabeledContent(L10n.text("CA 到期"), value: DateDisplayText.local(expiry))
                    }
                }
                Text(L10n.text("系统按用户与 CA 共享身份并自动续期，探测证书独立管理，无需先分配用户。共享 CA 是共享信任域；更换 CA 后客户端需要刷新订阅。"))
                    .font(.caption).foregroundStyle(.secondary)
                if !manualTLS { Text(L10n.text("托管 mTLS 仅支持凭据中心的手动 TLS 证书。" )).foregroundStyle(.red) }
                if let error { Text(error).font(.caption).foregroundStyle(.red) }
            }
        }
        .task(id: store.dataRefreshToken) { await store.prepareDirectory(.credentials) }
        .task(id: "\(draft.caID)|\(store.dataRefreshToken)") { await loadAuthority() }
        .sheet(isPresented: $creating) {
            CAIdentityEditorView(store: store) { id in draft.caID = id; draft.caVersion = nil }
        }
    }
    private func loadAuthority() async {
        let id = draft.caID
        guard !id.isEmpty else { authority = nil; return }
        loading = true; error = nil
        defer { loading = false }
        do {
            let loaded = try await store.credentialDetail(id)
            guard !Task.isCancelled, draft.caID == id else { return }
            guard let ca = loaded.ca, !loaded.archived else {
                draft.caVersion = nil
                throw APIClientError.server(L10n.text("请选择有效 CA 身份并等待读取当前发布版本。"))
            }
            authority = loaded; draft.caVersion = ca.publishedVersion
        } catch {
            if !Task.isCancelled && draft.caID == id { self.error = error.localizedDescription; draft.caVersion = nil }
        }
    }
}
