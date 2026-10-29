import SwiftUI

struct AuthorizationGroupMembersView: View {
    @Bindable var store: ManagementStore
    let group: AuthorizationGroupSummary
    let canEdit: Bool
    var onOpen: (String) -> Void
    var onEdit: (AuthorizationMemberEditorMode, Set<String>) -> Void
    @State private var mode = AuthorizationMemberEditorMode.members
    @State private var draft = AuthorizationRelationDraft()

    var body: some View {
        AuthorizationRelationBrowser(store: store, group: group, users: true, mode: $mode, draft: $draft, onOpen: onOpen, onEdit: canEdit ? onEdit : nil)
            .padding(16)
    }
}

struct AuthorizationGroupNodesView: View {
    @Bindable var store: ManagementStore
    let group: AuthorizationGroupSummary
    var onOpen: (String) -> Void
    @State private var mode = AuthorizationMemberEditorMode.members
    @State private var draft = AuthorizationRelationDraft()

    var body: some View {
        AuthorizationRelationBrowser(store: store, group: group, users: false, mode: $mode, draft: $draft, onOpen: onOpen)
    }
}
