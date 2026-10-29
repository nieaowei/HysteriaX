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
    @State private var userIDs: Set<String>
    @State private var nodeIDs: Set<String>
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
    private var isReadyToSave: Bool { !isSaving }

    private var editorSize: CGSize {
        let hasDetails = receipt != nil
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

    private var requiredDirectories: [ManagementStore.Directory] {
        switch intent {
        case .create: [.nodes, .users]
        case .users: [.users]
        case .nodes: [.nodes]
        case .rename: []
        }
    }

    var body: some View {
        directoryContent
            .task(id: store.dataRefreshToken) {
                guard receipt == nil else { return }
                await store.prepareDirectories(requiredDirectories)
            }
            .disabled(receipt == nil && requiredDirectories.contains { store.loadingDirectories.contains($0) })
    }

    private var directoryContent: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(receipt == nil ? title : L10n.text("授权组已更新"))
                .font(.title2.bold())

            if intent == .users, receipt == nil {
                Text(group?.name ?? "").font(.headline)
                AuthorizationGroupMembersEditor(users: store.users, originalIDs: originalUserIDs, userIDs: $userIDs, mode: $memberMode)
                    .disabled(isSaving)
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

    }

    private var memberChanges: AuthorizationMemberChanges { .init(original: originalUserIDs, proposed: userIDs) }

    @ViewBuilder
    private var editorContent: some View {
        if let receipt {
            VStack(alignment: .leading, spacing: 12) {
                Text(L10n.text("新增 {0} 条连接授权 · 移除 {1} 条连接授权", String(receipt.additionsCount), String(receipt.removalsCount)))
                    .font(.callout.weight(.medium))
                    .accessibilityIdentifier("authorizationGroups.editor.resultSummary")
                AuthorizationMutationReceiptView(store: store, createdCredentials: receipt.createdCredentials, revocationJobIDs: receipt.revocationJobIds)
            }
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
                    .disabled(isSaving)
                }
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
        case .rename: L10n.text("修改授权组名称后保存。")
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

    private func save() {
        guard isReadyToSave else { return }
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
                        nodeIDs: nodeIDs.sorted()
                    )
                } else {
                    result = try await store.createAuthorizationGroup(
                        name: proposedName,
                        userIDs: userIDs.sorted(),
                        nodeIDs: nodeIDs.sorted()
                    )
                }
                receipt = result
                if let groupID = result.group?.id ?? group?.id { onSaved(groupID) }
            } catch {
                errorMessage = L10n.text("保存失败，草稿已保留。请检查错误后重试；如有版本冲突，请重新打开编辑。\n{0}", String(describing: (error.localizedDescription)))
            }
        }
    }

    private var proposedName: String {
        name
    }

}
