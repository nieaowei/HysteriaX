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
        var kind = "ssh-test"
        var testStatus = "failed"
        var confirmations: [[String: Any]] = []
        var patches: [[String: Any]] = []
        var retries: [[String: Any]] = []
        var rechecks: [(String, [String: Any])] = []
        var executionFilters: [String: String] = [:]
        var verificationComplete = false
        var wrongVerificationResult = false


        func reset() {
            lock.lock(); defer { lock.unlock() }
            revision = 7; fingerprint = oldFingerprint; state = "fingerprint_changed"
            conflict = false; retryID = nil
            error = "SSH host key changed: expected \(oldFingerprint); observed \(newFingerprint)"
            patches = []; retries = []; confirmations = []; testStatus = "failed"; kind = "ssh-test"
            rechecks = []; executionFilters = [:]; verificationComplete = false; wrongVerificationResult = false

        }
        func confirmationBodies() -> [[String: Any]] { lock.lock(); defer { lock.unlock() }; return confirmations }
        func patchBodies() -> [[String: Any]] { lock.lock(); defer { lock.unlock() }; return patches }
        func retryBodies() -> [[String: Any]] { lock.lock(); defer { lock.unlock() }; return retries }
        func verificationRequests() -> [(String, [String: Any])] { lock.lock(); defer { lock.unlock() }; return rechecks }

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
            var job: [String: Any] = ["id": "job-1", "node_id": "node-1", "kind": kind, "status": testStatus,
                                      "stage": "failed", "attempts": 1, "error_message": error,
                                      "created_at": "2026-10-05T00:00:00Z", "updated_at": "2026-10-05T00:00:00Z"]
            if testStatus == "waiting_confirmation" {
                job["stage"] = "fingerprint_confirmation_required"
                job["result"] = ["stage": "fingerprint_confirmation_required", "result": ["status": "needs_confirmation", "fingerprint": newFingerprint, "expected_fingerprint": oldFingerprint]]
            }
            if let retryID { job["retry_job_id"] = retryID }
            func json(_ value: Any, _ status: Int = 200) -> (Int, Data) { (status, try! JSONSerialization.data(withJSONObject: value)) }
            if path.hasSuffix("version") {
                return json(["api_version": "2.0.0", "management_protocol":2, "directory_version":"b3:test", "service_version": "test", "hysteria_version": "test", "mihomo_version": "test", "features": ["job_retry_links"]])
            }
            if path.hasSuffix("/execution-operations") {
                executionFilters = Dictionary(uniqueKeysWithValues: URLComponents(url: request.url!, resolvingAgainstBaseURL: false)!.queryItems!.map { ($0.name, $0.value!) })
                return json(["items": [["operation_id":"operation-1", "resource_key":"node:node-1", "work_id":"job-1", "generation":5, "epoch":"ec829402-b930-4c88-a949-bd9450eb3b38", "step":"ssh-command:0", "state":"needs_attention", "kind":"ssh-command", "protocol":"ssh-journal-v2"]], "next_cursor":NSNull()])
            }
            if path.hasSuffix("/execution-operations/operation-1/reverify") {
                let key = request.value(forHTTPHeaderField: "Idempotency-Key") ?? ""
                rechecks.append((key, try! JSONSerialization.jsonObject(with: Self.body(request)) as! [String: Any]))
                if conflict { return json(["error":["code":"revision_conflict", "message":"execution changed"]],409) }
                return json(["operation_id":wrongVerificationResult ? "other-operation" : "operation-1", "recheck_id":"check-1", "state":verificationComplete ? "verified" : "needs_attention", "resource_blocked":!verificationComplete, "retry_allowed":verificationComplete, "reason":"fixture"])
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
            if path.hasSuffix("/confirm-fingerprint") {
                let body = try! JSONSerialization.jsonObject(with: Self.body(request)) as! [String: Any]
                confirmations.append(body)
                if conflict { return json(["error": ["code": "conflict", "message": "node changed while saving"]], 409) }
                testStatus = "queued"; fingerprint = newFingerprint; revision += 1
                return json(["job_id": "job-1", "status": "queued"], 202)
            }
            if path.hasSuffix("/retry") {
                retries.append(try! JSONSerialization.jsonObject(with: Self.body(request)) as! [String: Any])
                retryID = "retry-1"
                return json(["job_id": "retry-1", "status": "queued"], 202)
            }
            if path.hasSuffix("/nodes/node-1") { return json(node) }
            if path.hasSuffix("/jobs/job-1") { return json(["job": job, "logs": []]) }
            if path.hasSuffix("/nodes") { return json(["items": [node], "total": 1, "page": 1, "page_size": 200]) }
            if path.hasSuffix("/jobs") { return json(["items": [job], "total": 1, "page": 1, "page_size": 200]) }
            if path.hasSuffix("/audit") { return json(["items": [], "total": 0, "page": 1, "page_size": 200]) }
            if ["/api/v2/users", "/api/v2/credentials", "/api/v2/dns/records"].contains(path) { return json(["items": [], "total": 0, "page": 1, "page_size": 200]) }
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
        // The suspended SSH test uses the dedicated confirmation operation and
        // preserves its ID. It never creates a retry or performs a node PATCH.
        FingerprintTransport.responses.reset()
        FingerprintTransport.responses.mutate { $0.testStatus = "waiting_confirmation" }
        await store.refresh()
        let waiting = store.jobs[0]
        precondition(NodeTaskFeedback.isActive(waiting))
        precondition(!NodeTaskFeedback.blocksOperation(waiting))
        precondition(JobDisplayText.status(waiting.status) != waiting.status)
        precondition(waiting.result?.result?.expectedFingerprint == oldFingerprint)
        try await store.confirmJobFingerprint(waiting, fingerprint: newFingerprint)
        let confirmation = FingerprintTransport.responses.confirmationBodies().last!
        precondition(confirmation["expected_revision"] as? Int == 7)
        precondition(confirmation["fingerprint"] as? String == newFingerprint && confirmation.count == 2)
        precondition(store.jobs[0].id == waiting.id && store.jobs[0].status == "queued")
        precondition(FingerprintTransport.responses.patchBodies().isEmpty)
        precondition(FingerprintTransport.responses.retryBodies().isEmpty)
        FingerprintTransport.responses.mutate { $0.conflict = true }
        do { try await store.confirmJobFingerprint(waiting, fingerprint: newFingerprint); preconditionFailure("Confirmation conflicts must be surfaced") } catch {}
        for kind in ["deploy", "sync", "rollback"] {
            FingerprintTransport.responses.reset()
            FingerprintTransport.responses.mutate { $0.kind = kind; $0.testStatus = "waiting_confirmation" }
            await store.refresh()
            let operation = store.jobs[0]
            try await store.confirmJobFingerprint(operation, fingerprint: newFingerprint)
            precondition(store.jobs[0].id == operation.id && store.jobs[0].kind == kind && store.jobs[0].status == "queued")
            precondition(FingerprintTransport.responses.confirmationBodies().count == 1)
            precondition(FingerprintTransport.responses.patchBodies().isEmpty && FingerprintTransport.responses.retryBodies().isEmpty)
        }
        FingerprintTransport.responses.reset()
        let operations = try await store.executionOperations(workID: "job-1", after: "prior cursor")
        let operation = operations.items[0]
        precondition(operation.verificationProtocol == "ssh-journal-v2")
        precondition(FingerprintTransport.responses.executionFilters == ["work_id":"job-1", "after":"prior cursor"])
        let unresolved = try await store.reverifyExecution(operation)
        precondition(unresolved.state == "needs_attention" && unresolved.resourceBlocked && !unresolved.retryAllowed)
        precondition(FingerprintTransport.responses.retryBodies().isEmpty, "Verification never retries the job")
        FingerprintTransport.responses.mutate { $0.verificationComplete = true }
        let verified = try await store.reverifyExecution(operation)
        precondition(verified.state == "verified" && !verified.resourceBlocked && verified.retryAllowed)
        let requests = FingerprintTransport.responses.verificationRequests()
        precondition(requests.count == 2 && !requests[0].0.isEmpty && requests[0].0 != requests[1].0)
        precondition(requests.allSatisfy { $0.1.count == 2 && $0.1["expected_generation"] as? Int == 5 && $0.1["expected_epoch"] as? String == operation.epoch })
        precondition(store.jobs[0].status == "failed" && FingerprintTransport.responses.retryBodies().isEmpty)
        FingerprintTransport.responses.mutate { $0.wrongVerificationResult = true }
        do { _ = try await store.reverifyExecution(operation); preconditionFailure("Wrong-operation proof must be rejected") } catch {}
        FingerprintTransport.responses.mutate { $0.wrongVerificationResult = false; $0.conflict = true }
        do { _ = try await store.reverifyExecution(operation); preconditionFailure("Verification conflicts must be surfaced") } catch {}
        store.isConnected = false
        let count = FingerprintTransport.responses.patchBodies().count
        do { try await store.confirmChangedHostFingerprint(job); preconditionFailure("Offline writes must be rejected") } catch {}
        precondition(FingerprintTransport.responses.patchBodies().count == count)
        do { try await store.confirmJobFingerprint(waiting, fingerprint: newFingerprint); preconditionFailure("Offline continuation must be rejected") } catch {}
        let verificationCount = FingerprintTransport.responses.verificationRequests().count
        do { _ = try await store.reverifyExecution(operation); preconditionFailure("Offline verification must be rejected") } catch {}
        precondition(FingerprintTransport.responses.verificationRequests().count == verificationCount)
        print("SSH test continuation on the original task, SSH fingerprint parsing, confirmation, fresh revisions, linked retry, stale-state rejection, conflicts and offline checks passed")
    }
}
