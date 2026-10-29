import Foundation

struct AuthorizationRelationRow: Identifiable, Sendable {
    let id: String
    let name: String
    let enabled: Bool?
    let expiresAt: String?
    let state: String?

    static func placeholder(_ id: String) -> Self {
        .init(id: id, name: id, enabled: nil, expiresAt: nil, state: nil)
    }
}

struct AuthorizationRelationPage: Sendable {
    let rows: [AuthorizationRelationRow]
    let total: Int
    let page: Int
    let revision: Int
}

/// Store only explicit intent. Unvisited pages never participate in the delta.
struct AuthorizationRelationDraft {
    var added: [String: AuthorizationRelationRow] = [:]
    var removed: [String: AuthorizationRelationRow] = [:]
    var isEmpty: Bool { added.isEmpty && removed.isEmpty }
    var rows: [AuthorizationRelationRow] { Array(added.values) + Array(removed.values) }

    mutating func setMember(_ member: Bool, row: AuthorizationRelationRow, originallyMember: Bool) {
        added[row.id] = nil
        removed[row.id] = nil
        guard member != originallyMember else { return }
        if member { added[row.id] = row } else { removed[row.id] = row }
    }

    mutating func undo(_ ids: Set<String>) {
        for id in ids { added[id] = nil; removed[id] = nil }
    }
}
