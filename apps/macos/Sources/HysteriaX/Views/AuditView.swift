import SwiftUI

struct AuditView: View {
    @Bindable var store: ManagementStore
    @State private var searchText = ""
    @State private var sortOrder = [KeyPathComparator<AuditSummary>(\.createdAt, order: .reverse)]

    private var visibleRecords: [AuditSummary] {
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        let filtered = store.auditRecords.filter { record in
            query.isEmpty
                || record.localizedAction.localizedCaseInsensitiveContains(query)
                || record.localizedEntityType.localizedCaseInsensitiveContains(query)
                || record.localizedActor.localizedCaseInsensitiveContains(query)
                || record.action.localizedCaseInsensitiveContains(query)
                || record.actor.localizedCaseInsensitiveContains(query)
                || record.entityType.localizedCaseInsensitiveContains(query)
                || record.entityID.localizedCaseInsensitiveContains(query)
        }
        return filtered.sorted(using: sortOrder)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Table(visibleRecords, sortOrder: $sortOrder) {
                TableColumn("操作", value: \.localizedAction)
                TableColumn("对象", value: \.localizedEntityType)
                TableColumn("对象 ID", value: \.entityID)
                TableColumn("操作者", value: \.localizedActor)
                TableColumn("时间") { record in Text(DateDisplayText.local(record.createdAt)) }
            }
            .overlay {
                if store.auditRecords.isEmpty {
                    ContentUnavailableView("暂无审计记录", systemImage: "clock.arrow.circlepath")
                } else if visibleRecords.isEmpty {
                    ContentUnavailableView("没有匹配的审计记录", systemImage: "magnifyingglass")
                }
            }
        }
        .searchable(text: $searchText, prompt: "搜索审计")
    }
}

private extension AuditSummary {
    var localizedAction: String { AuditDisplayText.action(action) }
    var localizedEntityType: String { AuditDisplayText.entityType(entityType) }
    var localizedActor: String { AuditDisplayText.actor(actor) }
}
