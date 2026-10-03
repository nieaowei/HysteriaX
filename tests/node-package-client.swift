import Foundation

@MainActor final class FakeNotifications: NodeNotificationTransport {
    var authorized = false
    var fail = false
    var sent: [String] = []
    func requestPermission() async throws -> Bool { authorized }
    func isAuthorized() async -> Bool { authorized }
    func send(id: String, title: String, body: String) async throws {
        if fail { throw APIClientError.invalidResponse }
        sent.append(id)
    }
}

@main struct NodePackageClientTests {
    @MainActor static func main() async throws {
        let decoder = JSONDecoder()
        let oldVersion = try decoder.decode(APIVersion.self, from: Data(#"{"api_version":"1.0.0","service_version":"0.1.0","hysteria_version":"app/v2.12.3","mihomo_version":"v1.19.31"}"#.utf8))
        precondition(oldVersion.features == nil)
        let legacy = try decoder.decode(NodeSummary.self, from: Data(#"{"id":"node","name":"Node","revision":1,"state":"deployed"}"#.utf8))
        precondition(legacy.package == nil && legacy.packageUsage == nil)
        let cached = try decoder.decode(NodeSummary.self, from: JSONEncoder().encode(legacy))
        precondition(cached.package == nil)
        var draft = NodePackageDraft()
        draft.hasQuota = true
        draft.quotaGB = "1.5"
        draft.hasExpiry = true
        draft.expiry = Date(timeIntervalSince1970: 1_800_000_000)
        let configured = try draft.package()
        precondition(configured.quotaBytes == 1_500_000_000 && configured.trafficWarningPercent == 80)
        let roundtrip = NodePackageDraft(configured)
        precondition(roundtrip.hasExpiry && roundtrip.quotaGB == "1.5")
        let oneByte = try NodePackageDraft.bytes("0.000000001")
        precondition(oneByte == 1)
        let maximum = try NodePackageDraft.bytes(NodePackageDraft.gbText(Int64.max))
        precondition(maximum == Int64.max)
        for invalid in ["-1", "invalid", "1junk", "1,5", "99999999999999999999"] {
            do { _ = try NodePackageDraft.bytes(invalid); preconditionFailure("invalid amount accepted") }
            catch { }
        }
        draft.hasExpiry = false
        draft.hasQuota = false
        let patch = NodePatchRequest(expectedRevision: 1, package: try draft.package())
        let payload = try JSONSerialization.jsonObject(with: JSONEncoder().encode(patch)) as! [String: Any]
        precondition(payload["config"] == nil && payload["package"] != nil)
        let cleared = payload["package"] as! [String: Any]
        precondition(cleared["expires_at"] == nil && cleared["quota_bytes"] == nil)
        var server = ServerConfigurationDraft()
        server.name = "Server"
        server.sshHost = "node.example.test"
        let unchanged = try server.request(revision: 7, originalAuthType: "password")
        let sshJSON = try JSONSerialization.jsonObject(with: JSONEncoder().encode(unchanged)) as! [String: Any]
        precondition(sshJSON["ssh_secret"] == nil && sshJSON["ssh_passphrase"] == nil)
        precondition(sshJSON["config"] == nil && sshJSON["package"] == nil)
        precondition(sshJSON["expected_revision"] as! Int == 7)
        server.sshAuthType = "private_key"
        do { _ = try server.request(revision: 7, originalAuthType: "password"); preconditionFailure("credential required when switching auth") } catch { }
        server.sshSecret = "new private key"
        server.clearPassphrase = true
        let replacement = try server.request(revision: 7, originalAuthType: "password")
        precondition(replacement.sshSecret == "new private key" && replacement.sshPassphrase == "")
        server.sshPort = "65536"
        do { _ = try server.request(revision: 7, originalAuthType: "password"); preconditionFailure("invalid port accepted") } catch { }
        let usageOperation = APIEndpoints.updateNodeUsage(id: "node")
        precondition(usageOperation.method == "PUT" && usageOperation.path == "api/v1/nodes/node/usage")
        let modern = try decoder.decode(NodeSummary.self, from: Data(#"{"id":"node","name":"Node","revision":1,"state":"deployed","package_usage":{"usage_bytes":800,"restricted":false,"reasons":[],"freshness":"fresh","alerts":[{"id":"event","kind":"traffic_warning","created_at":"2026-10-03T00:00:00Z"}]}}"#.utf8))
        let suite = "NodePackageTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let transport = FakeNotifications()
        let service = NodeAlertNotifications(transport: transport, defaults: defaults)
        await service.deliver(nodes: [modern], service: "server1")
        precondition(transport.sent.isEmpty)
        defaults.set(true, forKey: NodeAlertNotifications.preferenceKey)
        await service.deliver(nodes: [modern], service: "server1")
        precondition(transport.sent.isEmpty) // Denied permission keeps app alerts and permits future delivery.
        transport.authorized = true
        transport.fail = true
        await service.deliver(nodes: [modern], service: "server1")
        precondition(transport.sent.isEmpty)
        transport.fail = false
        await service.deliver(nodes: [modern], service: "server1")
        await service.deliver(nodes: [modern], service: "server1")
        let restarted = NodeAlertNotifications(transport: transport, defaults: defaults)
        await restarted.deliver(nodes: [modern], service: "server1")
        precondition(transport.sent.count == 1)
        await restarted.deliver(nodes: [modern], service: "server2")
        precondition(transport.sent.count == 2)
        print("Node package client tests passed: cache, drafts, API, denied permission, delivery retry and persistent deduplication.")
    }
}
