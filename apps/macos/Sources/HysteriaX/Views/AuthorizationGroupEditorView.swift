import SwiftUI

struct AuthorizationGroupEditorView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ManagementStore
    @State private var group: AuthorizationGroupSummary?
    let intent: AuthorizationGroupEditorIntent
    var onSaved: (String) -> Void = { _ in }

    @State private var name: String
    @State private var originalUserIDs: Set<String>
    @State private var memberMode: AuthorizationMemberEditorMode
    @State private var showingImpact = false
    @State private var previewUserIDs: Set<String>?
    @State private var userIDs: Set<String>
    @State private var nodeIDs: Set<String>
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
        memberMode: AuthorizationMemberEditorMode = .members,
        removedUserIDs: Set<String> = [],
        onSaved: @escaping (String) -> Void = { _ in }
    ) {
        self.store = store
        _group = State(initialValue: group)
        self.intent = intent
        self.onSaved = onSaved
        _name = State(initialValue: group?.name ?? "")
        _originalUserIDs = State(initialValue: Set(group?.userIds ?? []))
        _memberMode = State(initialValue: memberMode)
        _userIDs = State(initialValue: Set(group?.userIds ?? []).subtracting(removedUserIDs))
        _nodeIDs = State(initialValue: Set(group?.nodeIds ?? []))
    }

    private var canEditUsers: Bool { intent == .create || intent == .users }
    private var canEditNodes: Bool { intent == .create || intent == .nodes }
    private var isReadyToSave: Bool { preview != nil && !isSaving && !isPreviewing }

    private var editorSize: CGSize {
        let hasDetails = preview != nil || receipt != nil
        switch intent {
        case .create:
            return CGSize(width: 760, height: 560)
        case .users:
            return CGSize(width: 880, height: 620)
        case .rename:
            return CGSize(width: 560, height: hasDetails ? 480 : 340)
        case .nodes:
            return CGSize(width: 680, height: hasDetails ? 580 : 380)
        }
    }

    var body: some View {
        directoryContent
            .task(id: store.dataRefreshToken) { await store.prepareDirectories([.nodes, .users]) }
            .disabled([ManagementStore.Directory.nodes, .users].contains { store.loadingDirectories.contains($0) })
    }

    private var directoryContent: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(receipt == nil ? title : L10n.text("授权组已更新"))
                .font(.title2.bold())

            if intent == .users, receipt == nil {
                HStack {
                    Text(group?.name ?? "").font(.headline)
                    Spacer()
                    Button(showingImpact ? L10n.text("返回成员编辑") : L10n.text("查看权限预览")) { showingImpact.toggle() }
                        .disabled(!showingImpact && preview == nil)
                        .accessibilityIdentifier("authorization.members.showImpact")
                }
                if showingImpact {
                    ScrollView {
                        VStack(alignment: .leading, spacing: 16) {
                            if let preview { previewSummary(preview) }
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }
                } else {
                    AuthorizationGroupMembersEditor(users: store.users, originalIDs: originalUserIDs, userIDs: $userIDs, mode: $memberMode)
                        .disabled(isPreviewing || isSaving)
                }
                Text(L10n.text("待添加 {0} 人 · 待移除 {1} 人", String(memberChanges.added.count), String(memberChanges.removed.count)))
                    .font(.callout.weight(.medium))
                    .accessibilityIdentifier("authorization.members.changeCount")
            } else {
                ScrollView {
                    editorContent.frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(maxHeight: .infinity)
            }

            if let errorMessage { Text(errorMessage).foregroundStyle(.red).textSelection(.enabled) }

            HStack {
                Button(receipt == nil ? L10n.text("取消") : L10n.text("完成"), role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                    .disabled(isSaving)
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
        .frame(width: editorSize.width, height: editorSize.height)
        .interactiveDismissDisabled(isSaving || (intent == .users && !memberChanges.changed.isEmpty))
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
        .onChange(of: name) { _, _ in
            if intent == .create || intent == .rename { invalidatePreview() }
        }
        .onChange(of: userIDs) { _, _ in
            if canEditUsers, previewUserIDs != userIDs { invalidatePreview() }
        }
        .onChange(of: nodeIDs) { _, _ in if canEditNodes { invalidatePreview() } }
    }

    private var memberChanges: AuthorizationMemberChanges { .init(original: originalUserIDs, proposed: userIDs) }

    @ViewBuilder
    private var editorContent: some View {
        if let receipt {
            AuthorizationMutationReceiptView(store: store, createdCredentials: receipt.createdCredentials, revocationJobIDs: receipt.revocationJobIds)
        } else {
            VStack(alignment: .leading, spacing: 16) {
                Text(description).font(.callout).foregroundStyle(.secondary)
                GroupBox {
                    VStack(alignment: .leading, spacing: 14) {
                        if intent == .create || intent == .rename {
                            LabeledContent(L10n.text("授权组名称")) {
                                TextField(L10n.text("授权组名称"), text: $name)
                                    .textFieldStyle(.roundedBorder)
                                    .accessibilityIdentifier("authorizationGroups.editor.name")
                            }
                        } else if let group { LabeledContent(L10n.text("授权组名称"), value: group.name) }
                        if canEditUsers {
                            selectionRow(title: L10n.text("成员用户"), count: userIDs.count, buttonTitle: L10n.text("选择用户…"), identifier: "authorizationGroups.editor.users") { selectingUsers = true }
                        } else { LabeledContent(L10n.text("成员用户"), value: L10n.text("{0} 位", String(userIDs.count))) }
                        if canEditNodes {
                            selectionRow(title: L10n.text("授权节点"), count: nodeIDs.count, buttonTitle: L10n.text("选择节点…"), identifier: "authorizationGroups.editor.nodes") { selectingNodes = true }
                        } else { LabeledContent(L10n.text("授权节点"), value: L10n.text("{0} 个", String(nodeIDs.count))) }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading).padding(8)
                    .disabled(isPreviewing || isSaving)
                }
                if let preview { previewSummary(preview) }
            }
        }
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
                Text(L10n.text("新增访问涉及 {0} 人 · 撤权涉及 {1} 人", String(preview.additionsUserCount ?? Set(preview.additions.map(\.userId)).count), String(preview.removalsUserCount ?? Set(preview.removals.map(\.userId)).count)))
                    .font(.caption).foregroundStyle(.secondary)
                if intent == .users, !memberChanges.removed.isEmpty {
                    let revoked = memberChanges.removed.intersection(preview.removals.map(\.userId)).count
                    Text(L10n.text("移除 {0} 位成员，其中 {1} 位将失去部分节点访问权限；其余成员的节点访问权限不变。", String(memberChanges.removed.count), String(revoked)))
                        .font(.callout)
                }
                if !preview.additions.isEmpty {
                    DisclosureGroup(L10n.text("查看新增授权（{0}）", String(preview.additions.count))) {
                        AuthorizationUserImpactList(pairs: preview.additions, users: store.users, nodes: store.nodes).padding(.top, 6)
                    }
                }
                if !preview.removals.isEmpty {
                    DisclosureGroup(L10n.text("查看撤权对象（{0}）", String(preview.removals.count))) {
                        AuthorizationUserImpactList(pairs: preview.removals, users: store.users, nodes: store.nodes).padding(.top, 6)
                    }
                }
                ManagedIdentityIssuanceSummary(immediate: preview.automaticIssueCount, pending: preview.pendingIssueCount)
                Text(L10n.text("预览有效。再次修改草稿或凭据后，需要重新预览。"))
                    .font(.caption).foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, 4)
        } label: {
            Label(L10n.text("授权变更预览"), systemImage: "checklist")
        }
        .accessibilityIdentifier("authorizationGroups.editor.previewSummary")
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
                        name = latest.name
                        userIDs = memberChanges.applying(to: Set(latest.userIds))
                        originalUserIDs = Set(latest.userIds)
                        nodeIDs = Set(latest.nodeIds)
                    }
                    if intent == .nodes {
                        userIDs = Set(latest.userIds)
                    }
                    if intent == .rename {
                        userIDs = Set(latest.userIds)
                        nodeIDs = Set(latest.nodeIds)
                    }
                }
                previewUserIDs = userIDs
                let result = try await store.previewAuthorizationGroup(
                    group: group,
                    name: proposedName,
                    userIDs: userIDs.sorted(),
                    nodeIDs: nodeIDs.sorted()
                )
                preview = result
                if intent == .users { showingImpact = true }
            } catch {
                preview = nil
                errorMessage = L10n.text("无法生成预览。草稿已保留，请重新加载数据后再试。\n{0}", String(describing: (error.localizedDescription)))
            }
        }
    }

    private func save() {
        guard let preview, !preview.previewToken.isEmpty else { return }
        isSaving = true
        errorMessage = nil
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
                        previewToken: preview.previewToken
                    )
                } else {
                    result = try await store.createAuthorizationGroup(
                        name: proposedName,
                        userIDs: userIDs.sorted(),
                        nodeIDs: nodeIDs.sorted(),
                        previewToken: preview.previewToken
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

    private func invalidatePreview() {
        guard receipt == nil else { return }
        preview = nil
        previewUserIDs = nil
        errorMessage = nil
    }
}
