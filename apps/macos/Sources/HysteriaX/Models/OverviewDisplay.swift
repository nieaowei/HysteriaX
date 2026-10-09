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
        case "healthy": L10n.text("健康")
        case "checking": L10n.text("确认中")
        case "failed": L10n.text("失败")
        case "fresh": L10n.text("最新")
        case "stale": L10n.text("已陈旧")
        case "unconfigured": L10n.text("未配置")
        case "unknown": L10n.text("未知")
        case "ok": L10n.text("成功")
        case "deployed": L10n.text("已部署")
        case "new": L10n.text("未部署")
        case "needs_fingerprint": L10n.text("待确认指纹")
        case "ready": L10n.text("待部署")
        case "syncing": L10n.text("同步中")
        case "rolled_back": L10n.text("已回滚")
        case "deleting": L10n.text("卸载中")
        case "unreachable": L10n.text("无法连接")
        case "fingerprint_changed": L10n.text("指纹变更")
        case "sync_failed": L10n.text("同步失败")
        case "rollback_failed": L10n.text("回滚失败")
        case "delete_failed": L10n.text("卸载失败")
        case "drift": L10n.text("配置漂移")
        default: value
        }
    }
    static func reason(_ issue: OverviewIssue) -> String {
        switch issue.kind {
        case "fingerprint_changed", "sync_failed", "rollback_failed", "delete_failed", "drift", "unreachable": status(issue.kind)
        default: issue.reason
        }
    }
    static func bytes(_ value: Int64) -> String { TrafficUnits.display(value) }
    static func milliseconds(_ value: Double?) -> String { value.map { String(format: "%.0f ms", $0) } ?? "—" }
}
