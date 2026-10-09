import Foundation
import Observation

/// State shared by the two toolbar tabs so each list keeps its selection,
/// search query, and native table sort while the other tab is visible.
@MainActor
@Observable
final class AuthorizationPageState {
    var selectedUserID: String?
    var userSearchText = ""
    var userPage = 1
    var userPageSize = 50
    var userSortOrder = [KeyPathComparator<UserSummary>(\.name)]

    var selectedGroupID: String?
    var groupSearchText = ""
    var groupSortOrder = [KeyPathComparator<AuthorizationGroupSummary>(\.name)]
    // Toolbar actions may outlive a tab view; keep their presentation state page-owned.
    var groupEditorTarget: AuthorizationGroupEditorTarget?
}

struct AuthorizationGroupEditorTarget: Identifiable {
    let group: AuthorizationGroupSummary?
    let intent: AuthorizationGroupEditorIntent
    var memberMode: AuthorizationMemberEditorMode = .members
    var removedUserIDs: Set<String> = []
    var id: String { "\(group?.id ?? "new"):\(intent.rawValue)" }
}

enum AuthorizationGroupEditorIntent: String, Equatable {
    case create, rename, users, nodes
}

enum AuthorizationMemberEditorMode: String, Hashable {
    case members, add, changes
}
