import Foundation
import Observation

/// State shared by the two toolbar tabs so each list keeps its selection,
/// search query, and native table sort while the other tab is visible.
@MainActor
@Observable
final class AuthorizationPageState {
    var selectedUserID: String?
    var userSearchText = ""
    var userSortOrder = [KeyPathComparator<UserSummary>(\.name)]

    var selectedGroupID: String?
    var groupSearchText = ""
    var groupSortOrder = [KeyPathComparator<AuthorizationGroupSummary>(\.name)]
}
