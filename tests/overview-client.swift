import AppKit
import SwiftUI

@main
struct OverviewClientChecks {
    @MainActor static func main() throws {
        _ = NSApplication.shared
        let fixture = CommandLine.arguments[1]
        let output = CommandLine.arguments[2]
        let store = ManagementStore(restoreSnapshot: false)
        store.serviceAddress = "https://overview-fixture.invalid"
        defer { UserDefaults.standard.removeObject(forKey: "overviewHistory.\(store.serviceAddress)") }
        store.nodes = try JSONDecoder().decode([NodeSummary].self, from: Data(contentsOf: URL(fileURLWithPath: fixture + "/nodes.json")))
        store.serverMonitoring = try JSONDecoder().decode(ServerMonitoring.self, from: Data(contentsOf: URL(fileURLWithPath: fixture + "/server-monitoring.json")))
        store.jobs = try JSONDecoder().decode([JobSummary].self, from: Data(contentsOf: URL(fileURLWithPath: fixture + "/jobs.json")))
        store.overview = try JSONDecoder().decode(OverviewResponse.self, from: Data(contentsOf: URL(fileURLWithPath: fixture + "/overview.json")))
        let history = try JSONDecoder().decode(OverviewHistory.self, from: Data(contentsOf: URL(fileURLWithPath: fixture + "/history.json")))
        store.cacheOverviewHistory(history, nodeID: nil)
        let recent = try JSONDecoder().decode(OverviewHistory.self, from: Data(contentsOf: URL(fileURLWithPath: fixture + "/history-24h.json")))
        store.cacheOverviewHistory(recent, nodeID: nil)
        let month = try JSONDecoder().decode(OverviewHistory.self, from: Data(contentsOf: URL(fileURLWithPath: fixture + "/history-30d.json")))
        store.cacheOverviewHistory(month, nodeID: nil)
        store.supportsOverviewMonitoring = true
        store.lastUpdated = Date()
        precondition(store.cachedOverviewHistory(range: "7d", nodeID: nil, source: "users")?.buckets.count == 7)
        precondition(store.cachedOverviewHistory(range: "7d", nodeID: "other-node", source: "users") == nil)
        precondition(store.overview?.nodes[0].inletStatus == "ok")
        precondition(history.buckets[0].txBytes == nil && history.buckets[0].incomplete)
        let prepared = OverviewHistoryDisplay(recent)
        precondition(prepared.buckets.count == 24 && prepared.incompleteCount == 1)
        precondition(prepared.domain.lowerBound == DateDisplayText.parse(recent.buckets.first!.start))
        precondition(prepared.domain.upperBound == DateDisplayText.parse(recent.buckets.last!.end))
        precondition(prepared.buckets.allSatisfy { $0.date < $0.endDate })
        precondition(DateDisplayText.parse("invalid") == nil)
        precondition(DateDisplayText.parse("2026-10-04T01:02:03Z") == DateDisplayText.parse("2026-10-04T01:02:03.000000Z"))
        let scene = OverviewSceneState()
        scene.traffic.history.range = "7d"
        scene.traffic.history.source = "network"
        scene.traffic.history.nodeID = "old-node"
        scene.traffic.history.replaceHistory(history)
        scene.traffic.scrollOffset = CGPoint(x: 0, y: 200)
        scene.summary.showAllIssues = true
        precondition(scene.tab("traffic") === scene.traffic && scene.quality.history.range == "24h")
        scene.changedService()
        precondition(scene.traffic.history.range == "7d" && scene.traffic.history.source == "network")
        precondition(scene.traffic.history.nodeID.isEmpty && scene.traffic.history.display == nil && scene.traffic.scrollOffset == .zero)
        let layouts: [(String, CGFloat, ColorScheme, String)] = [
            ("wide-light", 1100, .light, "24h"), ("narrow-dark", 700, .dark, "24h"),
            ("compact-dark", 550, .dark, "24h"),
            ("traffic-wide-light", 1100, .light, "24h"), ("traffic-narrow-dark", 700, .dark, "24h"),
            ("traffic-7d-light", 1100, .light, "7d"), ("traffic-7d-dark", 700, .dark, "7d"),
            ("traffic-30d-light", 1100, .light, "30d"), ("traffic-30d-dark", 700, .dark, "30d"),
            ("quality-wide-light", 1100, .light, "24h"), ("quality-narrow-dark", 700, .dark, "24h")
        ]
        for (name, width, scheme, range) in layouts {
            let page: AnyView
            if name.hasPrefix("traffic-") { page = AnyView(OverviewHistoryView(store: store, isActive: false, range: range)) }
            else if name.hasPrefix("quality-") { page = AnyView(OverviewHistoryView(store: store, category: .quality, isActive: false, range: range)) }
            else { page = AnyView(OverviewView(store: store)) }
            let content = page.frame(width: width, height: 1600).environment(\.colorScheme, scheme).background(scheme == .dark ? Color(red: 0.12, green: 0.12, blue: 0.13) : Color.white)
            let hosting = NSHostingView(rootView: content)
            let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: width, height: 1600), styleMask: [.borderless], backing: .buffered, defer: false)
            window.appearance = NSAppearance(named: scheme == .dark ? .darkAqua : .aqua)
            hosting.appearance = window.appearance
            window.contentView = hosting
            hosting.frame = NSRect(x: 0, y: 0, width: width, height: 1600)
            hosting.layoutSubtreeIfNeeded()
            RunLoop.current.run(until: Date().addingTimeInterval(0.2))
            guard let bitmap = hosting.bitmapImageRepForCachingDisplay(in: hosting.bounds) else { fatalError("Overview bitmap allocation failed") }
            hosting.cacheDisplay(in: hosting.bounds, to: bitmap)
            if name.hasPrefix("traffic-") || name.hasPrefix("quality-") {
                // Floating interval strokes pass DTO/UI checks; inspect the rendered
                // blue series to require a substantial vertical column above baseline.
                var longestColumn = 0
                for x in stride(from: 0, to: bitmap.pixelsWide, by: 16) {
                    var run = 0
                    for y in 0..<bitmap.pixelsHigh {
                        let color = bitmap.colorAt(x: x, y: y)?.usingColorSpace(.deviceRGB)
                        if let color, color.blueComponent > color.redComponent + 0.3, color.blueComponent > color.greenComponent + 0.1 {
                            run += 1
                            longestColumn = max(longestColumn, run)
                        } else { run = 0 }
                    }
                }
                precondition(longestColumn > 30, "\(name): expected columns extending upward from baseline, longest blue column was \(longestColumn) pixels")
            }
            guard let png = bitmap.representation(using: .png, properties: [:]) else { fatalError("Overview rendering failed") }
            try png.write(to: URL(fileURLWithPath: output + "/" + name + ".png"))
        }
        print("Overview DTOs, cache isolation and light/dark layout rendering passed")
    }
}
