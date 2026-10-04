import Foundation

// ISO8601DateFormatter is mutable; synchronize access to the shared instances.
final class DateDisplayParser: @unchecked Sendable {
    static let shared = DateDisplayParser()
    private let lock = NSLock()
    private let fractional = ISO8601DateFormatter()
    private let standard = ISO8601DateFormatter()
    private let cache = NSCache<NSString, NSDate>()

    private init() {
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        standard.formatOptions = [.withInternetDateTime]
        cache.countLimit = 2048
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
}
