import Foundation

@main struct NodeTaskFeedbackChecks {
    static func job(_ id: String, node: String = "node-1", kind: String = "sync", status: String, created: String) -> JobSummary {
        JobSummary(resourceType: nil, resourceId: nil, resourceName: nil, id: id, kind: kind, nodeID: node, nodeName: nil,
                   retryOfJobId: nil, retryJobId: nil, targetRevision: 3, status: status, stage: "queued", errorMessage: nil,
                   result: nil, attempts: 0, createdAt: created, updatedAt: created, finishedAt: nil)
    }
    static func main() throws {
        // Startup reads the whole jobs page. Credential tasks return a numeric
        // version while SSH tests return an OS version string in the same field.
        let decoder = JSONDecoder()
        for (versionField, expected): (String, JSONValue?) in [
            (#""version":2,"#, .integer(2)),
            (#""version":"Ubuntu 24.04", "#, .string("Ubuntu 24.04")),
            (#""version":null,"#, nil),
            ("", nil),
        ] {
            let payload = Data("""
            {"items":[{"id":"credential-job","kind":"credential-apply",
              "status":"succeeded","stage":"completed","attempts":1,
              "created_at":"2026-10-21T00:00:00Z","updated_at":"2026-10-21T00:00:00Z",
              "result":{"stage":"completed","result":{\(versionField)
                "fingerprint":"SHA256:fixture","proxy_probe":{"status":"passed"}}}}],
             "page":1,"page_size":200,"total":1}
            """.utf8)
            let page = try decoder.decode(JobsPage.self, from: payload)
            precondition(page.items.count == 1)
            precondition(page.items[0].result?.result?.version == expected)
            precondition(page.items[0].result?.result?.fingerprint == "SHA256:fixture")
            precondition(page.items[0].result?.result?.proxyProbe?["status"] == .string("passed"))
            let cached = try decoder.decode(JobsPage.self, from: JSONEncoder().encode(page))
            precondition(cached.items[0].result?.result?.version == expected)
            // Use the same job payload in the detail envelope as returned by getJob.
            let envelope = try JSONSerialization.jsonObject(with: payload) as! [String: Any]
            let detailData = try JSONSerialization.data(withJSONObject: [
                "job": (envelope["items"] as! [Any])[0], "logs": []
            ])
            let decodedDetail = try decoder.decode(JobDetailResponse.self, from: detailData)
            precondition(decodedDetail.job.result?.result?.version == expected)
        }
        precondition(NodeTaskFeedback.allowsAction("ssh-test", state: "delete_failed"))
        precondition(!NodeTaskFeedback.allowsAction("rollback", state: "deployed", mtlsState: "recovery_required"))
        precondition(!NodeTaskFeedback.allowsAction("deploy", state: "deployed", mtlsState: "needs_configuration"))
        precondition(!NodeTaskFeedback.allowsAction("sync", state: "deployed", mtlsState: "needs_configuration"))
        precondition(NodeTaskFeedback.allowsAction("ssh-test", state: "deployed", mtlsState: "needs_configuration"))
        precondition(NodeTaskFeedback.allowsAction("kick", state: "deployed", mtlsState: "needs_configuration"))
        precondition(NodeTaskFeedback.allowsAction("sync", state: "deployed", mtlsState: "recovery_required"))
        precondition(NodeTaskFeedback.allowsAction("sync", state: "deployed", mtlsState: "needs_configuration", recoveryConfigured: true))
        precondition(!NodeTaskFeedback.allowsAction("rollback", state: "deployed", mtlsState: "needs_configuration", recoveryConfigured: true))
        for action in ["ssh-test", "deploy", "sync", "rollback"] {
            precondition(!NodeTaskFeedback.allowsAction(action, state: "deleting"))
            if action != "ssh-test" { precondition(!NodeTaskFeedback.allowsAction(action, state: "delete_failed")) }
        }
        let running = job("running", status: "running", created: "2026-10-07T01:00:00Z")
        let queued = job("queued", status: "queued", created: "2026-10-07T02:00:00Z")
        let succeeded = job("success", status: "succeeded", created: "2026-10-07T03:00:00Z")
        let failed = job("failed", status: "failed", created: "2026-10-07T00:00:00Z")
        let unrelated = job("other-node", node: "node-2", status: "running", created: "2026-10-07T04:00:00Z")
        let tasks = NodeTaskFeedback.tasks(for: "node-1", in: [failed, succeeded, unrelated, running, queued])
        precondition(tasks.map(\.id) == ["running", "queued", "success", "failed"], "A newly queued task must not hide the running task; isolate nodes")
        precondition(NodeTaskFeedback.tasks(for: "missing", in: tasks).isEmpty)
        precondition(NodeTaskFeedback.blocksOperation(running) && NodeTaskFeedback.blocksOperation(queued))
        precondition(!NodeTaskFeedback.blocksOperation(succeeded) && !NodeTaskFeedback.blocksOperation(failed))
        precondition(!NodeTaskFeedback.blocksOperation(job("kick", kind: "kick", status: "running", created: "2026-10-07T01:00:00Z")))
        for kind in ["ssh-test", "deploy", "sync", "rollback", "uninstall"] {
            precondition(NodeTaskFeedback.blocksOperation(job(kind, kind: kind, status: "running", created: "2026-10-07T01:00:00Z")))
            for status in ["succeeded", "failed", "cancelled", "rolled_back"] {
                precondition(!NodeTaskFeedback.blocksOperation(job(kind, kind: kind, status: status, created: "2026-10-07T01:00:00Z")))
            }
        }
        let fractional = job("fractional", status: "succeeded", created: "2026-10-07T03:00:00.100Z")
        precondition(NodeTaskFeedback.tasks(for: "node-1", in: [succeeded, fractional]).first?.id == "fractional", "Compare parsed timestamps")
        print("Node task selection, isolation, operation guards and timestamp ordering passed.")
    }
}
