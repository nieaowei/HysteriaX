import Foundation

@dynamicMemberLookup
struct OverviewHistoryDisplay {
    let history: OverviewHistory
    let buckets: [OverviewChartBucket]
    let domain: ClosedRange<Date>
    let incompleteCount: Int
    let onlineIncompleteCount: Int
    let minTrafficCovered: Int64
    let maxTrafficCovered: Int64
    let maxTrafficExpected: Int64
    let minOnlineCovered: Int64
    var updatedAtText: String { DateDisplayText.local(history.generatedAt) }

    init(_ history: OverviewHistory) {
        self.history = history
        buckets = history.buckets.map(OverviewChartBucket.init)
        let start = buckets.first?.date ?? DateDisplayText.parse(history.generatedAt) ?? .distantPast
        let end = buckets.last?.endDate ?? start.addingTimeInterval(1)
        domain = start...max(start.addingTimeInterval(1), end)
        incompleteCount = history.buckets.filter(\.incomplete).count
        onlineIncompleteCount = history.buckets.filter(\.onlineIncomplete).count
        minTrafficCovered = history.buckets.map(\.trafficCoveredNodes).min() ?? 0
        maxTrafficCovered = history.buckets.map(\.trafficCoveredNodes).max() ?? 0
        maxTrafficExpected = history.buckets.map(\.trafficExpectedNodes).max() ?? 0
        minOnlineCovered = history.buckets.map(\.coveredNodes).min() ?? 0
    }

    enum BucketMatchMode { case interval, nearestStart }

    func bucket(at targetDate: Date, mode: BucketMatchMode = .interval) -> OverviewChartBucket? {
        guard !buckets.isEmpty else { return nil }
        switch mode {
        case .interval:
            if let exact = buckets.first(where: { targetDate >= $0.date && targetDate < $0.endDate }) {
                return exact
            }
            if let last = buckets.last, targetDate == last.endDate {
                return last
            }
            return buckets.min {
                abs($0.midpoint.timeIntervalSince(targetDate)) < abs($1.midpoint.timeIntervalSince(targetDate))
            }
        case .nearestStart:
            return buckets.min {
                abs($0.date.timeIntervalSince(targetDate)) < abs($1.date.timeIntervalSince(targetDate))
            }
        }
    }

    subscript<T>(dynamicMember key: KeyPath<OverviewHistory, T>) -> T { history[keyPath: key] }
}

@dynamicMemberLookup
struct OverviewChartBucket: Identifiable {
    let sample: OverviewBucket
    let date: Date
    let endDate: Date
    var id: String { sample.start }
    var midpoint: Date { date.addingTimeInterval(max(0, endDate.timeIntervalSince(date)) / 2) }
    var probeSuccessRate: Double? {
        sample.probeAttempts > 0 ? Double(sample.probeSuccesses) / Double(sample.probeAttempts) * 100 : nil
    }

    init(_ sample: OverviewBucket) {
        self.sample = sample
        date = DateDisplayText.parse(sample.start) ?? .distantPast
        endDate = DateDisplayText.parse(sample.end) ?? date
    }

    func timeRangeText(range: String, timeZoneIdentifier: String) -> String {
        let timeZone = TimeZone(identifier: timeZoneIdentifier) ?? .autoupdatingCurrent
        let locale = Locale(identifier: L10n.language)
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = timeZone

        var dayStyle = Date.FormatStyle(locale: locale).month(.twoDigits).day(.twoDigits)
        dayStyle.timeZone = timeZone

        var timeStyle = Date.FormatStyle(locale: locale).hour(.twoDigits(amPM: .omitted)).minute(.twoDigits)
        timeStyle.timeZone = timeZone

        var dateTimeStyle = Date.FormatStyle(locale: locale).month(.twoDigits).day(.twoDigits)
            .hour(.twoDigits(amPM: .omitted)).minute(.twoDigits)
        dateTimeStyle.timeZone = timeZone

        let duration = endDate.timeIntervalSince(date)
        if range == "24h" || duration < 86400 {
            if calendar.isDate(date, inSameDayAs: endDate) {
                return "\(date.formatted(dateTimeStyle))–\(endDate.formatted(timeStyle))"
            }
            return "\(date.formatted(dateTimeStyle)) – \(endDate.formatted(dateTimeStyle))"
        }
        if duration > 86400 * 1.1 {
            return "\(date.formatted(dayStyle))–\(endDate.formatted(dayStyle))"
        }
        return date.formatted(dayStyle)
    }

    subscript<T>(dynamicMember key: KeyPath<OverviewBucket, T>) -> T { sample[keyPath: key] }
}
