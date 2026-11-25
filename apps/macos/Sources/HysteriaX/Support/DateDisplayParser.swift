import Foundation

// ISO8601DateFormatter is mutable; synchronize access to the shared instances.
final class DateDisplayParser: @unchecked Sendable {
    static let shared = DateDisplayParser()
    private let lock = NSLock()
    private let fractional = ISO8601DateFormatter()
    private let standard = ISO8601DateFormatter()
    private let cache = NSCache<NSString, NSDate>()
    private let formattedCache = NSCache<NSString, NSString>()

    private init() {
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        standard.formatOptions = [.withInternetDateTime]
        cache.countLimit = 2048
        formattedCache.countLimit = 4096
    }

    func parse(_ value: String?) -> Date? {
        guard let value else { return nil }
        lock.lock()
        defer { lock.unlock() }
        if let cached = cache.object(forKey: value as NSString) { return cached as Date }
        guard let date = fractional.date(from: value) ?? standard.date(from: value) else { return nil }
        cache.setObject(date as NSDate, forKey: value as NSString)
        return date
    }

    func local(_ value: String?, language: String, timeZoneIdentifier: String) -> String {
        guard let value else { return "—" }
        let cacheKey = "\(language)|\(timeZoneIdentifier)|\(value)" as NSString
        lock.lock()
        if let cached = formattedCache.object(forKey: cacheKey) {
            lock.unlock()
            return cached as String
        }
        let parsedDate: Date?
        if let cachedDate = cache.object(forKey: value as NSString) {
            parsedDate = cachedDate as Date
        } else if let date = fractional.date(from: value) ?? standard.date(from: value) {
            cache.setObject(date as NSDate, forKey: value as NSString)
            parsedDate = date
        } else {
            parsedDate = nil
        }
        lock.unlock()

        guard let date = parsedDate else { return value }
        let formatted = date.formatted(Date.FormatStyle(date: .abbreviated, time: .shortened).locale(Locale(identifier: language)))
        lock.lock()
        formattedCache.setObject(formatted as NSString, forKey: cacheKey)
        lock.unlock()
        return formatted
    }
}

extension NodeACMEStatus {
    var isStale: Bool {
        guard let collected = DateDisplayParser.shared.parse(collectedAt) else { return true }
        return Date().timeIntervalSince(collected) > 120
    }
    var daysRemaining: Int? {
        DateDisplayParser.shared.parse(notAfter).map { Int(floor($0.timeIntervalSinceNow / 86400)) }
    }
    var displayState: String {
        if isStale { return L10n.text("数据陈旧") }
        if let days = daysRemaining, days < 0 { return L10n.text("已到期") }
        if serviceState == "inactive" || serviceState == "failed" { return L10n.text("证书组件已停止") }
        switch state {
        case "issuing": return L10n.text("正在签发")
        case "renewing": return L10n.text("正在续期")
        case "failed": return L10n.text("证书维护失败")
        case "expired": return L10n.text("已到期")
        case "ready": return L10n.text("就绪")
        default: return L10n.text("等待签发")
        }
    }
}
