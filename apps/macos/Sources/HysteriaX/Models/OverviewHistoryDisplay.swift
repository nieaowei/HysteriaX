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

    subscript<T>(dynamicMember key: KeyPath<OverviewHistory, T>) -> T { history[keyPath: key] }
}

@dynamicMemberLookup
struct OverviewChartBucket: Identifiable {
    let sample: OverviewBucket
    let date: Date
    let endDate: Date
    var id: String { sample.start }

    init(_ sample: OverviewBucket) {
        self.sample = sample
        date = DateDisplayText.parse(sample.start) ?? .distantPast
        endDate = DateDisplayText.parse(sample.end) ?? date
    }

    subscript<T>(dynamicMember key: KeyPath<OverviewBucket, T>) -> T { sample[keyPath: key] }
}
