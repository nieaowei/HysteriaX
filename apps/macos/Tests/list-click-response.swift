import AppKit
import SwiftUI

@Observable
@MainActor
final class ClickSelection {
    var selectedID: String?
}

@main
struct ListClickResponseChecks {
    @MainActor
    static func tables(in view: NSView) -> [NSTableView] {
        (view as? NSTableView).map { [$0] } ?? view.subviews.flatMap { tables(in: $0) }
    }

    @MainActor
    static func measureClickScenario<Content: View>(
        _ name: String,
        ids: [String],
        @ViewBuilder content: @escaping (ClickSelection) -> Content
    ) -> [String: Any] {
        let selection = ClickSelection()
        let host = NSHostingView(rootView: content(selection))
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1100, height: 850),
            styleMask: [.borderless],
            backing: .buffered,
            defer: false
        )
        window.isReleasedWhenClosed = false
        window.contentView = host
        defer { window.contentView = nil; window.close() }
        host.layoutSubtreeIfNeeded()
        RunLoop.current.run(until: Date().addingTimeInterval(0.15))

        let initialTableID = tables(in: host).first.map(ObjectIdentifier.init)
        precondition(initialTableID != nil, "\(name): expected initial NSTableView")

        func step(_ targetID: String?) -> Double {
            let start = DispatchTime.now().uptimeNanoseconds
            selection.selectedID = targetID
            host.layoutSubtreeIfNeeded()
            if let bitmap = host.bitmapImageRepForCachingDisplay(in: host.bounds) {
                host.cacheDisplay(in: host.bounds, to: bitmap)
            }
            let elapsed = Double(DispatchTime.now().uptimeNanoseconds - start) / 1_000_000
            RunLoop.current.run(until: Date().addingTimeInterval(0.01))
            return elapsed
        }

        let firstClickMs = step(ids[0])
        precondition(tables(in: host).first.map(ObjectIdentifier.init) == initialTableID, "\(name): first click recreated NSTableView")

        var switchLatencies: [Double] = []
        for id in ids.dropFirst() {
            switchLatencies.append(step(id))
            precondition(tables(in: host).first.map(ObjectIdentifier.init) == initialTableID, "\(name): row switch recreated NSTableView")
        }

        let closeClickMs = step(nil)
        precondition(tables(in: host).first.map(ObjectIdentifier.init) == initialTableID, "\(name): close recreated NSTableView")

        let meanSwitchMs = switchLatencies.reduce(0, +) / Double(max(1, switchLatencies.count))
        let maxSwitchMs = switchLatencies.max() ?? 0
        return [
            "name": name,
            "first_click_ms": firstClickMs,
            "mean_switch_ms": meanSwitchMs,
            "max_switch_ms": maxSwitchMs,
            "close_ms": closeClickMs
        ]
    }

    @MainActor
    static func main() async throws {
        _ = NSApplication.shared
        let folder = CommandLine.arguments[1]
        let output = CommandLine.arguments[2]
        func decode<T: Decodable>(_ type: T.Type, _ name: String) throws -> T {
            try JSONDecoder().decode(type, from: Data(contentsOf: URL(fileURLWithPath: folder + "/" + name + ".json")))
        }

        let store = ManagementStore(restoreSnapshot: false)
        let baseNodes = try decode([NodeSummary].self, "nodes")
        let baseUsers = try decode([UserSummary].self, "users")
        let baseJobs = try decode([JobSummary].self, "jobs")

        // Scale to 1,000 nodes & users to exercise O(1) lookup paths.
        let encoder = JSONEncoder()
        let decoder = JSONDecoder()
        let nodeProto = try JSONSerialization.jsonObject(with: encoder.encode(baseNodes[0])) as! [String: Any]
        let scaledNodesData = try JSONSerialization.data(withJSONObject: (0..<1000).map { i -> [String: Any] in
            var copy = nodeProto
            copy["id"] = i < baseNodes.count ? baseNodes[i].id : "scale-node-\(i)"
            copy["name"] = i < baseNodes.count ? baseNodes[i].name : "Scale Node \(i)"
            return copy
        })
        store.nodes = try decoder.decode([NodeSummary].self, from: scaledNodesData)
        store.users = baseUsers
        store.jobs = baseJobs

        let stamp = "2026-10-07T00:00:00Z"
        let record = DNSRecord(
            id: "click-dns-0", zoneId: "zone-0", providerRecordId: "remote-0",
            name: "node-0.example.test", recordType: "AAAA", content: "2001:db8::1",
            ttl: 300, proxied: false, origin: "hysteriax", revision: 1,
            remoteSnapshot: nil, desired: nil, state: "synced",
            resolutionStatus: "verified", resolutionDetail: nil,
            boundNodeId: store.nodes.last?.id, checkedAt: stamp, updatedAt: stamp
        )
        let dnsProto = try JSONSerialization.jsonObject(with: encoder.encode(record)) as! [String: Any]
        store.dnsRecords = try decoder.decode([DNSRecord].self, from: JSONSerialization.data(withJSONObject: (0..<50).map { i -> [String: Any] in
            var copy = dnsProto
            copy["id"] = "click-dns-\(i)"
            copy["name"] = "node-\(i).example.test"
            return copy
        }))

        // Verify ListPageCache.peek() zero-decode speed on 50-item NodesPage.
        let cachedPage = NodesPage(items: Array(store.nodes.prefix(50)), total: 1000, page: 1, pageSize: 50)
        _ = try await store.listPageCache.load("bench:nodes:p1", ttl: 60) { _ in cachedPage }
        let peekStart = DispatchTime.now().uptimeNanoseconds
        for _ in 0..<1000 {
            let page = store.listPageCache.peek("bench:nodes:p1", as: NodesPage.self)
            precondition(page?.items.count == 50)
        }
        let peek1000Ms = Double(DispatchTime.now().uptimeNanoseconds - peekStart) / 1_000_000
        precondition(peek1000Ms < 50, "Expected 1000 ListPageCache.peek() calls under 50ms, got \(peek1000Ms)ms")

        // Verify O(1) lookup helpers.
        let lastNodeID = store.nodes.last!.id
        precondition(store.node(for: lastNodeID)?.id == lastNodeID)

        // Verify DateDisplayText caching.
        let dateStart = DispatchTime.now().uptimeNanoseconds
        for _ in 0..<1000 {
            let text = DateDisplayText.local(stamp)
            precondition(!text.isEmpty && text != "—")
        }
        let date1000Ms = Double(DispatchTime.now().uptimeNanoseconds - dateStart) / 1_000_000

        let nodeIDs = Array(store.nodes.prefix(5).map(\.id))
        let userIDs = Array(store.users.prefix(5).map(\.id))
        let jobIDs = Array(store.jobs.prefix(5).map(\.id))
        let dnsIDs = Array(store.dnsRecords.prefix(5).map(\.id))

        let results: [[String: Any]] = [
            measureClickScenario("nodes_view", ids: nodeIDs) { sel in
                NodesView(store: store, initialSelection: sel.selectedID)
            },
            measureClickScenario("users_view", ids: userIDs) { sel in
                UsersView(store: store, initialSelection: sel.selectedID)
            },
            measureClickScenario("jobs_view", ids: jobIDs) { sel in
                JobsView(store: store, initialSelection: sel.selectedID)
            },
            measureClickScenario("dns_view", ids: dnsIDs) { sel in
                let _ = sel.selectedID
                store.requestedDNSRecordID = sel.selectedID
                return DNSRecordsView(store: store)
            }
        ]

        let summary: [String: Any] = [
            "peek_1000_ms": peek1000Ms,
            "date_format_1000_ms": date1000Ms,
            "views": results
        ]
        let json = try JSONSerialization.data(withJSONObject: summary, options: [.prettyPrinted, .sortedKeys])
        try json.write(to: URL(fileURLWithPath: output))
        print(String(data: json, encoding: .utf8)!)
    }
}
