import Foundation

struct OverviewDestination: Equatable {
    let section: String
    let entityID: String
}

extension OverviewIssue: Identifiable {}
extension OverviewNode: Identifiable { var id: String { nodeID } }
extension OverviewBucket: Identifiable {
    var id: String { start }
    var date: Date { DateDisplayText.parse(start) ?? .distantPast }
}

enum OverviewDisplay {
    static func status(_ value: String) -> String {
        switch value {
        case "healthy": "健康"
        case "checking": "确认中"
        case "failed": "失败"
        case "fresh": "最新"
        case "stale": "已陈旧"
        case "unconfigured": "未配置"
        case "unknown": "未知"
        case "ok": "成功"
        case "deployed": "已部署"
        case "new": "未部署"
        case "needs_fingerprint": "待确认指纹"
        case "ready": "待部署"
        case "syncing": "同步中"
        case "rolled_back": "已回滚"
        case "deleting": "卸载中"
        case "unreachable": "无法连接"
        case "fingerprint_changed": "指纹变更"
        case "sync_failed": "同步失败"
        case "rollback_failed": "回滚失败"
        case "delete_failed": "卸载失败"
        case "drift": "配置漂移"
        default: value
        }
    }
    static func reason(_ issue: OverviewIssue) -> String {
        switch issue.kind {
        case "fingerprint_changed", "sync_failed", "rollback_failed", "delete_failed", "drift", "unreachable": status(issue.kind)
        default: issue.reason
        }
    }
    static func bytes(_ value: Int64) -> String { ByteCountFormatter.string(fromByteCount: value, countStyle: .decimal) }
    static func milliseconds(_ value: Double?) -> String { value.map { String(format: "%.0f ms", $0) } ?? "—" }
}
