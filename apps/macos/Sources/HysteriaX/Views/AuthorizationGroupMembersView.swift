import SwiftUI

struct AuthorizationGroupMembersView: View {
    let group: AuthorizationGroupSummary
    let users: [UserSummary]
    let canEdit: Bool
    var onOpen: (String) -> Void
    var onEdit: (AuthorizationMemberEditorMode, Set<String>) -> Void
    @State private var selection: Set<String> = []

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            DetailHeaderLayout {
                Text(L10n.text("成员用户（{0}）", String(group.userCount))).font(.headline)
                HStack {
                    Button(L10n.text("添加成员"), systemImage: "person.badge.plus") { onEdit(.add, []) }
                        .disabled(!canEdit)
                        .accessibilityIdentifier("authorizationGroups.addMembers")
                    Button(L10n.text("移除所选 {0} 人", String(selection.count)), role: .destructive) { onEdit(.changes, selection) }
                        .disabled(selection.isEmpty || !canEdit)
                        .accessibilityIdentifier("authorizationGroups.removeMembers")
                }
            }
            AuthorizationMemberBrowser(
                rows: AuthorizationMemberRow.make(users: users, ids: Set(group.userIds)), selection: $selection, minimumTableHeight: 80,
                onOpen: onOpen, onRemove: canEdit ? { ids in onEdit(.changes, ids) } : nil
            )
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .padding(16)
    }
}
