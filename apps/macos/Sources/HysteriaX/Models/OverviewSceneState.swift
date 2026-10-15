import SwiftUI
import Observation

@MainActor @Observable
final class OverviewSceneState {
    let summary = OverviewTabState()
    let traffic = OverviewTabState()
    let quality = OverviewTabState()

    func tab(_ id: String) -> OverviewTabState {
        switch id { case "traffic": traffic; case "quality": quality; default: summary }
    }

    func changedService() {
        for tab in [summary, traffic, quality] {
            tab.scrollOffset = .zero
            tab.inspectedNode = nil
            tab.nodeQuery = ""
            tab.nodePage = 1
            tab.nodePageResponse = nil
            tab.nodeLoadedKey = nil
            tab.nodePageError = nil
            tab.history.nodeID = ""
            tab.history.replaceHistory(nil)
            tab.history.loadedQuery = ""
            tab.history.error = nil
        }
    }
}

@MainActor @Observable
final class OverviewTabState {
    let history = OverviewHistoryState()
    var showAllIssues = false
    var inspectedNode: OverviewNode?
    var nodeQuery = ""
    var nodePage = 1
    var nodePageSize = 20
    var nodePageResponse: OverviewNodesPage?
    var nodeLoadedKey: String?
    var nodePageLoading = false
    var nodePageError: String?
    var nodeRetryToken = UUID()
    // Scrolling must not invalidate the entire page on every pixel.
    @ObservationIgnored var scrollOffset: CGPoint = .zero
    @ObservationIgnored var handledQuotaRequest: UUID?
}

@MainActor @Observable
final class OverviewHistoryState {
    var range: String
    var source = "users"
    var nodeID = ""
    var display: OverviewHistoryDisplay?
    var error: String?
    var loading = false
    var loadedQuery = ""

    init(range: String = "24h", history: OverviewHistory? = nil) {
        self.range = range
        display = history.map(OverviewHistoryDisplay.init)
    }
    func replaceHistory(_ history: OverviewHistory?) { display = history.map(OverviewHistoryDisplay.init) }
}
