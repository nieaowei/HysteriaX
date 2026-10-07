import SwiftUI

struct AuthorizationGroupMembersEditor: View {
    let users: [UserSummary]
    let originalIDs: Set<String>
    @Binding var userIDs: Set<String>
    @Binding var mode: AuthorizationMemberEditorMode
    @State private var memberSelection: Set<String> = []
    @State private var candidateSelection: Set<String> = []
    @State private var changeSelection: Set<String> = []

    private var changes: AuthorizationMemberChanges { .init(original: originalIDs, proposed: userIDs) }
    private var rows: [AuthorizationMemberRow] {
        let ids: Set<String>
        switch mode {
        case .members: ids = userIDs
        case .add: ids = Set(users.map(\.id)).subtracting(userIDs)
        case .changes: ids = changes.changed
        }
        return AuthorizationMemberRow.make(users: users, ids: ids, changes: changes)
    }
    private var selection: Binding<Set<String>> {
        switch mode {
        case .members: return $memberSelection
        case .add: return $candidateSelection
        case .changes: return $changeSelection
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Picker(L10n.text("成员管理"), selection: $mode) {
                    Text(L10n.text("成员（{0}）", String(userIDs.count))).tag(AuthorizationMemberEditorMode.members)
                    Text(L10n.text("添加成员")).tag(AuthorizationMemberEditorMode.add)
                    Text(L10n.text("仅看变更（{0}）", String(changes.changed.count))).tag(AuthorizationMemberEditorMode.changes)
                }
                .pickerStyle(.segmented)
                .accessibilityIdentifier("authorization.members.mode")
                switch mode {
                case .members:
                    Button(L10n.text("移除所选 {0} 人", String(memberSelection.count)), role: .destructive) {
                        userIDs.subtract(memberSelection)
                        memberSelection = []
                    }
                    .disabled(memberSelection.isEmpty)
                    .accessibilityIdentifier("authorization.members.remove")
                case .add:
                    Button(L10n.text("添加所选 {0} 人", String(candidateSelection.count))) {
                        userIDs.formUnion(candidateSelection)
                        candidateSelection = []
                    }
                    .disabled(candidateSelection.isEmpty)
                    .accessibilityIdentifier("authorization.members.add")
                case .changes:
                    Button(L10n.text("撤销所选变更")) {
                        userIDs = changes.undoing(changeSelection)
                        changeSelection = []
                    }
                    .disabled(changeSelection.isEmpty)
                    .accessibilityIdentifier("authorization.members.undo")
                }
            }
            AuthorizationMemberBrowser(rows: rows, selection: selection, showsChanges: true,
                onRemove: mode == .members ? { ids in userIDs.subtract(ids) } : nil,
                onUndo: mode == .changes ? { ids in userIDs = changes.undoing(ids) } : nil)
                .id(mode)
            Text(L10n.text("选择会跨搜索和分页保留；添加、移除只修改草稿，预览后保存才会生效。"))
                .font(.caption).foregroundStyle(.secondary)
        }
    }
}
