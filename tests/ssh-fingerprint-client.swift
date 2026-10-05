import Foundation

private let oldFingerprint = "SHA256:" + Data(repeating: 1, count: 32).base64EncodedString().replacingOccurrences(of: "=", with: "")
private let newFingerprint = "SHA256:" + Data(repeating: 2, count: 32).base64EncodedString().replacingOccurrences(of: "=", with: "")

final class FingerprintTransport: URLProtocol, @unchecked Sendable {
    final class Responses: @unchecked Sendable {
        let lock = NSLock()
        var revision = 7
        var fingerprint = oldFingerprint
        var state = "fingerprint_changed"
        var conflict = false
        var retryID: String?
        var error = "SSH host key changed: expected \(oldFingerprint); observed \(newFingerprint)"
        var patches: [[String: Any]] = []
        var retries: [[String: Any]] = []

        func reset() {
            lock.lock(); defer { lock.unlock() }
            revision = 7; fingerprint = oldFingerprint; state = "fingerprint_changed"
            conflict = false; retryID = nil
            error = "SSH host key changed: expected \(oldFingerprint); observed \(newFingerprint)"
            patches = []; retries = []
        }
        func patchBodies() -> [[String: Any]] { lock.lock(); defer { lock.unlock() }; return patches }
        func retryBodies() -> [[String: Any]] { lock.lock(); defer { lock.unlock() }; return retries }
        func mutate(_ block: (Responses) -> Void) { lock.lock(); defer { lock.unlock() }; block(self) }

        func response(_ request: URLRequest) -> (Int, Data) {
            lock.lock(); defer { lock.unlock() }
            let path = request.url!.path
            var node: [String: Any] = [
                "id": "node-1", "name": "Fingerprint test", "revision": revision, "state": state,
                "ssh": ["host": "node.invalid", "port": 22, "username": "root", "auth_type": "private_key",
                        "host_fingerprint": fingerprint, "credential_id": "ssh-1", "credential_version": 1],
                "config": [:], "yaml_preview": "", "public": ["host": "node.invalid", "port": 443, "listen_addr": ":443", "skip_cert_verify": false],
                "created_at": "2026-10-05T00:00:00Z", "updated_at": "2026-10-05T00:00:00Z"
            ]
            var job: [String: Any] = ["id": "job-1", "node_id": "node-1", "kind": "ssh-test", "status": "failed",
                                      "stage": "failed", "attempts": 1, "error_message": error,
                                      "created_at": "2026-10-05T00:00:00Z", "updated_at": "2026-10-05T00:00:00Z"]
            if let retryID { job["retry_job_id"] = retryID }
            func json(_ value: Any, _ status: Int = 200) -> (Int, Data) { (status, try! JSONSerialization.data(withJSONObject: value)) }
            if path.hasSuffix("version") {
                return json(["api_version": "1.0.0", "service_version": "test", "hysteria_version": "test", "mihomo_version": "test", "features": ["job_retry_links"]])
            }
            if path.hasSuffix("server/monitoring") { return (503, Data()) }
            if path.hasSuffix("/nodes/node-1"), request.httpMethod == "PATCH" {
                let body = try! JSONSerialization.jsonObject(with: Self.body(request)) as! [String: Any]
                patches.append(body)
                if conflict || body["expected_revision"] as? Int != revision {
                    return json(["error": ["code": "conflict", "message": "node changed while saving"]], 409)
                }
                fingerprint = body["ssh_host_fingerprint"] as! String
                revision += 1; state = "ready"
                node["revision"] = revision
                return json(["id": "node-1", "name": "Fingerprint test", "state": "fingerprint_changed", "revision": revision, "sync_job_queued": false])
            }
            if path.hasSuffix("/retry") {
                retries.append(try! JSONSerialization.jsonObject(with: Self.body(request)) as! [String: Any])
                retryID = "retry-1"
                return json(["job_id": "retry-1", "status": "queued"], 202)
            }
            if path.hasSuffix("/nodes/node-1") { return json(node) }
            if path.hasSuffix("/jobs/job-1") { return json(["job": job, "logs": []]) }
            if path.hasSuffix("/nodes") { return json([node]) }
            if path.hasSuffix("/jobs") { return json([job]) }
            return json([])
        }

