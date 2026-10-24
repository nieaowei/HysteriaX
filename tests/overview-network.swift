import Foundation

final class OverviewTransport: URLProtocol, @unchecked Sendable {
    final class Responses: @unchecked Sendable {
        let lock = NSLock()
        var mode = "advanced"
        private var versionRequests = 0
        private var holdVersions = false
        private var heldVersions: [@Sendable () -> Void] = []
        func versionCount() -> Int { lock.lock(); defer { lock.unlock() }; return versionRequests }
        func heldVersionCount() -> Int { lock.lock(); defer { lock.unlock() }; return heldVersions.count }
        func setHoldVersions(_ hold: Bool) { lock.lock(); defer { lock.unlock() }; holdVersions = hold }
        func holdVersion(_ deliver: @escaping @Sendable () -> Void) -> Bool {
            lock.lock(); defer { lock.unlock() }
            guard holdVersions else { return false }
            heldVersions.append(deliver)
            return true
        }
        func releaseVersion() {
            lock.lock(); let deliver = heldVersions.removeFirst(); lock.unlock()
            deliver()
        }
        var overview = Data()
        var monitoring = Data()
        var monitoringRequest: URLRequest?
        func recordedMonitoring() -> URLRequest? { lock.lock(); defer { lock.unlock() }; return monitoringRequest }
        var nodes = Data()
        var history = Data()
        var jobs = Data("[]".utf8)
        var pageRequests = 0
        func recordedPageCount() -> Int { lock.lock(); defer { lock.unlock() }; return pageRequests }
        var directoryRequests = 0
        func recordedDirectoryCount() -> Int { lock.lock(); defer { lock.unlock() }; return directoryRequests }
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
            if ["/api/v1/nodes", "/api/v1/users", "/api/v1/credentials", "/api/v1/jobs", "/api/v1/audit", "/api/v1/dns/records"].contains(path) { pageRequests += 1 }
            if path == "/api/v1/nodes", request.httpMethod == "POST" {
                return (201, Data("{\"node\":{\"id\":\"created-node\",\"name\":\"Created node\",\"revision\":1,\"state\":\"new\"},\"node_auth_token\":\"test-created-token\"}".utf8))
            }
            if path.hasSuffix("version") {
                versionRequests += 1
                if mode == "version-failure" { return (503, Data()) }
                let features = mode == "legacy" ? "[]" : "[\"overview_monitoring\",\"job_retry_links\",\"node_record_removal\",\"client_summary\"]"
                return (200, Data("{\"api_version\":\"1.0.0\",\"directory_generation\":1,\"service_version\":\"test\",\"hysteria_version\":\"test\",\"mihomo_version\":\"test\",\"features\":\(features)}".utf8))
            }
            if path == "/api/v1/client/summary" {
                let fixture = try! JSONSerialization.jsonObject(with: nodes) as! [[String: Any]]
                return (200, try! JSONSerialization.data(withJSONObject: ["node_count":235,"user_total":400,"user_enabled":380,"user_expired":2,"user_expiring":3,"user_depleted":1,"notification_nodes":fixture,"notification_credentials":[]]))
            }
            if path == "/api/v1/overview/nodes" {
                monitoringRequest = request
                let parameters = URLComponents(url: url, resolvingAgainstBaseURL: false)!.queryItems ?? []
                let size = Int(parameters.first { $0.name == "page_size" }?.value ?? "20")!
                let requested = Int(parameters.first { $0.name == "page" }?.value ?? "1")!
                let search = parameters.first { $0.name == "q" }?.value ?? ""
                var response = try! JSONSerialization.jsonObject(with: monitoring) as! [String: Any]
                let template = (response["items"] as! [[String: Any]])[0]
                let items = (1...235).map { i -> [String: Any] in
                    var item = template; item["node_id"] = "monitor-\(i)"; item["name"] = "Monitoring \(i)"; return item
                }.filter { search.isEmpty || ($0["name"] as! String).localizedCaseInsensitiveContains(search) }
                let page = min(requested,max(1,(items.count+size-1)/size))
                response["items"] = Array(items.dropFirst((page-1)*size).prefix(size))
                response["total"] = items.count; response["page"] = page; response["page_size"] = size
                return (200,try! JSONSerialization.data(withJSONObject: response))
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
            if path == "/api/v1/nodes" {
                nodesPageRequest = request
                let parameters = URLComponents(url: url, resolvingAgainstBaseURL: false)!.queryItems ?? []
                let size = Int(parameters.first { $0.name == "page_size" }?.value ?? "50")!
                if size == 200 { directoryRequests += 1 }
                let requested = Int(parameters.first { $0.name == "page" }?.value ?? "1")!
                let fixture = try! JSONSerialization.jsonObject(with: nodes) as! [[String: Any]]
                let items: [[String: Any]] = parameters.contains { $0.name == "q" && !($0.value ?? "").isEmpty } ? [] : (1...235).map { index in
                    var node = fixture[0]; node["id"] = "node-\(index)"; return node
                }
                let page = min(requested, max(1, (items.count + size - 1) / size))
                return (200, try! JSONSerialization.data(withJSONObject: ["items": Array(items.dropFirst((page - 1) * size).prefix(size)), "total": items.count, "page": page, "page_size": size]))
            }
            if path == "/api/v1/credentials" {
                credentialsPageRequest = request
                return (200, Data(#"{"items":[],"total":0,"page":1,"page_size":25}"#.utf8))
            }
            if path == "/api/v1/dns/records" {
                dnsPageRequest = request
                return (200, Data(#"{"items":[],"total":0,"page":1,"page_size":25}"#.utf8))
            }
            if path == "/api/v1/users" {
                usersRequest = request
                return (200, Data(#"{"items":[],"total":0,"page":1,"page_size":25}"#.utf8))
            }
            if path.hasSuffix("audit") {
                auditRequest = request
                return (200, Data(#"{"items":[],"total":0,"page":1,"page_size":25}"#.utf8))
            }
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
        if request.url!.path.hasSuffix("version"), Self.responses.holdVersion(deliver) { return }
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
    @MainActor static func waitForHeldVersion() async throws {
        let deadline = Date().addingTimeInterval(5)
        while OverviewTransport.responses.heldVersionCount() == 0, Date() < deadline {
            try await Task.sleep(for: .milliseconds(5))
        }
        precondition(OverviewTransport.responses.heldVersionCount() == 1)
    }

    @MainActor static func main() async throws {
        let folder = CommandLine.arguments[1]
        OverviewTransport.responses.monitoring = try Data(contentsOf: URL(fileURLWithPath: folder + "/overview-nodes.json"))
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
        precondition(store.nodes.count == 50 && Set(store.nodes.map(\.id)).count == 50, "bootstrap must load only the first page")
        precondition(store.clientSummary?.nodeCount == 235 && store.clientSummary?.userTotal == 400)
        precondition(OverviewTransport.responses.recordedDirectoryCount() == 0, "bootstrap must not traverse directories")
        precondition(store.supportsJobRetryLinks && store.supportsNodeRecordRemoval)
        // A committed creation must return its one-time token even while the
        // following refresh is blocked on the network.
        OverviewTransport.responses.setHoldVersions(true)
        var creationCompleted = false
        let creation = Task {
            let receipt = try await store.createNode(NodeCreateRequest(
                name: "Created node", sshHost: "127.0.0.1", sshPort: 22, sshUsername: "root",
                publicHost: "node.example.test", publicPort: 443, listenAddr: ":443",
                sshCredentialId: "fixture-ssh", sshCredentialVersion: 1
            ))
            precondition(receipt.nodeAuthToken == "test-created-token")
            creationCompleted = true
        }
        try await waitForHeldVersion()
        for _ in 0..<20 { await Task.yield() }
        let returnedBeforeRefresh = creationCompleted
        let cachedCreatedNode = store.nodes.contains { $0.id == "created-node" && $0.state == "new" }
        OverviewTransport.responses.setHoldVersions(false)
        OverviewTransport.responses.releaseVersion()
        try await creation.value
        await store.refresh(forceDirectories: false)
        precondition(returnedBeforeRefresh, "creation must not await the background refresh")
        precondition(cachedCreatedNode, "creation must update the local node directory immediately")
        // Freeze each version response to deterministically exercise overlapping
        // refresh triggers without relying on network timing.
        let versionsBefore = OverviewTransport.responses.versionCount()
        OverviewTransport.responses.setHoldVersions(true)
        var refreshWaitersCompleted = 0
        let ordinary = Task { await store.refresh(forceDirectories: false); refreshWaitersCompleted += 1 }
        try await waitForHeldVersion()
        let joining = Task { await store.refresh(forceDirectories: false); refreshWaitersCompleted += 1 }
        let forceA = Task { await store.refresh(); refreshWaitersCompleted += 1 }
        let forceB = Task { await store.refresh(); refreshWaitersCompleted += 1 }
        for _ in 0..<20 { await Task.yield() }
        ordinary.cancel() // Cancelling a consumer must not cancel the shared round.
        precondition(store.isLoading && refreshWaitersCompleted == 0)
        precondition(OverviewTransport.responses.versionCount() == versionsBefore + 1)
        OverviewTransport.responses.releaseVersion()
        try await waitForHeldVersion()
        precondition(OverviewTransport.responses.versionCount() == versionsBefore + 2)
        precondition(store.isLoading && refreshWaitersCompleted == 0)
        let forceC = Task { await store.refresh(); refreshWaitersCompleted += 1 }
        let forceD = Task { await store.refresh(); refreshWaitersCompleted += 1 }
        for _ in 0..<20 { await Task.yield() }
        OverviewTransport.responses.releaseVersion()
        try await waitForHeldVersion()
        precondition(OverviewTransport.responses.versionCount() == versionsBefore + 3)
        OverviewTransport.responses.setHoldVersions(false)
        OverviewTransport.responses.releaseVersion()
        await ordinary.value; await joining.value; await forceA.value; await forceB.value; await forceC.value; await forceD.value
        precondition(refreshWaitersCompleted == 6 && !store.isLoading && store.isConnected)
        precondition(OverviewTransport.responses.versionCount() == versionsBefore + 3)
        OverviewTransport.responses.setMode("version-failure")
        await store.refresh(forceDirectories: false)
        precondition(!store.isConnected && !store.isLoading && store.errorMessage != nil)
        OverviewTransport.responses.setMode("advanced")
        await store.refresh(forceDirectories: false)
        precondition(store.isConnected && !store.isLoading && store.errorMessage == nil)
        let monitorKey = store.listPageKey(["3", "20", ""])
        let monitoring = try await store.overviewNodesPage(page: 3, pageSize: 20, query: "", cacheKey: monitorKey, revalidate: true)
        precondition(monitoring.total == 235 && monitoring.items.count == 20 && monitoring.items[0].nodeID == "monitor-41")
        precondition(store.cachedOverviewNodesPage(monitorKey)?.page == 3)
        let monitorSearch = "Monitoring 235"
        let searchKey = store.listPageKey(["1", "20", monitorSearch])
        let matched = try await store.overviewNodesPage(page: 1, pageSize: 20, query: monitorSearch, cacheKey: searchKey, revalidate: true)
        precondition(matched.total == 1 && matched.items[0].nodeID == "monitor-235")
        let monitorQuery = URLComponents(url: OverviewTransport.responses.recordedMonitoring()!.url!, resolvingAgainstBaseURL: false)!.queryItems!
        precondition(monitorQuery.contains(URLQueryItem(name: "q", value: monitorSearch)))
        precondition(store.overview?.nodeCount == 1, "monitoring filters must not replace global summary metrics")
        let directoryCount = OverviewTransport.responses.recordedDirectoryCount()
        let refreshToken = store.dataRefreshToken
        await store.refresh(forceDirectories: false)
        precondition(store.isConnected && OverviewTransport.responses.recordedDirectoryCount() == directoryCount, "periodic overview refresh must not reload unchanged directories")
        precondition(store.dataRefreshToken != refreshToken)
        await store.refresh()
        precondition(OverviewTransport.responses.recordedDirectoryCount() == directoryCount, "manual refresh must remain bounded")
        precondition(OverviewTransport.responses.recordedNodesPage()!.value(forHTTPHeaderField: "Cache-Control") == "no-cache")
        async let first: Void = store.prepareDirectory(.nodes)
        async let second: Void = store.prepareDirectory(.nodes)
        _ = await (first, second)
        precondition(store.nodes.count == 235 && Set(store.nodes.map(\.id)).count == 235)
        precondition(OverviewTransport.responses.recordedDirectoryCount() == directoryCount + 2, "on-demand directory reads must be deduplicated")
        await store.refresh()
        precondition(store.nodes.count == 50 && OverviewTransport.responses.recordedDirectoryCount() == directoryCount + 2, "refresh must not eagerly rebuild selector directories")
        let restored = UserDefaults.standard.data(forKey: "displaySnapshot.\(store.serviceAddress)")!
        let display = try JSONSerialization.jsonObject(with: restored) as! [String: Any]
        precondition((display["nodes"] as! [Any]).count <= 50, "disk snapshots must remain bounded")
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
        let sharedKey = "jobs-page-five"
        _ = try await store.jobsPage(page: 5, pageSize: 25, query: "历史 %_&", sort: "status", order: "asc", cacheKey: sharedKey)
        precondition(store.cachedListPage(sharedKey, as: JobsPage.self)?.total == 2)
        let beforeReuse = OverviewTransport.responses.recordedPageCount()
        _ = try await store.jobsPage(page: 5, pageSize: 25, query: "历史 %_&", sort: "status", order: "asc", cacheKey: sharedKey)
        precondition(OverviewTransport.responses.recordedPageCount() == beforeReuse, "re-entering a page must reuse its shared cache")
        await store.refresh(forceDirectories: false)
        _ = try await store.jobsPage(page: 5, pageSize: 25, query: "历史 %_&", sort: "status", order: "asc", cacheKey: sharedKey)
        precondition(OverviewTransport.responses.recordedPageCount() == beforeReuse, "unrelated overview refresh must not refetch a fresh page")
        _ = try await store.jobsPage(page: 5, pageSize: 25, query: "历史 %_&", sort: "status", order: "asc", cacheKey: sharedKey, revalidate: true)
        precondition(OverviewTransport.responses.recordedPageCount() == beforeReuse + 1, "page entry must revalidate even a fresh cache")
        precondition(OverviewTransport.responses.recordedJobs()!.value(forHTTPHeaderField: "Cache-Control") == "no-cache")
        await store.refresh()
        let afterManual = OverviewTransport.responses.recordedPageCount()
        _ = try await store.jobsPage(page: 5, pageSize: 25, query: "历史 %_&", sort: "status", order: "asc", cacheKey: sharedKey)
        precondition(OverviewTransport.responses.recordedPageCount() == afterManual + 1)
        precondition(OverviewTransport.responses.recordedJobs()!.value(forHTTPHeaderField: "Cache-Control") == "no-cache")
        let emptyPage = try JSONDecoder().decode(JobsPage.self, from: Data(#"{"items":[],"total":0,"page":1,"page_size":50}"#.utf8))
        precondition(emptyPage.items.isEmpty && emptyPage.total == 0)
        let nodeQuery = NodeDisplayText.state("unreachable")
        let nodesPage = try await store.nodesPage(page: 3, pageSize: 25, query: nodeQuery, sort: "ssh_host", order: "desc")
        precondition(nodesPage.items.isEmpty && nodesPage.total == 0 && nodesPage.page == 1)
        let nodesRequest = OverviewTransport.responses.recordedNodesPage()!
        precondition(nodesRequest.url!.path == "/api/v1/nodes" && nodesRequest.httpMethod == "GET")
        let nodesQuery = URLComponents(url: nodesRequest.url!, resolvingAgainstBaseURL: false)!.queryItems!
        precondition(Dictionary(uniqueKeysWithValues: nodesQuery.map { ($0.name, $0.value!) }) == ["page": "3", "page_size": "25", "q": nodeQuery, "state_matches": "unreachable", "sort": "ssh_host", "order": "desc"])
        let credentialsPage = try await store.credentialsPage(page: 3, pageSize: 25, category: "user", kind: "tls_identity", query: "历史 %_&", sort: "status", order: "desc")
        precondition(credentialsPage.items.isEmpty && credentialsPage.total == 0 && credentialsPage.page == 1)
        let credentialsRequest = OverviewTransport.responses.recordedCredentialsPage()!
        precondition(credentialsRequest.url!.path == "/api/v1/credentials" && credentialsRequest.httpMethod == "GET")
        let credentialsQuery = URLComponents(url: credentialsRequest.url!, resolvingAgainstBaseURL: false)!.queryItems!
        precondition(Dictionary(uniqueKeysWithValues: credentialsQuery.map { ($0.name, $0.value!) }) == ["page": "3", "page_size": "25", "category": "user", "kind": "tls_identity", "include_internal": "false", "q": "历史 %_&", "sort": "status", "order": "desc"])
        let dnsPage = try await store.dnsRecordsPage(page: 4, pageSize: 25, connectionID: "connection-1", zoneID: "zone-1", query: "历史 %_&", sort: "content", order: "desc")
        precondition(dnsPage.items.isEmpty && dnsPage.total == 0 && dnsPage.page == 1)
        let dnsRequest = OverviewTransport.responses.recordedDNSPage()!
        precondition(dnsRequest.url!.path == "/api/v1/dns/records" && dnsRequest.httpMethod == "GET")
        let dnsQuery = URLComponents(url: dnsRequest.url!, resolvingAgainstBaseURL: false)!.queryItems!
        precondition(Dictionary(uniqueKeysWithValues: dnsQuery.map { ($0.name, $0.value!) }) == ["page": "4", "page_size": "25", "connection_id": "connection-1", "zone_id": "zone-1", "q": "历史 %_&", "sort": "content", "order": "desc"])
        let users = try await store.usersPage(page: 3, pageSize: 25, query: "历史 %_&", order: "desc")
        precondition(users.items.isEmpty && users.total == 0 && users.page == 1 && users.pageSize == 25)
        let usersRequest = OverviewTransport.responses.recordedUsers()!
        precondition(usersRequest.url!.path == "/api/v1/users" && usersRequest.httpMethod == "GET")
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
        try await store.retryJob(store.jobs[0], on: removedNode)
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
        let oldSession = store.listCacheSession
        let oldService = store.serviceAddress
        let switching = Task { try await store.overviewHistory(range: "30d", nodeID: nil, source: "users") }
        OverviewTransport.responses.setHoldVersions(true)
        let oldRefresh = Task { await store.refresh(forceDirectories: false) }
        try await waitForHeldVersion()
        precondition(store.isLoading)
        OverviewTransport.responses.setHoldVersions(false)
        try await Task.sleep(for: .milliseconds(30))
        try await store.connect(serviceAddress: "https://overview-other-fixture.invalid", token: "test-only", session: session)
        OverviewTransport.responses.releaseVersion()
        await oldRefresh.value
        precondition(!store.isLoading && store.isConnected, "old refresh cleanup must not alter the new session")
        precondition(store.listCacheSession != oldSession)
        precondition(store.cachedListPage(sharedKey, as: JobsPage.self) == nil)
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
        precondition(store.nodes.count == 235, "legacy servers must retain complete-directory behavior until upgraded")
        precondition(!store.supportsNodeRecordRemoval)
        let removalCount = OverviewTransport.responses.recordedRemovals().count
        do { try await store.removeNodeRecord(store.nodes[0]); preconditionFailure("Legacy service must not receive a record-removal request") } catch {}
        precondition(OverviewTransport.responses.recordedRemovals().count == removalCount)
        do { try await store.retryJob(store.jobs[0], on: store.nodes[0]); preconditionFailure("Legacy service should require an update for linked retry") } catch {}
        print("Advanced/legacy services, isolated failure, cached history, cancellation and service-switch checks passed")
    }
}
