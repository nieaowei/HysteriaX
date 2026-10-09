import Foundation

final class OverviewTransport: URLProtocol, @unchecked Sendable {
    final class Responses: @unchecked Sendable {
        let lock = NSLock()
        var mode = "advanced"
        var overview = Data()
        var nodes = Data()
        var history = Data()
        var jobs = Data("[]".utf8)
        var nodesPageRequest: URLRequest?
        func recordedNodesPage() -> URLRequest? { lock.lock(); defer { lock.unlock() }; return nodesPageRequest }
        var credentialsPageRequest: URLRequest?
        func recordedCredentialsPage() -> URLRequest? { lock.lock(); defer { lock.unlock() }; return credentialsPageRequest }
        var dnsPageRequest: URLRequest?
        func recordedDNSPage() -> URLRequest? { lock.lock(); defer { lock.unlock() }; return dnsPageRequest }
        var usersRequest: URLRequest?
        func recordedUsers() -> URLRequest? { lock.lock(); defer { lock.unlock() }; return usersRequest }
        var auditRequest: URLRequest?
        func recordedAudit() -> URLRequest? { lock.lock(); defer { lock.unlock() }; return auditRequest }
        var jobsRequest: URLRequest?
        func recordedJobs() -> URLRequest? { lock.lock(); defer { lock.unlock() }; return jobsRequest }
        var recordRemovalRequests: [URLRequest] = []
        func recordedRemovals() -> [URLRequest] { lock.lock(); defer { lock.unlock() }; return recordRemovalRequests }
        var retryRequest: URLRequest?
        func recordedRetry() -> URLRequest? { lock.lock(); defer { lock.unlock() }; return retryRequest }
        func setMode(_ value: String) { lock.lock(); defer { lock.unlock() }; mode = value }
        func response(for request: URLRequest) -> (Int, Data) {
            let url = request.url!
            let path = url.path
            lock.lock(); defer { lock.unlock() }
            if path.hasSuffix("version") {
                let features = mode == "legacy" ? "[]" : "[\"overview_monitoring\",\"job_retry_links\",\"node_record_removal\"]"
                return (200, Data("{\"api_version\":\"1.0.0\",\"service_version\":\"test\",\"hysteria_version\":\"test\",\"mihomo_version\":\"test\",\"features\":\(features)}".utf8))
            }
            if path.hasSuffix("overview/history") {
                var body = try! JSONSerialization.jsonObject(with: history) as! [String: Any]
                let query = URLComponents(url: url, resolvingAgainstBaseURL: false)!.queryItems ?? []
                for item in query where ["range", "source", "timezone"].contains(item.name) { body[item.name] = item.value }
                return (200, try! JSONSerialization.data(withJSONObject: body))
            }
            if path.hasSuffix("readyz") { return (200, Data("{\"status\":\"ok\"}".utf8)) }
            if path.hasSuffix("overview") { return (mode == "overview-failure" ? 503 : 200, overview) }
            if path.hasSuffix("server/monitoring") { return (503, Data()) }
            if path.hasSuffix("retry") { retryRequest = request; return (202, Data("{\"job_id\":\"retry-1\",\"status\":\"queued\"}".utf8)) }
            if path.hasSuffix("/record") { recordRemovalRequests.append(request); return (204, Data()) }
            if path.hasSuffix("jobs") {
                jobsRequest = request
                let items = try! JSONSerialization.jsonObject(with: jobs) as! [Any]
                let parameters = URLComponents(url: url, resolvingAgainstBaseURL: false)!.queryItems ?? []
                let size = Int(parameters.first { $0.name == "page_size" }?.value ?? "50")!
                return (200, try! JSONSerialization.data(withJSONObject: ["items": items, "total": items.count, "page": 1, "page_size": size]))
            }
            if path == "/api/v1/nodes/page" {
                nodesPageRequest = request
                return (200, Data(#"{"items":[],"total":0,"page":1,"page_size":25}"#.utf8))
            }
            if path == "/api/v1/credentials/page" {
                credentialsPageRequest = request
                return (200, Data(#"{"items":[],"total":0,"page":1,"page_size":25}"#.utf8))
            }
            if path == "/api/v1/dns/records/page" {
                dnsPageRequest = request
                return (200, Data(#"{"items":[],"total":0,"page":1,"page_size":25}"#.utf8))
            }
            if path == "/api/v1/users/page" {
                usersRequest = request
                return (200, Data(#"{"items":[],"total":0,"page":1,"page_size":25}"#.utf8))
            }
            if path.hasSuffix("audit") {
                auditRequest = request
                return (200, Data(#"{"items":[],"total":0,"page":1,"page_size":25}"#.utf8))
            }
            if path.hasSuffix("nodes") { return (200, nodes) }
            return (200, Data("[]".utf8))
        }
    }
    static let responses = Responses()
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        let (status, body) = Self.responses.response(for: request)
        let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: "HTTP/1.1", headerFields: ["Content-Type": "application/json"])!
        let deliver: @Sendable () -> Void = { [self] in
            stateLock.lock(); defer { stateLock.unlock() }
            guard !cancelled else { return }
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: body)
            client?.urlProtocolDidFinishLoading(self)
        }
        if request.url!.path.hasSuffix("overview/history") { DispatchQueue.global().asyncAfter(deadline: .now() + 0.2, execute: deliver) } else { deliver() }
    }
    private let stateLock = NSLock()
    private var cancelled = false
    override func stopLoading() { stateLock.lock(); cancelled = true; stateLock.unlock() }
}

@MainActor
struct OverviewSilentNotifications: NodeNotificationTransport {
    func requestPermission() async throws -> Bool { false }
    func isAuthorized() async -> Bool { false }
    func send(id: String, title: String, body: String) async throws {}
}

@main
struct OverviewNetworkChecks {
    @MainActor static func main() async throws {
        let folder = CommandLine.arguments[1]
        OverviewTransport.responses.overview = try Data(contentsOf: URL(fileURLWithPath: folder + "/overview.json"))
        OverviewTransport.responses.jobs = try Data(contentsOf: URL(fileURLWithPath: folder + "/jobs.json"))
        OverviewTransport.responses.nodes = try Data(contentsOf: URL(fileURLWithPath: folder + "/nodes.json"))
        OverviewTransport.responses.history = try Data(contentsOf: URL(fileURLWithPath: folder + "/history.json"))
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [OverviewTransport.self]
        let session = URLSession(configuration: configuration)
        let client = APIClient(baseURL: URL(string: "https://overview-network-fixture.invalid")!, token: "test-only", session: session)
        let notifications = NodeAlertNotifications(transport: OverviewSilentNotifications(), defaults: UserDefaults(suiteName: "com.hysteriax.overview.network-tests")!)
        let store = ManagementStore(client: client, restoreSnapshot: false, nodeNotifications: notifications)
        store.serviceAddress = "https://overview-network-fixture.invalid"
        defer {
            UserDefaults.standard.removeObject(forKey: "displaySnapshot.\(store.serviceAddress)")
            UserDefaults.standard.removeObject(forKey: "overviewHistory.\(store.serviceAddress)")
        }
        await store.refresh()
        precondition(store.isConnected && store.supportsOverviewMonitoring && store.overview?.nodeCount == 1)
        precondition(store.supportsJobRetryLinks && store.supportsNodeRecordRemoval)
        let removedNode = store.nodes[0]
        try await store.removeNodeRecord(removedNode)
        let removalRequest = OverviewTransport.responses.recordedRemovals().last!
        precondition(removalRequest.httpMethod == "DELETE" && removalRequest.url!.path == "/api/v1/nodes/\(removedNode.id)/record")
        let removalQuery = URLComponents(url: removalRequest.url!, resolvingAgainstBaseURL: false)!.queryItems!
        precondition(removalQuery.contains(URLQueryItem(name: "expected_revision", value: String(removedNode.revision))))
        let pageOperation = APIEndpoints.listJobs(page: 5, pageSize: 25, q: "历史 %_&", sort: "status", order: "asc")
        precondition(pageOperation.queryParameters == ["page": "5", "page_size": "25", "q": "历史 %_&", "sort": "status", "order": "asc"])
        let page = try await store.jobsPage(page: 5, pageSize: 25, query: "历史 %_&", sort: "status", order: "asc")
        precondition(page.total == 2 && page.items.count == 2 && page.page == 1 && page.pageSize == 25)
        let jobsRequest = OverviewTransport.responses.recordedJobs()!
        let jobsQuery = URLComponents(url: jobsRequest.url!, resolvingAgainstBaseURL: false)!.queryItems!
        precondition(Dictionary(uniqueKeysWithValues: jobsQuery.map { ($0.name, $0.value!) }) == pageOperation.queryParameters)
        let emptyPage = try JSONDecoder().decode(JobsPage.self, from: Data(#"{"items":[],"total":0,"page":1,"page_size":50}"#.utf8))
        precondition(emptyPage.items.isEmpty && emptyPage.total == 0)
        let nodeQuery = NodeDisplayText.state("unreachable")
        let nodesPage = try await store.nodesPage(page: 3, pageSize: 25, query: nodeQuery, sort: "ssh_host", order: "desc")
        precondition(nodesPage.items.isEmpty && nodesPage.total == 0 && nodesPage.page == 1)
        let nodesRequest = OverviewTransport.responses.recordedNodesPage()!
        precondition(nodesRequest.url!.path == "/api/v1/nodes/page" && nodesRequest.httpMethod == "GET")
        let nodesQuery = URLComponents(url: nodesRequest.url!, resolvingAgainstBaseURL: false)!.queryItems!
        precondition(Dictionary(uniqueKeysWithValues: nodesQuery.map { ($0.name, $0.value!) }) == ["page": "3", "page_size": "25", "q": nodeQuery, "state_matches": "unreachable", "sort": "ssh_host", "order": "desc"])
        let credentialsPage = try await store.credentialsPage(page: 3, pageSize: 25, category: "user", kind: "tls_identity", query: "历史 %_&", sort: "status", order: "desc")
        precondition(credentialsPage.items.isEmpty && credentialsPage.total == 0 && credentialsPage.page == 1)
        let credentialsRequest = OverviewTransport.responses.recordedCredentialsPage()!
        precondition(credentialsRequest.url!.path == "/api/v1/credentials/page" && credentialsRequest.httpMethod == "GET")
        let credentialsQuery = URLComponents(url: credentialsRequest.url!, resolvingAgainstBaseURL: false)!.queryItems!
        precondition(Dictionary(uniqueKeysWithValues: credentialsQuery.map { ($0.name, $0.value!) }) == ["page": "3", "page_size": "25", "category": "user", "kind": "tls_identity", "q": "历史 %_&", "sort": "status", "order": "desc"])
        let dnsPage = try await store.dnsRecordsPage(page: 4, pageSize: 25, connectionID: "connection-1", zoneID: "zone-1", query: "历史 %_&", sort: "content", order: "desc")
        precondition(dnsPage.items.isEmpty && dnsPage.total == 0 && dnsPage.page == 1)
        let dnsRequest = OverviewTransport.responses.recordedDNSPage()!
        precondition(dnsRequest.url!.path == "/api/v1/dns/records/page" && dnsRequest.httpMethod == "GET")
        let dnsQuery = URLComponents(url: dnsRequest.url!, resolvingAgainstBaseURL: false)!.queryItems!
        precondition(Dictionary(uniqueKeysWithValues: dnsQuery.map { ($0.name, $0.value!) }) == ["page": "4", "page_size": "25", "connection_id": "connection-1", "zone_id": "zone-1", "q": "历史 %_&", "sort": "content", "order": "desc"])
        let users = try await store.usersPage(page: 3, pageSize: 25, query: "历史 %_&", order: "desc")
        precondition(users.items.isEmpty && users.total == 0 && users.page == 1 && users.pageSize == 25)
        let usersRequest = OverviewTransport.responses.recordedUsers()!
        precondition(usersRequest.url!.path == "/api/v1/users/page" && usersRequest.httpMethod == "GET")
        let usersQuery = URLComponents(url: usersRequest.url!, resolvingAgainstBaseURL: false)!.queryItems!
        precondition(Dictionary(uniqueKeysWithValues: usersQuery.map { ($0.name, $0.value!) }) == ["page": "3", "page_size": "25", "q": "历史 %_&", "sort": "name", "order": "desc"])
        let auditQuery = AuditDisplayText.action("user.deleted")
        let audit = try await store.auditPage(page: 2, pageSize: 25, query: auditQuery, sort: "entity_id", order: "asc")
        precondition(audit.items.isEmpty && audit.total == 0 && audit.page == 1 && audit.pageSize == 25)
        let auditRequest = OverviewTransport.responses.recordedAudit()!
        precondition(auditRequest.url!.path == "/api/v1/audit" && auditRequest.httpMethod == "GET")
        let auditParameters = URLComponents(url: auditRequest.url!, resolvingAgainstBaseURL: false)!.queryItems!
        precondition(auditParameters.contains(URLQueryItem(name: "q", value: auditQuery)))
        precondition(auditParameters.contains(URLQueryItem(name: "action_matches", value: "user.deleted")))
        precondition(auditParameters.contains(URLQueryItem(name: "page", value: "2")))
        precondition(auditParameters.contains(URLQueryItem(name: "sort", value: "entity_id")))
        try await store.retryJob(store.jobs[0], on: store.nodes[0])
        let retryRequest = OverviewTransport.responses.recordedRetry()!
        precondition(retryRequest.url!.path == "/api/v1/jobs/job-1/retry" && retryRequest.httpMethod == "POST")
        var retryBody = retryRequest.httpBody ?? Data()
        if retryBody.isEmpty, let stream = retryRequest.httpBodyStream {
            stream.open(); defer { stream.close() }
            var buffer = [UInt8](repeating: 0, count: 1024)
            while stream.hasBytesAvailable { let count = stream.read(&buffer, maxLength: buffer.count); if count <= 0 { break }; retryBody.append(contentsOf: buffer.prefix(count)) }
        }
        let revisionRequest = try JSONSerialization.jsonObject(with: retryBody) as! [String: Any]
        precondition(revisionRequest["expected_revision"] as? Int == store.nodes[0].revision)
        OverviewTransport.responses.setMode("overview-failure")
        await store.refresh()
        precondition(store.isConnected && store.overview?.nodeCount == 1 && store.overviewError != nil)
        _ = try await store.overviewHistory(range: "7d", nodeID: nil, source: "users")
        precondition(store.cachedOverviewHistory(range: "7d", nodeID: nil, source: "users") != nil)
        let cancelled = Task { try await store.overviewHistory(range: "30d", nodeID: nil, source: "users") }
        try await Task.sleep(for: .milliseconds(30))
        cancelled.cancel()
        do { _ = try await cancelled.value; preconditionFailure("Cancelled history should not be applied") } catch {}
        precondition(store.cachedOverviewHistory(range: "30d", nodeID: nil, source: "users") == nil)
        let oldService = store.serviceAddress
        let switching = Task { try await store.overviewHistory(range: "30d", nodeID: nil, source: "users") }
        try await Task.sleep(for: .milliseconds(30))
        try await store.connect(serviceAddress: "https://overview-other-fixture.invalid", token: "test-only", session: session)
        do { _ = try await switching.value; preconditionFailure("Previous-service history should not be applied") } catch {}
        precondition(store.cachedOverviewHistory(range: "30d", nodeID: nil, source: "users") == nil)
        UserDefaults.standard.removeObject(forKey: "displaySnapshot.\(oldService)")
        UserDefaults.standard.removeObject(forKey: "overviewHistory.\(oldService)")
        UserDefaults.standard.removeObject(forKey: "displaySnapshot.\(store.serviceAddress)")
        UserDefaults.standard.removeObject(forKey: "overviewHistory.\(store.serviceAddress)")
        store.serviceAddress = "https://overview-network-fixture.invalid"
        UserDefaults.standard.removeObject(forKey: "serviceAddress")
        OverviewTransport.responses.setMode("legacy")
        await store.refresh()
        precondition(store.isConnected && !store.supportsOverviewMonitoring && !store.supportsJobRetryLinks && store.overview == nil)
        precondition(!store.supportsNodeRecordRemoval)
        let removalCount = OverviewTransport.responses.recordedRemovals().count
        do { try await store.removeNodeRecord(store.nodes[0]); preconditionFailure("Legacy service must not receive a record-removal request") } catch {}
        precondition(OverviewTransport.responses.recordedRemovals().count == removalCount)
        do { try await store.retryJob(store.jobs[0], on: store.nodes[0]); preconditionFailure("Legacy service should require an update for linked retry") } catch {}
        print("Advanced/legacy services, isolated failure, cached history, cancellation and service-switch checks passed")
    }
}