        private static func body(_ request: URLRequest) -> Data {
            if let body = request.httpBody { return body }
            guard let stream = request.httpBodyStream else { return Data() }
            stream.open(); defer { stream.close() }
            var body = Data()
            var buffer = [UInt8](repeating: 0, count: 4096)
            while stream.hasBytesAvailable {
                let count = stream.read(&buffer, maxLength: buffer.count)
                if count <= 0 { break }
                body.append(contentsOf: buffer.prefix(count))
            }
            return body
        }
    }

    static let responses = Responses()
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        let (status, data) = Self.responses.response(request)
        let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: "HTTP/1.1", headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
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
struct SSHFingerprintClientChecks {
    @MainActor static func main() async throws {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [FingerprintTransport.self]
        let session = URLSession(configuration: configuration)
        let client = APIClient(baseURL: URL(string: "https://fingerprint-fixture.invalid")!, token: "test", session: session)
        let defaults = UserDefaults(suiteName: "com.hysteriax.fingerprint-tests")!
        let store = ManagementStore(client: client, restoreSnapshot: false, nodeNotifications: NodeAlertNotifications(transport: SilentNotifications(), defaults: defaults))
        store.serviceAddress = "https://fingerprint-fixture.invalid"
        defer {
            UserDefaults.standard.removeObject(forKey: "displaySnapshot.\(store.serviceAddress)")
            defaults.removePersistentDomain(forName: "com.hysteriax.fingerprint-tests")
        }
        await store.refresh()
        precondition(store.isConnected)
        let job = store.jobs[0]
        let change = SSHHostFingerprintChange(job: job)!
        precondition(change.expected == oldFingerprint && change.observed == newFingerprint)

        // Only an intact mismatch from a failed task may offer confirmation.
        for error in ["SSH host key changed; credential was not applied", "SSH host key changed: expected SHA256:short; observed SHA256:short", "SSH host key changed: expected \(oldFingerprint); observed \(oldFingerprint)"] {
            FingerprintTransport.responses.mutate { $0.error = error }
            let detail = try await store.jobDetail("job-1")
            precondition(SSHHostFingerprintChange(job: detail.job) == nil)
        }
        FingerprintTransport.responses.reset()

        // Use the fresh node revision, then retry against the revision after saving.
        FingerprintTransport.responses.mutate { $0.revision = 9 }
        try await store.confirmChangedHostFingerprint(job)
        let patch = FingerprintTransport.responses.patchBodies().last!
        precondition(patch["expected_revision"] as? Int == 9)
        precondition(patch["ssh_host_fingerprint"] as? String == newFingerprint && patch.count == 2)
        precondition(store.nodes[0].state == "ready" && store.nodes[0].ssh?.hostFingerprint == newFingerprint)
        precondition(FingerprintTransport.responses.retryBodies().isEmpty, "Saving must leave retry to the user")
        try await store.retryJob(job, on: store.nodes[0])
        precondition(FingerprintTransport.responses.retryBodies().last?["expected_revision"] as? Int == 10)
        precondition(store.jobs[0].retryJobId == "retry-1")

        // Changes since the task was displayed must not overwrite a newer fingerprint.
        for scenario in ["fingerprint", "state", "retry", "error"] {
            FingerprintTransport.responses.reset()
            FingerprintTransport.responses.mutate {
                switch scenario {
                case "fingerprint": $0.fingerprint = newFingerprint
                case "state": $0.state = "ready"
                case "retry": $0.retryID = "retry-1"
                default: $0.error = "SSH transport unavailable"
                }
            }
            do { try await store.confirmChangedHostFingerprint(job); preconditionFailure("Stale \(scenario) must be rejected") } catch {}
            precondition(FingerprintTransport.responses.patchBodies().isEmpty)
        }
        FingerprintTransport.responses.reset()
        FingerprintTransport.responses.mutate { $0.conflict = true }
        do { try await store.confirmChangedHostFingerprint(job); preconditionFailure("Revision conflict must be surfaced") } catch {}
        let detail = try await store.nodeDetail("node-1")
        precondition(detail.ssh.hostFingerprint == oldFingerprint && detail.state == "fingerprint_changed")
        store.isConnected = false
        let count = FingerprintTransport.responses.patchBodies().count
        do { try await store.confirmChangedHostFingerprint(job); preconditionFailure("Offline writes must be rejected") } catch {}
        precondition(FingerprintTransport.responses.patchBodies().count == count)
        print("SSH fingerprint parsing, confirmation, fresh revisions, linked retry, stale-state rejection, conflicts and offline checks passed")
    }
}
