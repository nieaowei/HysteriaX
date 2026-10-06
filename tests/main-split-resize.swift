import AppKit
import SwiftUI

@main struct MainSplitResizeChecks {
    @MainActor static func measure(_ name: String, root: AnyView) throws -> [String: Any] {
        let host = NSHostingView(rootView: root)
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1100, height: 850), styleMask: [.borderless], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = host
        defer { window.contentView = nil; window.close() }
        host.layoutSubtreeIfNeeded()
        RunLoop.current.run(until: Date().addingTimeInterval(0.3))
        func resize(_ width: CGFloat, _ height: CGFloat) {
            host.frame.size = NSSize(width: width, height: height)
            host.layoutSubtreeIfNeeded()
            RunLoop.current.run(until: Date().addingTimeInterval(0.001))
        }
        for index in 0..<8 { resize(CGFloat(760 + index * 48), CGFloat(740 + index * 20)) }
        print("BEGIN \(name)"); fflush(stdout)
        var layouts: [Double] = []
        var renders: [Double] = []
        for _ in 0..<2 {
            for index in 0..<24 {
                let step = index < 12 ? index : 23 - index
                let start = DispatchTime.now().uptimeNanoseconds
                resize(CGFloat(720 + step * 38), CGFloat(700 + step * 18))
                let laidOut = DispatchTime.now().uptimeNanoseconds
                if let bitmap = host.bitmapImageRepForCachingDisplay(in: host.bounds) {
                    host.cacheDisplay(in: host.bounds, to: bitmap)
                }
                let rendered = DispatchTime.now().uptimeNanoseconds
                layouts.append(Double(laidOut - start) / 1_000_000)
                renders.append(Double(rendered - start) / 1_000_000)
            }
        }
        let sorted = layouts.sorted()
        let result: [String: Any] = ["name": name, "frames": layouts.count,
            "mean_layout_ms": layouts.reduce(0, +) / Double(layouts.count),
            "p95_layout_ms": sorted[Int(Double(sorted.count - 1) * 0.95)],
            "mean_layout_render_ms": renders.reduce(0, +) / Double(renders.count)]
        print(String(data: try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys]), encoding: .utf8)!); fflush(stdout)
        return result
    }

    @MainActor static func main() throws {
        _ = NSApplication.shared
        let folder = CommandLine.arguments[1]
        let output = CommandLine.arguments[2]
        func decode<T: Decodable>(_ type: T.Type, _ name: String) throws -> T {
            try JSONDecoder().decode(type, from: Data(contentsOf: URL(fileURLWithPath: folder + "/" + name + ".json")))
        }
        let store = ManagementStore(restoreSnapshot: false)
        store.nodes = try decode([NodeSummary].self, "nodes")
        store.users = try decode([UserSummary].self, "users")
        store.jobs = try decode([JobSummary].self, "jobs")
        let record = DNSRecord(id: "resize-dns", zoneId: "resize-zone", providerRecordId: "remote-dns", name: "node.example.test", recordType: "AAAA", content: "2001:db8::1234", ttl: 300, proxied: false, origin: "hysteriax", revision: 1, remoteSnapshot: nil, desired: nil, state: "synced", resolutionStatus: "verified", resolutionDetail: nil, boundNodeId: nil, checkedAt: "2026-10-07T00:00:00Z", updatedAt: "2026-10-07T00:00:00Z")
        store.dnsRecords = [record]
        store.requestedDNSRecordID = record.id
        let roots: [(String, AnyView)] = [
            ("nodes_list", AnyView(NodesView(store: store))),
            ("nodes_detail", AnyView(NodesView(store: store, initialSelection: store.nodes.first?.id))),
            ("users_detail", AnyView(UsersView(store: store, initialSelection: store.users.first?.id))),
            ("jobs_detail", AnyView(JobsView(store: store, initialSelection: store.jobs.first?.id))),
            ("dns_detail", AnyView(DNSRecordsView(store: store)))
        ]
        var results: [[String: Any]] = []
        for (name, root) in roots { results.append(try measure(name, root: root)) }
        try JSONSerialization.data(withJSONObject: results, options: [.prettyPrinted, .sortedKeys])
            .write(to: URL(fileURLWithPath: output))
    }
}
