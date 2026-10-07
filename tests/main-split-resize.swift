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
            window.setContentSize(NSSize(width: width, height: height))
            host.layoutSubtreeIfNeeded()
        }
        for index in 0..<8 { resize(CGFloat(760 + index * 48), CGFloat(740 + index * 20)) }
        print("BEGIN \(name)"); fflush(stdout)
        let minimumWidth = CGFloat(Int(ProcessInfo.processInfo.environment["HYSTERIAX_RESIZE_MIN_WIDTH"] ?? "720") ?? 720)
        var layouts: [Double] = []
        var renders: [Double] = []
        for _ in 0..<2 {
            for index in 0..<24 {
                let step = index < 12 ? index : 23 - index
                let start = DispatchTime.now().uptimeNanoseconds
                resize(minimumWidth + CGFloat(step * 38), CGFloat(700 + step * 18))
                let laidOut = DispatchTime.now().uptimeNanoseconds
                if let bitmap = host.bitmapImageRepForCachingDisplay(in: host.bounds) {
                    host.cacheDisplay(in: host.bounds, to: bitmap)
                    if let directory = ProcessInfo.processInfo.environment["HYSTERIAX_RESIZE_SCREENSHOT_DIRECTORY"], index == 0 || index == 11 {
                        try bitmap.representation(using: .png, properties: [:])?.write(
                            to: URL(fileURLWithPath: directory + "/\(name)-\(Int(host.bounds.width)).png"))
                    }
                }
                let rendered = DispatchTime.now().uptimeNanoseconds
                layouts.append(Double(laidOut - start) / 1_000_000)
                renders.append(Double(rendered - start) / 1_000_000)
                // Drain display callbacks after timing; they can perform software drawing.
                RunLoop.current.run(until: Date().addingTimeInterval(0.001))
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
        let recordCount = max(1, Int(ProcessInfo.processInfo.environment["HYSTERIAX_DNS_RECORD_COUNT"] ?? "1") ?? 1)
        let prototype = try JSONSerialization.jsonObject(with: JSONEncoder().encode(record)) as! [String: Any]
        let records = (0..<recordCount).map { index in
            var value = prototype
            value["id"] = index == 0 ? record.id : "resize-dns-\(index)"
            value["name"] = index == 0 ? record.name : "node-\(index).example.test"
            return value
        }
        store.dnsRecords = try JSONDecoder().decode([DNSRecord].self, from: JSONSerialization.data(withJSONObject: records))
        store.requestedDNSRecordID = record.id
        let stamp = "2026-10-07T00:00:00Z"
        let credential = CredentialDetail(referenceCount: 8, id: "resize-credential", name: "缩放测试凭据", kind: "dns", ownerUserId: nil,
            revision: 2, latestVersion: 2, archived: false, reminderAt: nil, expiresAt: nil, daysRemaining: nil,
            status: "active", metadata: ["fingerprint": .string("fixture-fingerprint")], createdAt: stamp, updatedAt: stamp,
            versions: [CredentialVersion(version: 2, metadata: [:], createdAt: stamp)],
            references: (0..<8).map { CredentialReference(entityType: "node", entityID: "node-\($0)", source: "node_config", name: "测试节点 \($0)", version: 2, field: nil, nodeID: "node-\($0)", configRevision: 2) },
            batches: (0..<2).map { batch in CredentialBatch(dnsItems: nil, id: "batch-\(batch)", credentialId: "resize-credential", version: 2, createdAt: stamp,
                items: (0..<4).map { index in CredentialBatchItem(nodeID: "node-\(index)", userId: "user-\(index)", jobId: "job-\(index)", status: index == 0 ? "failed" : "succeeded", stage: "finished", errorMessage: index == 0 ? "测试错误：连接超时" : nil, name: "测试节点 \(index)") }) })
        let emptyCredential = CredentialDetail(referenceCount: 0, id: "empty-credential", name: "空凭据", kind: "dns", ownerUserId: nil,
            revision: 1, latestVersion: 1, archived: false, reminderAt: nil, expiresAt: nil, daysRemaining: nil,
            status: "active", metadata: [:], createdAt: stamp, updatedAt: stamp, versions: [], references: [], batches: [])
        func credentialRoot(_ detail: CredentialDetail) -> AnyView {
            AnyView(MainVerticalSplitView(hasDetail: true) {
                Text("凭据列表").frame(maxWidth: .infinity, maxHeight: .infinity)
            } detail: {
                ScrollView {
                    CredentialManagedDetailView(detail: detail, isConnected: true, onJump: { _, _ in }, onRetry: { _ in })
                        .frame(maxWidth: .infinity, alignment: .leading).padding(16)
                }
            })
        }
        func groupRoot(count: Int) throws -> AnyView {
            let groupStore = ManagementStore(restoreSnapshot: false)
            let users = (0..<count).map { index -> [String: Any] in
                ["id": String(format: "%08x-1234-5678-9abc-000000000000", index),
                 "name": "Member \(index)", "enabled": true, "usage_bytes": 0, "revision": 1,
                 "assignments": [], "created_at": stamp, "updated_at": stamp]
            }
            groupStore.users = try JSONDecoder().decode([UserSummary].self, from: JSONSerialization.data(withJSONObject: users))
            groupStore.authorizationGroups = [AuthorizationGroupSummary(id: "resize-group", name: "Resize group", revision: 1,
                userIds: groupStore.users.map(\.id), nodeIds: [], userCount: count, nodeCount: 0, createdAt: stamp, updatedAt: stamp)]
            let page = AuthorizationPageState()
            page.selectedGroupID = "resize-group"
            return AnyView(AuthorizationGroupsView(store: groupStore, pageState: page, onOpenUser: { _ in }))
        }
        let roots: [(String, AnyView)] = [
            ("groups_6", try groupRoot(count: 6)),
            ("groups_5000", try groupRoot(count: 5_000)),
            ("nodes_list", AnyView(NodesView(store: store))),
            ("nodes_detail", AnyView(NodesView(store: store, initialSelection: store.nodes.first?.id))),
            ("users_detail", AnyView(UsersView(store: store, initialSelection: store.users.first?.id))),
            ("jobs_detail", AnyView(JobsView(store: store, initialSelection: store.jobs.first?.id))),
            ("dns_detail", AnyView(DNSRecordsView(store: store))),
            ("credential_managed_detail", credentialRoot(credential)),
            ("credential_empty_detail", credentialRoot(emptyCredential))
        ]
        var results: [[String: Any]] = []
        let scenarios = Set(CommandLine.arguments.dropFirst(3))
        for (name, root) in roots where scenarios.isEmpty || scenarios.contains(name) {
            results.append(try measure(name, root: root))
        }
        try JSONSerialization.data(withJSONObject: results, options: [.prettyPrinted, .sortedKeys])
            .write(to: URL(fileURLWithPath: output))
    }
}
