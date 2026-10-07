import Foundation

/// Membership intent, independent of row selection and the currently visible page.
struct AuthorizationMemberChanges {
    let original: Set<String>
    let proposed: Set<String>

    var added: Set<String> { proposed.subtracting(original) }
    var removed: Set<String> { original.subtracting(proposed) }
    var changed: Set<String> { added.union(removed) }

    func applying(to latest: Set<String>) -> Set<String> {
        latest.union(added).subtracting(removed)
    }

    func undoing(_ ids: Set<String>) -> Set<String> {
        proposed.subtracting(ids).union(original.intersection(ids))
    }
}

struct AuthorizationMemberRow: Identifiable {
    let id: String
    let name: String
    let enabled: Bool?
    let expiresAt: String
    let change: String

    var status: String {
        guard let enabled else { return L10n.text("未知") }
        return enabled ? L10n.text("已启用") : L10n.text("已停用")
    }

    static func make(users: [UserSummary], ids: Set<String>, changes: AuthorizationMemberChanges? = nil) -> [Self] {
        let lookup = Dictionary(users.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        let added = changes?.added ?? []
        let removed = changes?.removed ?? []
        return ids.map { id in
            let user = lookup[id]
            return Self(id: id, name: user?.name ?? id, enabled: user?.enabled,
                        expiresAt: user?.expiresAt ?? "",
                        change: added.contains(id) ? L10n.text("待添加") : removed.contains(id) ? L10n.text("待移除") : "")
        }
    }
}

/// Table only receives visible IDs; merging avoids losing selections on other pages.
enum AuthorizationMemberSelection {
    static func merging(_ visibleSelection: Set<String>, visibleIDs: Set<String>, into selection: Set<String>) -> Set<String> {
        selection.subtracting(visibleIDs).union(visibleSelection.intersection(visibleIDs))
    }
}

struct AuthorizationUserImpact: Identifiable {
    let id: String
    let nodeIDs: [String]

    static func grouped(_ pairs: [AuthorizationPair]) -> [Self] {
        Dictionary(grouping: pairs, by: \.userId).map { id, pairs in
            Self(id: id, nodeIDs: Set(pairs.map(\.nodeID)).sorted())
        }.sorted { $0.id < $1.id }
    }
}
