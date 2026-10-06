import Foundation

@main struct NodeTaskFeedbackChecks {
    static func job(_ id: String, node: String = "node-1", kind: String = "sync", status: String, created: String) -> JobSummary {
        JobSummary(resourceType: nil, resourceId: nil, resourceName: nil, id: id, kind: kind, nodeID: node, nodeName: nil,
                   retryOfJobId: nil, retryJobId: nil, targetRevision: 3, status: status, stage: "queued", errorMessage: nil,
                   result: nil, attempts: 0, createdAt: created, updatedAt: created, finishedAt: nil)
    }
    static func main() {
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
