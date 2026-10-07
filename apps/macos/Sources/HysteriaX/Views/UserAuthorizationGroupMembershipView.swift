import SwiftUI

struct UserAuthorizationGroupMembershipView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ManagementStore
    @State private var user: UserSummary

    @State private var groupIDs: Set<String>
    @State private var credentialIDsByPair: [String: String] = [:]
    @State private var requiredMTLSPairs: [AuthorizationPair] = []
    @State private var preview: AuthorizationChangePreview?
    @State private var receipt: UserAuthorizationGroupsMutationResponse?
    @State private var selectingGroups = false
    @State private var isPreviewing = false
    @State private var isSaving = false
    @State private var errorMessage: String?

    init(store: ManagementStore, user: UserSummary) {
        self.store = store
        _user = State(initialValue: user)
        _groupIDs = State(initialValue: Set(user.authorizationGroups?.map(\.id) ?? []))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(receipt == nil ? L10n.text("管理用户所属授权组") : L10n.text("用户授权已更新"))
                .font(.title2.bold())

            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
            if let receipt {
                AuthorizationMutationReceiptView(
                    store: store,
                    createdCredentials: receipt.createdCredentials,
                    revocationJobIDs: receipt.revocationJobIds
                )
            } else {
                Text(L10n.text("为 {0} 选择所属组。有效节点由所有授权组取并集；离开一个组后，只要其他组仍提供同一节点权限就不会撤权。", String(describing: (user.name))))
                    .font(.callout).foregroundStyle(.secondary)

                GroupBox {
                    VStack(alignment: .leading, spacing: 10) {
                        Text(L10n.text("已选 {0} 个授权组", String(groupIDs.count)))
                        HStack {
                            Text(selectedGroupNames)
                                .font(.caption).foregroundStyle(.secondary)
                                .lineLimit(3)
                            Spacer()
                            Button(L10n.text("选择授权组…")) { selectingGroups = true }
                                .accessibilityIdentifier("users.groups.select")
                                .disabled(isPreviewing || isSaving)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.vertical, 4)
                } label: {
                    Label(L10n.text("所属授权组"), systemImage: "person.3")
                }

                if let preview {
                    previewSummary(preview)
                }
                if !requiredMTLSPairs.isEmpty {
                    mtlsBindings
                        .disabled(isPreviewing || isSaving)
                }
            }

                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(maxHeight: .infinity)

            if let errorMessage { Text(errorMessage).foregroundStyle(.red).textSelection(.enabled) }

            HStack {
                Button(receipt == nil ? L10n.text("取消") : L10n.text("完成"), role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Spacer()
                if receipt == nil {
                    Button(isPreviewing ? L10n.text("正在预览…") : L10n.text("预览影响")) { runPreview() }
                        .disabled(isPreviewing || isSaving || !store.isConnected)
                        .accessibilityIdentifier("users.groups.preview")
                    Button(isSaving ? L10n.text("正在保存…") : L10n.text("保存所属组")) { save() }
                        .disabled(preview?.missingMtls.isEmpty != true || isSaving || isPreviewing || !store.isConnected)
                        .keyboardShortcut(.defaultAction)
                        .accessibilityIdentifier("users.groups.save")
                }
            }
        }
        .padding(24)
        .frame(width: 740, height: 560)
        .interactiveDismissDisabled(isSaving)
        .sheet(isPresented: $selectingGroups) {
            AuthorizationMultiSelectSheet(
                title: L10n.text("选择授权组"),
                options: store.authorizationGroups.map { AuthorizationPickerOption(id: $0.id, name: $0.name, detail: L10n.text("{0} 位用户 · {1} 个节点", String($0.userCount), String($0.nodeCount))) },
                selection: $groupIDs
            )
        }
        .onChange(of: groupIDs) { _, _ in
            preview = nil
            requiredMTLSPairs = []
            credentialIDsByPair = [:]
            errorMessage = nil
        }
    }

    private var selectedGroupNames: String {
        let names = groupIDs.sorted().map { id in
            store.authorizationGroups.first(where: { $0.id == id })?.name ?? id
        }
        return names.isEmpty ? L10n.text("尚未选择授权组") : names.joined(separator: "、")
    }

    private func previewSummary(_ preview: AuthorizationChangePreview) -> some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 10) {
                Text(L10n.text("新增 {0} 条有效连接 · 移除 {1} 条有效连接", String(preview.additionsCount), String(preview.removalsCount)))
                    .font(.callout.weight(.medium))
                if !preview.additions.isEmpty {
                    DisclosureGroup(L10n.text("查看新增连接（{0}）", String(preview.additions.count))) {
                        pairList(preview.additions).padding(.top, 6)
                    }
                }
                if !preview.removals.isEmpty {
                    DisclosureGroup(L10n.text("查看失去最后授权来源的连接（{0}）", String(preview.removals.count))) {
                        pairList(preview.removals).padding(.top, 6)
                    }
                }
                if !preview.missingMtls.isEmpty {
                    Text(L10n.text("新增连接中有 {0} 条需要该用户自己的 mTLS 证书。", String(preview.missingMtls.count)))
                        .font(.callout).foregroundStyle(.orange)
                } else {
                    Text(L10n.text("预览有效。再次修改草稿或凭据后，需要重新预览。"))
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, 4)
        } label: {
            Label(L10n.text("授权变更预览"), systemImage: "checklist")
        }
        .accessibilityIdentifier("users.groups.previewSummary")
    }

    private func pairList(_ pairs: [AuthorizationPair]) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(Array(pairs.enumerated()), id: \.offset) { _, pair in
                    HStack(spacing: 8) {
                        Text(store.users.first(where: { $0.id == pair.userId })?.name ?? pair.userId)
                        Image(systemName: "arrow.right").foregroundStyle(.secondary)
                        Text(store.nodes.first(where: { $0.id == pair.nodeID })?.name ?? pair.nodeID)
                    }
                    .font(.caption)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .frame(maxHeight: 150)
    }

    private var mtlsBindings: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 12) {
                ForEach(Array(requiredMTLSPairs.enumerated()), id: \.offset) { index, pair in
                    let key = pairKey(pair.userId, pair.nodeID)
                    VStack(alignment: .leading, spacing: 6) {
                        Text("\(user.name) · \(store.nodes.first(where: { $0.id == pair.nodeID })?.name ?? pair.nodeID)")
                            .font(.callout.weight(.medium))
                        CredentialPickerView(
                            store: store,
                            selection: Binding(
                                get: { credentialIDsByPair[key] ?? "" },
                                set: { newValue in
                                    credentialIDsByPair[key] = newValue
                                    preview = nil
                                }
                            ),
                            kinds: ["tls_identity"],
                            ownerUserID: user.id,
                            title: L10n.text("mTLS 凭据")
                        )
                    }
                    if index < requiredMTLSPairs.count - 1 { Divider() }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, 4)
        } label: {
            Label(L10n.text("逐项选择用户自己的证书"), systemImage: "checkmark.seal")
        }
    }

    private func runPreview() {
        isPreviewing = true
        errorMessage = nil
        let bindings = requiredMTLSPairs.compactMap { pair -> AuthorizationMTLSBinding? in
            let key = pairKey(pair.userId, pair.nodeID)
            guard let credentialID = credentialIDsByPair[key],
                  let credential = store.credentials.first(where: {
                      $0.id == credentialID && $0.ownerUserId == user.id && $0.kind == "tls_identity" && !$0.archived
                  }) else { return nil }
            return AuthorizationMTLSBinding(userId: user.id, nodeID: pair.nodeID, credentialId: credential.id, credentialVersion: credential.latestVersion)
        }
        Task {
            defer { isPreviewing = false }
            do {
                user = try await store.authorizationUserDetail(user.id)
                let result = try await store.previewUserAuthorizationGroups(user, groupIDs: groupIDs.sorted(), mtlsBindings: bindings)
                preview = result
                requiredMTLSPairs = result.additions.filter { pair in
                    result.missingMtls.contains { $0.userId == pair.userId && $0.nodeID == pair.nodeID } || credentialIDsByPair[pairKey(pair.userId, pair.nodeID)] != nil
                }
            } catch {
                self.preview = nil
                errorMessage = L10n.text("无法生成预览。草稿已保留，请重新加载数据后再试。\n{0}", String(describing: (error.localizedDescription)))
            }
        }
    }

    private func save() {
        guard let preview, preview.missingMtls.isEmpty else { return }
        let bindings = preview.additions.compactMap { pair -> AuthorizationMTLSBinding? in
            let key = pairKey(pair.userId, pair.nodeID)
            guard let credentialID = credentialIDsByPair[key],
                  let credential = store.credentials.first(where: {
                      $0.id == credentialID && $0.ownerUserId == user.id && $0.kind == "tls_identity" && !$0.archived
                  }) else { return nil }
            return AuthorizationMTLSBinding(userId: user.id, nodeID: pair.nodeID, credentialId: credential.id, credentialVersion: credential.latestVersion)
        }
        isSaving = true
        errorMessage = nil
        Task {
            defer { isSaving = false }
            do {
                receipt = try await store.updateUserAuthorizationGroups(user, groupIDs: groupIDs.sorted(), previewToken: preview.previewToken, mtlsBindings: bindings)
            } catch {
                self.preview = nil
                errorMessage = L10n.text("提交前授权数据可能已变化。草稿仍保留，请重新预览。\n{0}", String(describing: (error.localizedDescription)))
            }
        }
    }

    private func pairKey(_ userID: String, _ nodeID: String) -> String { "\(userID)|\(nodeID)" }
}
