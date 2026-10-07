import Foundation

private final class FixtureState: @unchecked Sendable {
    let lock = NSLock()
    var requests: [(String, String, Data)] = []
    var failGroups = false
    func record(_ request: URLRequest) -> Data {
        var body = request.httpBody ?? Data()
        if let stream = request.httpBodyStream {
            stream.open(); defer { stream.close() }
            var buffer = [UInt8](repeating: 0, count: 4096)
            while stream.hasBytesAvailable {
                let count = stream.read(&buffer, maxLength: buffer.count)
                if count <= 0 { break }
                body.append(contentsOf: buffer.prefix(count))
            }
        }
        lock.lock(); defer { lock.unlock() }
        requests.append((request.httpMethod ?? "GET", request.url!.path, body))
        return body
    }
}

private final class GroupFixtureProtocol: URLProtocol, @unchecked Sendable {
    static let state = FixtureState()
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        _ = Self.state.record(request)
        let path = request.url!.path
        let method = request.httpMethod ?? "GET"
        let receipt = #"{"group":null,"additions_count":0,"removals_count":1,"created_credentials":[],"revocation_job_ids":["kick-1"]}"#
        let preview = #"{"action":"create","preview_token":"guard-1","additions_count":1,"removals_count":0,"additions":[{"user_id":"user","node_id":"node"}],"removals":[],"missing_mtls":[{"user_id":"user","node_id":"node"}]}"#
        var status = 200
        let data: Data
        if path.hasSuffix("/preview") { data = Data(preview.utf8) }
        else if method == "DELETE" { data = Data(receipt.utf8) }
        else if path == "/api/v1/version" { data = Data(#"{"api_version":"1.0.0","features":["authorization_groups"],"service_version":"fixture","hysteria_version":"fixture","mihomo_version":"fixture"}"#.utf8) }
        else if path == "/api/v1/authorization-groups" && Self.state.failGroups { status = 503; data = Data(#"{"error":{"code":"temporary","message":"groups unavailable"}}"#.utf8) }
        else { data = Data("[]".utf8) }
        client?.urlProtocol(self, didReceive: HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: "HTTP/1.1", headerFields: ["Content-Type":"application/json"])!, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: data)
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}

@MainActor
private struct SilentNotifications: NodeNotificationTransport {
    func requestPermission() async throws -> Bool { false }
    func isAuthorized() async -> Bool { false }
    func send(id: String, title: String, body: String) async throws {}
}

@main
struct AuthorizationGroupClientChecks {
    @MainActor static func main() async throws {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [GroupFixtureProtocol.self]
        let client = APIClient(baseURL: URL(string:"https://authorization-fixture.invalid")!, token:"fixture", session: URLSession(configuration:config))
        let defaults = UserDefaults(suiteName: "authorization-tests-" + UUID().uuidString)!
        let store = ManagementStore(client:client, restoreSnapshot:false, nodeNotifications: NodeAlertNotifications(transport:SilentNotifications(), defaults:defaults))
        store.isConnected = true
        store.serviceAddress = "https://authorization-fixture.invalid"
        let oldUser = try JSONDecoder().decode(UserSummary.self, from: Data(#"{"id":"user","name":"Alice","enabled":true,"usage_bytes":0,"revision":1,"assignments":[],"created_at":"2026-10-07T00:00:00Z","updated_at":"2026-10-07T00:00:00Z"}"#.utf8))
        precondition(oldUser.authorizationGroups == nil)
        store.users = [oldUser]
        await store.refreshAuthorizationGroups()
        precondition(GroupFixtureProtocol.state.requests.isEmpty, "old server must not receive group endpoints")
        store.supportsAuthorizationGroups = true
        let binding = AuthorizationMTLSBinding(userId:"user",nodeID:"node",credentialId:"cert",credentialVersion:2)
        let preview = try await store.previewAuthorizationGroup(group:nil,name:"Test",userIDs:["user"],nodeIDs:["node"],mtlsBindings:[binding])
        precondition(preview.missingMtls.count == 1 && preview.previewToken == "guard-1")
        let sent = try JSONSerialization.jsonObject(with: GroupFixtureProtocol.state.requests.last!.2) as! [String:Any]
        let bindings = sent["mtls_bindings"] as! [[String:Any]]
        precondition(bindings[0]["credential_version"] as? Int == 2 && bindings[0]["user_id"] as? String == "user")
        let source = try JSONDecoder().decode(AssignmentInfo.self, from: Data(#"{"node_id":"node","created_at":"2026-10-07T00:00:00Z","source_groups":[{"id":"migration-abc","name":"Migrated"},{"id":"group-b","name":"Second"}]}"#.utf8))
        precondition(source.sourceGroups?.count == 2, "all sources must decode without a revision")
        let group = try JSONDecoder().decode(AuthorizationGroupSummary.self, from: Data(#"{"id":"migration-abc","name":"Migrated","revision":1,"user_ids":["user"],"node_ids":["node"],"user_count":1,"node_count":1,"created_at":"2026-10-07T00:00:00Z","updated_at":"2026-10-07T00:00:00Z"}"#.utf8))
        let deleted = try await store.deleteAuthorizationGroup(group,previewToken:"guard-delete")
        precondition(deleted.group == nil && deleted.revocationJobIds == ["kick-1"])
        let deleteRequest = GroupFixtureProtocol.state.requests.first { $0.0 == "DELETE" }!
        let deleteBody = try JSONSerialization.jsonObject(with: deleteRequest.2) as! [String:Any]
        precondition(deleteBody["preview_token"] as? String == "guard-delete")
        store.users = [oldUser]
        GroupFixtureProtocol.state.failGroups = true
        await store.refreshAuthorizationGroups()
        precondition(store.users.count == 1 && store.authorizationGroupsError != nil)
        let page = AuthorizationPageState()
        page.userSearchText = "Alice"; page.selectedUserID = "user"
        page.groupSearchText = "Migrated"; page.selectedGroupID = "migration-abc"
        precondition(page.userSearchText == "Alice" && page.selectedUserID == "user")
        checkMemberDrafts(user: oldUser)
        print("Authorization client checks passed: old service fallback, source decoding, personal mTLS wire shape, deletion receipt, isolated group errors and independent tab state.")
    }

    static func checkMemberDrafts(user: UserSummary) {
        let ids = Set((0..<5_000).map { "member-\($0)" })
        let proposed = ids.subtracting(["member-0", "member-101"]).union(["new-member"])
        let changes = AuthorizationMemberChanges(original: ids, proposed: proposed)
        precondition(changes.added == ["new-member"] && changes.removed == ["member-0", "member-101"])
        precondition(changes.undoing(["member-0", "new-member"]) == ids.subtracting(["member-101"]))
        let concurrent = ids.subtracting(["member-10"]).union(["other-admin-added"])
        let rebased = changes.applying(to: concurrent)
        precondition(rebased.contains("other-admin-added") && !rebased.contains("member-10"))
        precondition(rebased.contains("new-member") && !rebased.contains("member-101"))
        let secondPreview = AuthorizationMemberChanges(original: concurrent, proposed: rebased)
        precondition(secondPreview.added == changes.added && secondPreview.removed == changes.removed)
        // Search/page selection changes affect only the currently visible rows.
        let firstPage = Set((0..<100).map { "member-\($0)" })
        let secondPage = Set((100..<200).map { "member-\($0)" })
        var selection = AuthorizationMemberSelection.merging(["member-0"], visibleIDs: firstPage, into: [])
        selection = AuthorizationMemberSelection.merging(["member-101"], visibleIDs: secondPage, into: selection)
        precondition(selection == ["member-0", "member-101"])
        selection = AuthorizationMemberSelection.merging([], visibleIDs: firstPage, into: selection)
        precondition(selection == ["member-101"], "deselecting this page must preserve other pages")
        let rows = AuthorizationMemberRow.make(users: [user], ids: ids.union([user.id]), changes: changes)
        precondition(rows.count == 5_001 && rows.first { $0.id == user.id }?.name == user.name)
        precondition(rows.first { $0.id == "member-0" }?.enabled == nil, "missing users must remain manageable by ID")
        let pairs = (0..<5_000).flatMap { index in (0..<4).map { AuthorizationPair(userId: "member-\(index)", nodeID: "node-\($0)") } }
        let impacts = AuthorizationUserImpact.grouped(pairs + [pairs[0]])
        precondition(impacts.count == 5_000 && impacts.allSatisfy { $0.nodeIDs.count == 4 })
        print("Member checks passed: 5,000 members, 20,000 connections, cross-page selection, targeted undo, missing users and concurrent membership rebase.")
    }

}
