import Foundation

struct NodePackageDraft {
    var hasExpiry = false
    var expiry = Date().addingTimeInterval(30 * 86400)
    var hasQuota = false
    var quotaGB = ""
    var cycle = "fixed"
    var resetDay = 1
    var timezone = "Asia/Shanghai"
    var interface = ""
    var direction = "both"
    var warningDays = 7
    var warningPercent = 80

    init(_ package: NodePackage? = nil) {
        guard let package else { return }
        hasExpiry = package.expiresAt != nil
        expiry = PackageDisplay.date(package.expiresAt) ?? expiry
        hasQuota = package.quotaBytes != nil
        quotaGB = package.quotaBytes.map(Self.gbText) ?? ""
        cycle = package.cycle
        resetDay = package.resetDay
        timezone = package.timezone
        interface = package.interface ?? ""
        direction = package.direction
        warningDays = package.expiryWarningDays
        warningPercent = package.trafficWarningPercent
    }

    static func gbText(_ bytes: Int64) -> String {
        NSDecimalNumber(decimal: Decimal(bytes) / 1_000_000_000).stringValue
    }

    static func bytes(_ text: String) throws -> Int64 {
        let normalized = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard normalized.count <= 64,
              normalized.range(of: #"^[0-9]+(?:\.[0-9]{1,9})?$"#, options: .regularExpression) != nil,
              let gb = Decimal(string: normalized, locale: Locale(identifier: "en_US_POSIX")), gb >= 0 else {
            throw APIClientError.server("流量须为非负 GB 数值，使用小数点。")
        }
        let bytes = gb * 1_000_000_000
        guard bytes <= Decimal(Int64.max) else { throw APIClientError.server("流量数值过大。") }
        return NSDecimalNumber(decimal: bytes).int64Value
    }

    func package() throws -> NodePackage {
        let quota = hasQuota ? try Self.bytes(quotaGB) : nil
        if let quota, quota <= 0 { throw APIClientError.server("套餐额度须大于零。") }
        let timezone = timezone.trimmingCharacters(in: .whitespacesAndNewlines)
        guard TimeZone(identifier: timezone) != nil else { throw APIClientError.server("请输入有效的 IANA 时区，例如 Asia/Shanghai。") }
        return NodePackage(
            expiresAt: hasExpiry ? ISO8601DateFormatter().string(from: expiry) : nil,
            quotaBytes: quota, cycle: cycle, resetDay: resetDay, timezone: timezone,
            interface: interface.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : interface.trimmingCharacters(in: .whitespacesAndNewlines),
            direction: direction, expiryWarningDays: warningDays, trafficWarningPercent: warningPercent
        )
    }
}

enum PackageDisplay {
    static func date(_ value: String?) -> Date? {
        guard let value else { return nil }
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return fractional.date(from: value) ?? ISO8601DateFormatter().date(from: value)
    }
    static func warning(_ kind: String) -> String {
        switch kind {
        case "expiring": "节点即将到期"
        case "expired": "节点已到期，代理已限制"
        case "traffic_warning": "节点流量接近额度"
        case "quota_exhausted": "节点流量已耗尽，代理已限制"
        default: kind
        }
    }
    static func gap(_ reason: String) -> String {
        switch reason {
        case "network_sample_failed": "网卡采集失败，请检查 SSH 和网卡设置"
        case "network_counter_reset_or_interface_changed": "服务器重启、网卡变化或计数重置，已重新建立基线"
        case "meter_configuration_changed": "计费设置已修改，等待建立采集基线"
        case "usage_corrected": "用量已校正，等待建立采集基线"
        default: "存在未能完整采集的流量"
        }
    }
    static func expiry(_ package: NodePackage?) -> String {
        guard let date = date(package?.expiresAt) else { return "不限时" }
        let days = Int(ceil(date.timeIntervalSinceNow / 86400))
        return days <= 0 ? "已到期" : "剩余 \(days) 天"
    }
    static func listUsage(_ package: NodePackage?, _ usage: NodePackageUsage?) -> String {
        guard let quota = package?.quotaBytes else { return "不限流量" }
        let used = usage.map { twoDecimalGB($0.usageBytes) } ?? "—"
        return "\(used) / \(twoDecimalGB(quota)) GB"
    }

    private static func twoDecimalGB(_ bytes: Int64) -> String {
        (Decimal(bytes) / 1_000_000_000).formatted(
            .number.precision(.fractionLength(2)).grouping(.never).locale(Locale(identifier: "en_US_POSIX"))
        )
    }

    static func usage(_ package: NodePackage?, _ usage: NodePackageUsage?) -> String {
        guard let quota = package?.quotaBytes else { return "不限流量" }
        let used = usage.map { NodePackageDraft.gbText($0.usageBytes) } ?? "—"
        return "\(used) / \(NodePackageDraft.gbText(quota)) GB"
    }
}
