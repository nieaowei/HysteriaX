import Foundation

enum TrafficUnits {
    static let bytesPerGiB: Int64 = 1_073_741_824

    // Split the integer and fraction so even Int64.max round-trips exactly.
    static func gibText(_ bytes: Int64) -> String {
        let whole = bytes / bytesPerGiB
        let remainder = bytes % bytesPerGiB
        guard remainder != 0 else { return String(whole) }
        let fraction = NSDecimalNumber(decimal: Decimal(remainder) / Decimal(bytesPerGiB)).stringValue
        return "\(whole)\(fraction.dropFirst())"
    }

    static func bytes(_ text: String) throws -> Int64 {
        let normalized = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard normalized.count <= 64,
              normalized.range(of: #"^[0-9]+(?:\.[0-9]{1,30})?$"#, options: .regularExpression) != nil else {
            throw APIClientError.server(L10n.text("流量须为非负 GiB 数值，使用小数点。"))
        }
        let parts = normalized.split(separator: ".")
        guard let whole = Int64(parts[0]), whole <= Int64.max / bytesPerGiB else {
            throw APIClientError.server(L10n.text("流量数值过大。"))
        }
        // Decimal long multiplication gives floor(fraction * 2^30) without
        // floating-point rounding, including a one-byte fractional GiB.
        var fractionBytes: Int64 = 0
        if parts.count == 2 {
            for digit in parts[1].utf8.reversed() {
                fractionBytes = (Int64(digit - 48) * bytesPerGiB + fractionBytes) / 10
            }
        }
        let result = (whole * bytesPerGiB).addingReportingOverflow(fractionBytes)
        guard !result.overflow else { throw APIClientError.server(L10n.text("流量数值过大。")) }
        return result.partialValue
    }

    static func display(_ bytes: Int64) -> String {
        let units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"]
        var value = Decimal(bytes)
        var unit = 0
        while value >= 1024 && unit < units.count - 1 {
            value /= 1024
            unit += 1
        }
        return "\(value.formatted(.number.precision(.fractionLength(0...2)))) \(units[unit])"
    }

    static func fixedGiB(_ bytes: Int64) -> String {
        (Decimal(bytes) / Decimal(bytesPerGiB)).formatted(
            .number.precision(.fractionLength(2)).grouping(.never).locale(Locale(identifier: "en_US_POSIX"))
        )
    }
}
