import SwiftUI

struct AuthorizationGroupEditorView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ManagementStore
    let group: AuthorizationGroupSummary?
    let intent: AuthorizationGroupEditorIntent
    var onSaved: (String) -> Void = { _ in }

    @State private var name: String
    @State private var memberMode: AuthorizationMemberEditorMode
    @State private var draft: AuthorizationRelationDraft
    @State private var isSaving = false
    @State private var errorMessage: String?
    @State private var receipt: AuthorizationChangeReceipt?

    init(store: ManagementStore, group: AuthorizationGroupSummary?, intent: AuthorizationGroupEditorIntent,
         memberMode: AuthorizationMemberEditorMode = .members, removedUserIDs: Set<String> = [],
         onSaved: @escaping (String) -> Void = { _ in }) {
        self.store = store; self.group = group; self.intent = intent; self.onSaved = onSaved
        _name = State(initialValue: group?.name ?? "")
        _memberMode = State(initialValue: memberMode)
        var initial = AuthorizationRelationDraft()
        for id in removedUserIDs { initial.setMember(false, row: .placeholder(id), originallyMember: true) }
        _draft = State(initialValue: initial)
    }

    private var relations: Bool { intent == .users || intent == .nodes }
    private var isReadyToSave: Bool {
        guard !isSaving else { return false }
        if relations { return !draft.isEmpty }
        return !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && (intent == .create || name.trimmingCharacters(in: .whitespacesAndNewlines) != group?.name)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(receipt == nil ? title : L10n.text("授权变更已提交")).font(.title2.bold())
            if let receipt {
                Text(L10n.text("授权变更已提交")).accessibilityIdentifier("authorizationGroups.editor.resultSummary")
                AuthorizationMutationReceiptView(store: store, jobID: receipt.jobId)
            } else if relations, let group {
                Text(group.name).font(.headline)
                AuthorizationRelationBrowser(store: store, group: group, users: intent == .users, editing: true, mode: $memberMode, draft: $draft)
                    .disabled(isSaving)
            } else {
                Text(intent == .create ? L10n.text("填写名称创建空组，再分别添加成员和节点。") : L10n.text("修改授权组名称后保存。"))
                    .foregroundStyle(.secondary)
                LabeledContent(L10n.text("授权组名称")) {
                    TextField(L10n.text("授权组名称"), text: $name).textFieldStyle(.roundedBorder)
                        .accessibilityIdentifier("authorizationGroups.editor.name")
                }.disabled(isSaving)
            }
            if let errorMessage { Text(errorMessage).foregroundStyle(.red).textSelection(.enabled) }
            Spacer(minLength: 0)
            HStack {
                Button(receipt == nil ? L10n.text("取消") : L10n.text("完成"), role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction).disabled(isSaving)
                Spacer()
                if receipt == nil {
                    Button(isSaving ? L10n.text("正在保存…") : L10n.text("保存授权组")) { save() }
                        .disabled(!isReadyToSave || !store.isConnected).keyboardShortcut(.defaultAction)
                        .accessibilityIdentifier("authorizationGroups.editor.save")
                }
            }
        }
        .padding(24)
        .frame(width: relations ? 880 : 560, height: relations ? 620 : receipt == nil ? 340 : 480)
        .interactiveDismissDisabled(isSaving || (receipt == nil && !draft.isEmpty))
    }

    private var title: String {
        switch intent {
        case .create: L10n.text("创建授权组")
        case .rename: L10n.text("编辑授权组名称")
        case .users: L10n.text("管理授权组用户")
        case .nodes: L10n.text("管理授权组节点")
        }
    }

    private func save() {
        guard isReadyToSave else { return }
        isSaving = true; errorMessage = nil
        Task {
            defer { isSaving = false }
            do {
                if let group {
                    receipt = try await store.updateAuthorizationGroup(group,
                        name: intent == .rename ? name : nil,
                        addUserIDs: intent == .users ? draft.added.keys.sorted() : [],
                        removeUserIDs: intent == .users ? draft.removed.keys.sorted() : [],
                        addNodeIDs: intent == .nodes ? draft.added.keys.sorted() : [],
                        removeNodeIDs: intent == .nodes ? draft.removed.keys.sorted() : [])
                    onSaved(group.id)
                } else {
                    receipt = try await store.createAuthorizationGroup(name: name)
                    if let receipt { onSaved(receipt.resourceId) }
                }
            } catch {
                errorMessage = L10n.text("保存失败，草稿已保留。请检查错误后重试；如有版本冲突，请重新打开编辑。\n{0}", error.localizedDescription)
            }
        }
    }
}
