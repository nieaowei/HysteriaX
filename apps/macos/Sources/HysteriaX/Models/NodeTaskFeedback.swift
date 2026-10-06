import Foundation

/// Select actual node tasks independently from the node's deployment state.
enum NodeTaskFeedback {
    static func isActive(_ job: JobSummary) -> Bool {
        ["queued", "running"].contains(job.status)
    }

    static func blocksOperation(_ job: JobSummary) -> Bool {
        isActive(job) && ["ssh-test", "deploy", "sync", "rollback", "uninstall"].contains(job.kind)
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
        case "running": 2
        case "queued": 1
        default: 0
        }
    }
}
