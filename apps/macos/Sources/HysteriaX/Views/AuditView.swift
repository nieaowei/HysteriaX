import SwiftUI

struct AuditView: View {
    @Bindable var store: ManagementStore
    @State private var searchText = ""
    @State private var sortOrder = [KeyPathComparator<AuditSummary>(\.createdAt, order: .reverse)]

    private var visibleRecords: [AuditSummary] {
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        let filtered = store.auditRecords.filter { record in
            query.isEmpty
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
                TableColumn("操作者", value: \.actor)
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
    var localizedAction: String {
        let labels = [
            "node.created": "创建节点",
            "node.updated": "更新节点",
            "node.restricted": "限制节点代理",
            "node.restored": "恢复节点代理",
            "node.usage_reset": "重置节点套餐用量",
            "node.usage_corrected": "校正节点套餐用量",
            "node.deleted": "删除节点",
            "node.uninstall_requested": "请求卸载节点",
            "user.created": "创建用户",
            "user.updated": "更新用户",
            "user.deleted": "删除用户",
            "user.assigned": "分配用户到节点",
            "user.unassigned": "撤销节点分配",
            "user.client_certificate_updated": "更新客户端证书",
            "user.credentials_rotated": "轮换连接凭据",
            "user.subscription_rotated": "轮换订阅令牌",
            "user.quota_reset": "重置流量额度",
            "resource.created": "上传配置资源",
            "resource.deleted": "删除配置资源",
            "admin_token.created": "创建管理员令牌",
            "admin_token.revoked": "撤销管理员令牌",
        ]
        return labels[action] ?? action
    }

    var localizedEntityType: String {
        switch entityType {
        case "node": "节点"
        case "user": "用户"
        case "resource": "资源"
        case "admin_token": "管理员令牌"
        default: entityType
        }
    }
}
