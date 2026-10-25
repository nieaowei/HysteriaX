import Foundation

/// Select actual node tasks independently from the node's deployment state.
enum NodeTaskFeedback {
    static func isActive(_ job: JobSummary) -> Bool {
        ["queued", "running", "waiting_confirmation"].contains(job.status)
    }

    static func allowsAction(_ action: String, state: String, mtlsState: String? = nil, recoveryConfigured: Bool = false) -> Bool {
        guard state != "deleting", state != "delete_failed" || action == "ssh-test" else { return false }
        if action == "ssh-test" { return true }
        if action == "rollback", let mtlsState, mtlsState != "ready" { return false }
        return !["deploy", "sync", "rollback"].contains(action) || mtlsState != "needs_configuration" || recoveryConfigured
    }

    static func allowsAction(_ action: String, node: NodeSummary) -> Bool {
        allowsAction(action, state: node.state, mtlsState: node.mtlsState, recoveryConfigured: node.mtlsRecoveryConfigured == true)
    }

    static func blocksOperation(_ job: JobSummary) -> Bool {
        job.status != "waiting_confirmation" && isActive(job) && ["ssh-test", "deploy", "sync", "rollback", "uninstall", "record-remove"].contains(job.kind)
    }

    static func tasks(for nodeID: String, in jobs: [JobSummary]) -> [JobSummary] {
        jobs.filter { $0.nodeID == nodeID }.sorted { lhs, rhs in
            let leftPriority = priority(lhs)
            let rightPriority = priority(rhs)
            if leftPriority != rightPriority { return leftPriority > rightPriority }
            let left = DateDisplayParser.shared.parse(lhs.createdAt) ?? .distantPast
            let right = DateDisplayParser.shared.parse(rhs.createdAt) ?? .distantPast
            return left == right ? lhs.id > rhs.id : left > right
        }
    }

    private static func priority(_ job: JobSummary) -> Int {
        switch job.status {
        case "waiting_confirmation": 3
        case "running": 2
        case "queued": 1
        default: 0
        }
    }
}
