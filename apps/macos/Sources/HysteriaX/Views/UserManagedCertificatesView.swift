import SwiftUI

struct UserManagedCertificatesView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ManagementStore
    @State var user: UserSummary
    @State private var isLoading = true
    @State private var workingCA: String?
    @State private var requests: [String: ManagedUserReissueRequest] = [:]
    @State private var errorMessage: String?
    @State private var notice: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(L10n.text("{0} 的托管证书", user.name)).font(.title2.bold())
            Text(L10n.text("账号到期：{0}", user.expiresAt.map { DateDisplayText.local($0) } ?? L10n.text("永久")))
                .font(.callout).foregroundStyle(.secondary)
            if isLoading { ProgressView() }
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 14) {
                    ForEach(user.managedCertificates ?? [], id: \.identityId) { identity in
                        ManagedUserIdentityCard(identity: identity, nodes: store.nodes, isWorking: workingCA == identity.caCredentialId, disabled: workingCA != nil || !store.isConnected) { reissue(identity) }
                    }
                    if !isLoading && (user.managedCertificates ?? []).isEmpty {
                        Text(L10n.text("此用户尚无托管证书身份。加入托管节点的授权组后自动建立。"))
                            .foregroundStyle(.secondary)
                    }
                }
            }
            Text(L10n.text("重新签发后请刷新订阅。旧证书在自然到期前仍有效，不会因此断开现有连接。"))
                .font(.caption).foregroundStyle(.secondary)
            if let notice { Text(notice).font(.callout).foregroundStyle(.secondary) }
            if let errorMessage { Text(errorMessage).foregroundStyle(.red).textSelection(.enabled) }
            HStack {
                Button(L10n.text("关闭")) { dismiss() }.keyboardShortcut(.cancelAction)
                Spacer()
                Button(L10n.text("重新加载")) { requests.removeAll(); Task { await load() } }.disabled(isLoading || workingCA != nil)
            }
        }
        .padding(24).frame(width: 680, height: 580)
        .interactiveDismissDisabled(workingCA != nil)
        .task { await load() }
    }

    private func load() async {
        isLoading = true
        defer { isLoading = false }
        do { user = try await store.userSummary(user.id); errorMessage = nil }
        catch { errorMessage = error.localizedDescription }
    }

    private func reissue(_ identity: ManagedUserIdentity) {
        let request = requests[identity.caCredentialId] ?? ManagedUserReissueRequest(
            caCredentialId: identity.caCredentialId,
            expectedRevisions: [identity.caCredentialId: .integer(identity.revision)],
            idempotencyKey: UUID().uuidString
        )
        requests[identity.caCredentialId] = request
        workingCA = identity.caCredentialId
        errorMessage = nil
        Task {
            defer { workingCA = nil }
            do {
                let result = try await store.reissueManagedCertificates(userID: user.id, request: request)
                requests.removeValue(forKey: identity.caCredentialId)
                notice = result.identities.contains(where: { $0.issued }) ? L10n.text("共享证书已更新，请刷新相关节点的订阅。") : L10n.text("身份已保留，账号恢复可用后自动签发。")
                await load()
            } catch { errorMessage = error.localizedDescription }
        }
    }
}

private struct ManagedUserIdentityCard: View {
    let identity: ManagedUserIdentity
    let nodes: [NodeSummary]
    let isWorking: Bool
    let disabled: Bool
    let reissue: () -> Void

    private var current: ManagedIdentityVersion? {
        identity.versions.first { $0.released && !$0.discarded && $0.caVersion == identity.publishedCaVersion }
    }
    var body: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 8) {
                LabeledContent(L10n.text("CA 到期"), value: DateDisplayText.local(identity.caExpiresAt))
                if let current {
                    LabeledContent(L10n.text("证书到期"), value: DateDisplayText.local(current.expiresAt))
                } else {
                    Text(L10n.text("待签发或恢复，暂不发布缺少可用证书的节点。"))
                        .font(.callout).foregroundStyle(.orange)
                }
                Text(L10n.text("引用节点：{0}", identity.nodeIds.map { id in nodes.first { $0.id == id }?.name ?? id }.joined(separator: "、")))
                    .font(.caption).textSelection(.enabled)
                if !identity.versions.isEmpty {
                    DisclosureGroup(L10n.text("证书版本（{0}）", String(identity.versions.count))) {
                        ForEach(identity.versions, id: \.version) { version in
                            VStack(alignment: .leading, spacing: 3) {
                                Text(L10n.text("证书 v{0} · 签发 CA v{1}", String(version.version), String(version.caVersion)))
                                Text(DateDisplayText.local(version.expiresAt)).foregroundStyle(.secondary)
                                if !version.released { Text(L10n.text("准备中，尚未发布")) }
                            }.font(.caption).padding(.vertical, 3)
                        }
                    }
                }
                if let reason = identity.maintenanceError { Text(reason).foregroundStyle(.red).font(.caption) }
                HStack {
                    Text(identity.archived ? L10n.text("已归档") : L10n.text("重新签发将影响此 CA 下全部 {0} 个引用节点。", String(identity.nodeIds.count)))
                        .font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    Button(isWorking ? L10n.text("正在签发…") : L10n.text("重新签发"), action: reissue)
                        .disabled(disabled || identity.archived)
                }
            }.frame(maxWidth: .infinity, alignment: .leading).padding(4)
        } label: { Label(identity.caName, systemImage: "checkmark.shield") }
        .accessibilityIdentifier("users.managedIdentity.\(identity.identityId)")
    }
}
