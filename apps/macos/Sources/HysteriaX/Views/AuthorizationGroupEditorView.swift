import SwiftUI

struct AuthorizationGroupEditorView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ManagementStore
    @State private var group: AuthorizationGroupSummary?
    let intent: AuthorizationGroupEditorIntent
    var onSaved: (String) -> Void = { _ in }

    @State private var name: String
    @State private var userIDs: Set<String>
    @State private var nodeIDs: Set<String>
    @State private var credentialIDsByPair: [String: String] = [:]
    @State private var requiredMTLSPairs: [AuthorizationPair] = []
    @State private var preview: AuthorizationChangePreview?
    @State private var isPreviewing = false
    @State private var isSaving = false
    @State private var errorMessage: String?
    @State private var selectingUsers = false
    @State private var selectingNodes = false
    @State private var receipt: AuthorizationGroupMutationResponse?

    init(
        store: ManagementStore,
        group: AuthorizationGroupSummary?,
        intent: AuthorizationGroupEditorIntent,
        onSaved: @escaping (String) -> Void = { _ in }
    ) {
        self.store = store
        _group = State(initialValue: group)
        self.intent = intent
        self.onSaved = onSaved
        _name = State(initialValue: group?.name ?? "")
        _userIDs = State(initialValue: Set(group?.userIds ?? []))
        _nodeIDs = State(initialValue: Set(group?.nodeIds ?? []))
    }

    private var canEditUsers: Bool { intent == .create || intent == .users }
    private var canEditNodes: Bool { intent == .create || intent == .nodes }
    private var isReadyToSave: Bool { preview != nil && preview?.missingMtls.isEmpty == true && !isSaving && !isPreviewing }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(receipt == nil ? title : L10n.text("授权组已更新"))
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
                Text(description)
                    .font(.callout).foregroundStyle(.secondary)

                GroupBox {
                    VStack(alignment: .leading, spacing: 14) {
                        if intent == .create || intent == .rename {
                            LabeledContent(L10n.text("授权组名称")) {
                                TextField(L10n.text("授权组名称"), text: $name)
                                    .textFieldStyle(.roundedBorder)
                                    .accessibilityIdentifier("authorizationGroups.editor.name")
                            }
                        } else if let group {
                            LabeledContent(L10n.text("授权组名称"), value: group.name)
                        }
                        if canEditUsers {
                            selectionRow(title: L10n.text("成员用户"), count: userIDs.count,
                                buttonTitle: L10n.text("选择用户…"), identifier: "authorizationGroups.editor.users") { selectingUsers = true }
                        } else {
                            LabeledContent(L10n.text("成员用户"), value: L10n.text("{0} 位", String(userIDs.count)))
                        }
                        if canEditNodes {
                            selectionRow(title: L10n.text("授权节点"), count: nodeIDs.count,
                                buttonTitle: L10n.text("选择节点…"), identifier: "authorizationGroups.editor.nodes") { selectingNodes = true }
                        } else {
                            LabeledContent(L10n.text("授权节点"), value: L10n.text("{0} 个", String(nodeIDs.count)))
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(8)
                    .disabled(isPreviewing || isSaving)
                }

                if let preview {
                    previewSummary(preview)
                }

                if !requiredMTLSPairs.isEmpty {
                    mtlsBindingPicker
                        .accessibilityElement(children: .contain)
                        .accessibilityIdentifier("authorizationGroups.editor.mtlsBindings")
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
                        .disabled(isPreviewing || isSaving || !store.isConnected || (intent == .create && name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty))
                        .accessibilityIdentifier("authorizationGroups.editor.preview")
                    Button(isSaving ? L10n.text("正在保存…") : L10n.text("保存授权组")) { save() }
                        .disabled(!isReadyToSave || !store.isConnected || (intent == .create && name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty))
                        .keyboardShortcut(.defaultAction)
                        .accessibilityIdentifier("authorizationGroups.editor.save")
                }
            }
        }
        .padding(24)
        .frame(width: 760, height: 560)
        .interactiveDismissDisabled(isSaving)
        .sheet(isPresented: $selectingUsers) {
            AuthorizationMultiSelectSheet(
                title: L10n.text("选择成员用户"),
                options: store.users.map { AuthorizationPickerOption(id: $0.id, name: $0.name, detail: $0.enabled ? nil : L10n.text("已停用")) },
                selection: $userIDs
            )
        }
        .sheet(isPresented: $selectingNodes) {
            AuthorizationMultiSelectSheet(
                title: L10n.text("选择授权节点"),
                options: store.nodes.map { AuthorizationPickerOption(id: $0.id, name: $0.name, detail: $0.mtlsRequired == true ? "mTLS" : nil) },
                selection: $nodeIDs
            )
        }
        .onChange(of: name) { _, _ in invalidatePreview(clearMTLS: true) }
        .onChange(of: userIDs) { _, _ in if canEditUsers { invalidatePreview(clearMTLS: true) } }
        .onChange(of: nodeIDs) { _, _ in if canEditNodes { invalidatePreview(clearMTLS: true) } }
    }

    private var title: String {
        switch intent {
        case .create: L10n.text("创建授权组")
        case .rename: L10n.text("编辑授权组名称")
        case .users: L10n.text("管理授权组用户")
        case .nodes: L10n.text("管理授权组节点")
        }
    }

    private var description: String {
        switch intent {
        case .create: L10n.text("选择成员和节点；可以先创建空组，再分别管理用户或节点。")
        case .rename: L10n.text("保存前预览连接授权的影响。")
        case .users: L10n.text("调整本组成员；其他授权组授予的权限会保留。")
        case .nodes: L10n.text("调整本组节点；同一连接仍由其他授权组授予时不会撤权。")
        }
    }

    private func selectionRow(title: String, count: Int, buttonTitle: String, identifier: String, action: @escaping () -> Void) -> some View {
        HStack {
            LabeledContent(title, value: L10n.text("{0} 项已选", String(count)))
            Button(buttonTitle, action: action)
                .accessibilityIdentifier(identifier)
        }
    }

    private func previewSummary(_ preview: AuthorizationChangePreview) -> some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 10) {
                Text(L10n.text("新增 {0} 条连接授权 · 移除 {1} 条连接授权", String(preview.additionsCount), String(preview.removalsCount)))
                    .font(.callout.weight(.medium))
                if !preview.additions.isEmpty {
                    DisclosureGroup(L10n.text("查看新增授权（{0}）", String(preview.additions.count))) {
                        pairList(preview.additions).padding(.top, 6)
                    }
                }
                if !preview.removals.isEmpty {
                    DisclosureGroup(L10n.text("查看撤权对象（{0}）", String(preview.removals.count))) {
                        pairList(preview.removals).padding(.top, 6)
                    }
                }
                if preview.missingMtls.isEmpty {
                    Text(L10n.text("预览有效。再次修改草稿或凭据后，需要重新预览。"))
                        .font(.caption).foregroundStyle(.secondary)
                } else {
                    Text(L10n.text("{0} 条新增连接需要该用户自己的 mTLS 证书。选择证书后请再次预览。", String(preview.missingMtls.count)))
                        .font(.callout).foregroundStyle(.orange)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, 4)
        } label: {
            Label(L10n.text("授权变更预览"), systemImage: "checklist")
        }
        .accessibilityIdentifier("authorizationGroups.editor.previewSummary")
    }

    private func pairList(_ pairs: [AuthorizationPair]) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(Array(pairs.enumerated()), id: \.offset) { _, pair in
                    HStack(spacing: 8) {
                        Text(userName(pair.userId))
                        Image(systemName: "arrow.right").foregroundStyle(.secondary)
                        Text(nodeName(pair.nodeID))
                    }
                    .font(.caption)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .frame(maxHeight: 150)
    }

    private var mtlsBindingPicker: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 12) {
                ForEach(Array(requiredMTLSPairs.enumerated()), id: \.offset) { _, pair in
                    let key = pairKey(pair.userId, pair.nodeID)
                    VStack(alignment: .leading, spacing: 6) {
                        Text("\(userName(pair.userId)) · \(nodeName(pair.nodeID))")
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
                            ownerUserID: pair.userId,
                            title: L10n.text("{0} 的 mTLS 凭据", String(describing: (userName(pair.userId))))
                        )
                    }
                    if pairKey(pair.userId, pair.nodeID) != pairKey(requiredMTLSPairs.last?.userId ?? "", requiredMTLSPairs.last?.nodeID ?? "") {
                        Divider()
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, 4)
        } label: {
            Label(L10n.text("逐项选择用户自己的证书"), systemImage: "checkmark.seal")
        }
    }

    private func runPreview() {
        guard intent == .create || intent == .rename || intent == .users || intent == .nodes else { return }
        isPreviewing = true
        errorMessage = nil
        Task {
            defer { isPreviewing = false }
            do {
                if let group {
                    let latest = try await store.authorizationGroupDetail(group.id)
                    self.group = latest
                    if intent == .users {
                        if nodeIDs != Set(latest.nodeIds) { credentialIDsByPair = [:]; requiredMTLSPairs = [] }
                        nodeIDs = Set(latest.nodeIds)
                    }
                    if intent == .nodes {
                        if userIDs != Set(latest.userIds) { credentialIDsByPair = [:]; requiredMTLSPairs = [] }
                        userIDs = Set(latest.userIds)
                    }
                    if intent == .rename {
                        userIDs = Set(latest.userIds)
                        nodeIDs = Set(latest.nodeIds)
                    }
                }
                let requestedBindings = bindingRequests(for: requiredMTLSPairs)
                let result = try await store.previewAuthorizationGroup(
                    group: group,
                    name: proposedName,
                    userIDs: userIDs.sorted(),
                    nodeIDs: nodeIDs.sorted(),
                    mtlsBindings: requestedBindings
                )
                preview = result
                requiredMTLSPairs = result.additions.filter { pair in
                    result.missingMtls.contains { $0.userId == pair.userId && $0.nodeID == pair.nodeID } || credentialIDsByPair[pairKey(pair.userId, pair.nodeID)] != nil
                }
            } catch {
                preview = nil
                errorMessage = L10n.text("无法生成预览。草稿已保留，请重新加载数据后再试。\n{0}", String(describing: (error.localizedDescription)))
            }
        }
    }

    private func save() {
        guard let preview, preview.missingMtls.isEmpty, !preview.previewToken.isEmpty else { return }
        isSaving = true
        errorMessage = nil
        let bindings = bindingRequests(for: preview.additions)
        Task {
            defer { isSaving = false }
            do {
                let result: AuthorizationGroupMutationResponse
                if let group {
                    result = try await store.updateAuthorizationGroup(
                        group,
                        name: proposedName,
                        userIDs: userIDs.sorted(),
                        nodeIDs: nodeIDs.sorted(),
                        previewToken: preview.previewToken,
                        mtlsBindings: bindings
                    )
                } else {
                    result = try await store.createAuthorizationGroup(
                        name: proposedName,
                        userIDs: userIDs.sorted(),
                        nodeIDs: nodeIDs.sorted(),
                        previewToken: preview.previewToken,
                        mtlsBindings: bindings
                    )
                }
                receipt = result
                if let groupID = result.group?.id ?? group?.id { onSaved(groupID) }
            } catch {
                self.preview = nil
                errorMessage = L10n.text("提交前授权数据可能已变化。草稿仍保留，请重新预览。\n{0}", String(describing: (error.localizedDescription)))
            }
        }
    }

    private var proposedName: String {
        name
    }

    private func invalidatePreview(clearMTLS: Bool) {
        guard receipt == nil else { return }
        preview = nil
        errorMessage = nil
        if clearMTLS {
            credentialIDsByPair = [:]
            requiredMTLSPairs = []
        }
    }

    private func bindingRequests(for pairs: [AuthorizationPair]) -> [AuthorizationMTLSBinding] {
        pairs.compactMap { pair in
            let key = pairKey(pair.userId, pair.nodeID)
            guard let credentialID = credentialIDsByPair[key],
                  let credential = store.credentials.first(where: {
                      $0.id == credentialID && $0.ownerUserId == pair.userId && $0.kind == "tls_identity" && !$0.archived
                  }) else { return nil }
            return AuthorizationMTLSBinding(
                userId: pair.userId,
                nodeID: pair.nodeID,
                credentialId: credential.id,
                credentialVersion: credential.latestVersion
            )
        }
    }

    private func pairKey(_ userID: String, _ nodeID: String) -> String { "\(userID)|\(nodeID)" }

    private func userName(_ id: String) -> String { store.users.first(where: { $0.id == id })?.name ?? id }
    private func nodeName(_ id: String) -> String { store.nodes.first(where: { $0.id == id })?.name ?? id }
}
