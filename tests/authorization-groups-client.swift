import Foundation

private final class FixtureState: @unchecked Sendable {
    let lock = NSLock()
    var requests: [(String, String, Data, String?)] = []
    var failGroups = false
    private var holdingReads = false
    private var heldReads: [GroupFixtureProtocol] = []
    func holdRead(_ request: GroupFixtureProtocol) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard holdingReads else { return false }
        heldReads.append(request)
        return true
    }
    var hasHeldRead: Bool {
        lock.lock(); defer { lock.unlock() }
        return !heldReads.isEmpty
    }
    func beginHoldingReads() {
        lock.lock(); defer { lock.unlock() }
        precondition(heldReads.isEmpty)
        holdingReads = true
    }
    func releaseReads() {
        lock.lock()
        holdingReads = false
        let reads = heldReads
        heldReads = []
        lock.unlock()
        for read in reads { read.finishLoading() }
    }
    func cancel(_ request: GroupFixtureProtocol) {
        lock.lock(); defer { lock.unlock() }
        heldReads.removeAll { $0 === request }
    }
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
        requests.append((request.httpMethod ?? "GET", request.url!.path, body, request.value(forHTTPHeaderField: "Idempotency-Key")))
        return body
    }
}

private final class GroupFixtureProtocol: URLProtocol, @unchecked Sendable {
    static let state = FixtureState()
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        _ = Self.state.record(request)
        if request.httpMethod == "GET", Self.state.holdRead(self) { return }
        finishLoading()
    }
    func finishLoading() {
        let path = request.url!.path
        let method = request.httpMethod ?? "GET"
        let receipt = #"{"revocation_job_ids":["kick-1"]}"#
        var status = 200
        let data: Data
        if path == "/api/v2/credentials/ca-identities" || path.hasSuffix("/ca-versions") {
            data = Data(#"{"id":"ca","revision":2,"version":2,"batch_id":"batch","rotation_id":"rotation","published":false}"#.utf8)
        }
        else if path.hasSuffix("/ca-policy") {
            data = Data(#"{"id":"ca","policy_revision":4,"policy":{"client_validity_days":90,"renew_before_hours":168}}"#.utf8)
        }
        else if path.hasSuffix("/preview") { preconditionFailure("authorization previews are retired") }
        else if method == "DELETE" || method == "POST" && path == "/api/v2/authorization-groups" || method == "PATCH" && path.contains("/authorization-groups/") { data = Data(receipt.utf8) }
        else if method == "PUT" && path.hasSuffix("/authorization-groups") { data = Data(#"{"user_id":"user","revision":2,"group_ids":[],"assignments":[],"additions_count":0,"removals_count":1,"created_credentials":[],"revocation_job_ids":["kick-1"]}"#.utf8) }
        else if path == "/api/v2/version" { data = Data(#"{"api_version":"2.0.0","management_protocol":2,"directory_version":"b3:test","features":["authorization_groups","managed_mtls"],"service_version":"fixture","hysteria_version":"fixture","mihomo_version":"fixture"}"#.utf8) }
        else if path == "/api/v2/authorization-groups" && Self.state.failGroups { status = 503; data = Data(#"{"error":{"code":"temporary","message":"groups unavailable"}}"#.utf8) }
        else if ["/api/v2/authorization-groups", "/api/v2/jobs", "/api/v2/audit", "/api/v2/nodes", "/api/v2/users", "/api/v2/credentials", "/api/v2/dns/records"].contains(path) { data = Data(#"{"items":[],"total":0,"page":1,"page_size":50}"#.utf8) }
        else { data = Data("[]".utf8) }
        client?.urlProtocol(self, didReceive: HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: "HTTP/1.1", headerFields: ["Content-Type":"application/json"])!, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: data)
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() { Self.state.cancel(self) }
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
        precondition(GroupFixtureProtocol.state.requests.contains { $0.1 == "/api/v2/authorization-groups" })
        _ = try await store.createAuthorizationGroup(name:"Test")
        let createRequest = GroupFixtureProtocol.state.requests.first { $0.0 == "POST" && $0.1 == "/api/v2/authorization-groups" }!
        let sent = try JSONSerialization.jsonObject(with:createRequest.2) as! [String:Any]
        precondition(Set(sent.keys) == ["name"] && sent["name"] as? String == "Test")
        let reissue=ManagedUserReissueRequest(caCredentialId:"ca",expectedRevisions:["ca":.integer(3)])
        let encoded=try JSONSerialization.jsonObject(with:JSONEncoder().encode(reissue)) as! [String:Any]
        precondition((encoded["expected_revisions"] as? [String:Int])?["ca"] == 3)
        let source = try JSONDecoder().decode(AssignmentInfo.self, from: Data(#"{"node_id":"node","created_at":"2026-10-07T00:00:00Z","source_groups":[{"id":"migration-abc","name":"Migrated"},{"id":"group-b","name":"Second"}]}"#.utf8))
        precondition(source.sourceGroups?.count == 2, "all sources must decode without a revision")
        let group = try JSONDecoder().decode(AuthorizationGroupSummary.self, from: Data(#"{"id":"migration-abc","name":"Migrated","revision":1,"user_ids":["user"],"node_ids":["node"],"user_count":1,"node_count":1,"created_at":"2026-10-07T00:00:00Z","updated_at":"2026-10-07T00:00:00Z"}"#.utf8))
        let beforeUpdate = GroupFixtureProtocol.state.requests.count
        let updated = try await store.updateAuthorizationGroup(group, removeUserIDs: ["user"])
        precondition(updated.revocationJobIds == ["kick-1"])
        let updates = Array(GroupFixtureProtocol.state.requests.dropFirst(beforeUpdate))
        precondition(!updates.contains { $0.1.hasSuffix("/preview") }, "updates must not create hidden previews")
        let updateRequest = updates.first { $0.0 == "PATCH" }!
        let updateBody = try JSONSerialization.jsonObject(with: updateRequest.2) as! [String:Any]
        precondition(updateBody["preview_token"] == nil)
        precondition(updateBody["expected_revision"] as? Int == 1)
        precondition(Set(updateBody.keys) == ["expected_revision", "remove_user_ids"])
        precondition(updateBody["remove_user_ids"] as? [String] == ["user"])
        _ = try await store.updateAuthorizationGroup(group, name: "Renamed")
        let renameRequest = GroupFixtureProtocol.state.requests.last { $0.0 == "PATCH" && $0.1.contains("/authorization-groups/") }!
        let renameBody = try JSONSerialization.jsonObject(with: renameRequest.2) as! [String:Any]
        precondition(Set(renameBody.keys) == ["expected_revision", "name"])
        _ = try await store.updateAuthorizationGroup(group, addNodeIDs: ["node-2"], removeNodeIDs: ["node"])
        let nodeRequest = GroupFixtureProtocol.state.requests.last { $0.0 == "PATCH" && $0.1.contains("/authorization-groups/") }!
        let nodeBody = try JSONSerialization.jsonObject(with: nodeRequest.2) as! [String:Any]
        precondition(Set(nodeBody.keys) == ["expected_revision", "add_node_ids", "remove_node_ids"])
        precondition(nodeBody["add_node_ids"] as? [String] == ["node-2"])
        let deleted = try await store.deleteAuthorizationGroup(group)
        precondition(deleted.revocationJobIds == ["kick-1"])
        let deleteRequest = GroupFixtureProtocol.state.requests.first { $0.0 == "DELETE" }!
        let deleteBody = try JSONSerialization.jsonObject(with: deleteRequest.2) as! [String:Any]
        precondition(deleteBody["preview_token"] == nil)
        let membership = try await store.updateUserAuthorizationGroups(oldUser, groupIDs: [])
        precondition(membership.removalsCount == 1)
        let membershipRequest = GroupFixtureProtocol.state.requests.first { $0.0 == "PUT" && $0.1 == "/api/v2/users/user/authorization-groups" }!
        let membershipBody = try JSONSerialization.jsonObject(with: membershipRequest.2) as! [String:Any]
        precondition(membershipBody["preview_token"] == nil)
        precondition(!GroupFixtureProtocol.state.requests.contains { $0.1.hasSuffix("/preview") })
        await store.refresh()
        store.users = [oldUser]
        GroupFixtureProtocol.state.failGroups = true
        await store.refreshAuthorizationGroups()
        precondition(store.users.count == 1 && store.authorizationGroupsError != nil)
        let page = AuthorizationPageState()
        page.userSearchText = "Alice"; page.selectedUserID = "user"
        page.userPage = 3; page.userPageSize = 25
        page.groupSearchText = "Migrated"; page.selectedGroupID = "migration-abc"
        precondition(page.userSearchText == "Alice" && page.selectedUserID == "user")
        precondition(page.userPage == 3 && page.userPageSize == 25)
        checkMemberDrafts(user: oldUser)
        try await checkManagedMTLS(store: store)
        GroupFixtureProtocol.state.failGroups = false
        await store.refresh()
        try await checkSaveDoesNotWaitForRefresh(client: client, user: oldUser, group: group)
        print("Authorization client checks passed: automatic issuance wire shape, integer identity revisions, source decoding, deletion receipt, isolated group errors and independent tab state.")
    }

    @MainActor static func checkSaveDoesNotWaitForRefresh(client: APIClient, user: UserSummary, group: AuthorizationGroupSummary) async throws {
        for operation in 0..<4 {
            let defaults = UserDefaults(suiteName: "save-feedback-" + UUID().uuidString)!
            let store = ManagementStore(client: client, restoreSnapshot: false, nodeNotifications: NodeAlertNotifications(transport: SilentNotifications(), defaults: defaults))
            store.isConnected = true
            store.serviceAddress = "https://authorization-fixture.invalid"
            GroupFixtureProtocol.state.beginHoldingReads()
            var completed = false
            let mutation = Task {
                switch operation {
                case 0: _ = try await store.createAuthorizationGroup(name: "Immediate")
                case 1: _ = try await store.updateAuthorizationGroup(group, removeNodeIDs: ["node"])
                case 2: _ = try await store.deleteAuthorizationGroup(group)
                default: _ = try await store.updateUserAuthorizationGroups(user, groupIDs: [])
                }
                completed = true
            }
            let deadline = Date().addingTimeInterval(5)
            while !GroupFixtureProtocol.state.hasHeldRead, Date() < deadline {
                try await Task.sleep(for: .milliseconds(5))
            }
            let hadBlockedRefresh = GroupFixtureProtocol.state.hasHeldRead
            let completedBeforeRead = completed
            GroupFixtureProtocol.state.releaseReads()
            try await mutation.value
            await store.refresh()
            precondition(hadBlockedRefresh, "background refresh must still be scheduled")
            precondition(completedBeforeRead, "committed authorization save must return while refresh reads are blocked: operation \(operation)")
        }
        print("Save feedback checks passed: all four authorization mutations return before blocked refresh reads finish.")
    }

    @MainActor static func checkManagedMTLS(store: ManagementStore) async throws {
        var draft = ManagedMTLSDraft()
        precondition(draft.mode == "off" && draft.configuration.objectValue?["ca_credential_id"] == nil)
        draft.mode = "managed"
        precondition(draft.validationError != nil)
        draft.caID = "shared-ca"; draft.caVersion = 7
        precondition(draft.validationError == nil)
        let config: [String: JSONValue] = ["mtls": draft.configuration]
        let restored = ManagedMTLSDraft(configuration: config)
        precondition(restored.mode == "managed" && restored.caID == "shared-ca" && restored.caVersion == 7)
        let encoded = try JSONSerialization.jsonObject(with: JSONEncoder().encode(draft.configuration)) as! [String: Any]
        precondition(encoded["ca_credential_version"] as? Int == 7 && encoded["clientCA"] == nil)
        draft.mode = "off"
        precondition(draft.configuration.objectValue?.count == 1)
        let policy = ManagedCAPolicy(clientValidityDays: 90, renewBeforeHours: 168)
        let _: CAIdentityReceipt = try await store.createCAIdentity(CAIdentityCreateRequest(name: "Shared", source: "generated", policy: policy))
        try await store.updateCAIdentityPolicy("ca", request: CAPolicyUpdateRequest(expectedRevision: 3, policy: policy))
        let receipt = try await store.publishCAIdentity("ca", request: CAIdentityPublishRequest(expectedRevision: 2, replaceRotationId: "old-rotation"))
        precondition(receipt.rotationId == "rotation" && receipt.published == false)
        let requests = GroupFixtureProtocol.state.requests
        let policyRequest = requests.first { $0.1.hasSuffix("/ca-policy") }!
        precondition(policyRequest.0 == "PATCH")
        let policyBody = try JSONSerialization.jsonObject(with: policyRequest.2) as! [String:Any]
        precondition(policyBody["expected_revision"] as? Int == 3 && policyBody["idempotency_key"] == nil && policyRequest.3?.isEmpty == false)
        let publishRequest = requests.first { $0.1.hasSuffix("/ca-versions") }!
        let publishBody = try JSONSerialization.jsonObject(with: publishRequest.2) as! [String:Any]
        precondition(publishBody["replace_rotation_id"] as? String == "old-rotation" && publishBody["idempotency_key"] == nil && publishRequest.3?.isEmpty == false && publishRequest.3 != policyRequest.3)
        print("Managed mTLS client checks passed: explicit off/managed settings, exact CA version, dedicated CA routes, independent policy revision, persisted operation keys and explicit target replacement.")
    }

    static func checkMemberDrafts(user: UserSummary) {
        var draft = AuthorizationRelationDraft()
        let first = AuthorizationRelationRow.placeholder("member-0")
        let second = AuthorizationRelationRow.placeholder("member-101")
        let added = AuthorizationRelationRow.placeholder("new-member")
        draft.setMember(false, row: first, originallyMember: true)
        draft.setMember(false, row: second, originallyMember: true)
        draft.setMember(true, row: added, originallyMember: false)
        precondition(Set(draft.removed.keys) == [first.id, second.id])
        precondition(Set(draft.added.keys) == [added.id])
        // Cancel an explicit change without ever loading the rest of the group's members.
        draft.setMember(true, row: first, originallyMember: true)
        draft.undo([added.id])
        precondition(draft.added.isEmpty && Set(draft.removed.keys) == [second.id])
        precondition(draft.rows.count == 1, "unvisited members must never enter the patch")
        draft.undo([second.id])
        precondition(draft.isEmpty)
        print("Member checks passed: bounded deltas, cross-page edits and targeted undo.")
    }

}
