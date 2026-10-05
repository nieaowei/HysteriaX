import Foundation

final class OverviewTransport: URLProtocol, @unchecked Sendable {
    final class Responses: @unchecked Sendable {
        let lock = NSLock()
        var mode = "advanced"
        var overview = Data()
        var nodes = Data()
        var history = Data()
        var jobs = Data("[]".utf8)
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
            if path.hasSuffix("jobs") { return (200, jobs) }
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
