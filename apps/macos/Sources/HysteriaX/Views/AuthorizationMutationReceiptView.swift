import SwiftUI

struct AuthorizationMutationReceiptView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ManagementStore
    let jobID: String
    @State private var activeJobID: String?
    @State private var job: JobSummary?
    @State private var errorMessage: String?
    @State private var refreshToken = UUID()
    @State private var isRetrying = false

    private var currentID: String { activeJobID ?? jobID }
    private var succeeded: Bool { job?.status == "succeeded" }
    private var failed: Bool { job?.status == "failed" }

    var body: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 12) {
                Label(succeeded ? L10n.text("授权关系已生效") : failed ? L10n.text("授权任务失败") : L10n.text("授权变更处理中"),
                      systemImage: succeeded ? "checkmark.circle.fill" : failed ? "exclamationmark.triangle.fill" : "hourglass")
                    .accessibilityIdentifier("authorization.task.status")
                    .foregroundStyle(succeeded ? Color.green : failed ? Color.orange : Color.primary)
                if let job {
                    Text(JobDisplayText.stage(job.stage)).font(.callout)
                    if let message = job.errorMessage { Text(JobDisplayText.errorMessage(message)).foregroundStyle(.orange).textSelection(.enabled) }
                }
                Text(L10n.text("所需证书和断连由后台继续处理；证书就绪后刷新订阅。"))
                    .font(.caption).foregroundStyle(.secondary)
                Text(currentID).font(.caption.monospaced()).textSelection(.enabled)
                if let errorMessage { Text(errorMessage).foregroundStyle(.orange).textSelection(.enabled) }
                HStack {
                    Button(L10n.text("查看任务")) { store.requestedJobID = currentID; dismiss() }
                    if failed, let job {
                        Button(L10n.text("重试授权任务")) {
                            isRetrying = true
                            Task {
                                defer { isRetrying = false }
                                do { activeJobID = try await store.retryAuthorizationJob(job); errorMessage = nil }
                                catch { errorMessage = error.localizedDescription }
                            }
                        }.disabled(isRetrying || !store.isConnected)
                    }
                    if errorMessage != nil { Button(L10n.text("刷新状态")) { refreshToken = UUID() } }
                }
            }.frame(maxWidth: .infinity, alignment: .leading)
        }
        .accessibilityIdentifier("authorization.mutationReceipt")
        .task(id: "\(store.serviceAddress)|\(store.isConnected)|\(currentID)|\(refreshToken)") { await observe() }
    }

    private func observe() async {
        let service = store.serviceAddress
        let id = currentID
        do {
            while !Task.isCancelled {
                let detail = try await store.jobDetail(id)
                guard !Task.isCancelled, store.serviceAddress == service else { return }
                job = detail.job
                errorMessage = nil
                if let retryID = detail.job.retryJobId { activeJobID = retryID; return }
                if detail.job.status == "succeeded" { store.authorizationTaskDidFinish(); return }
                if ["failed", "cancelled", "rolled_back"].contains(detail.job.status) { return }
                try await Task.sleep(for: .seconds(1))
            }
        } catch is CancellationError {} catch {
            guard !Task.isCancelled, store.serviceAddress == service else { return }
            errorMessage = error.localizedDescription
        }
    }
}

enum AuthorizationConflictHelper {
    static func isPendingError(_ error: Error) -> Bool {
        let message = error.localizedDescription.lowercased()
        return message.contains("authorization change is pending")
            || message.contains("related membership change is pending")
    }
}

struct AuthorizationPendingConflictActions: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ManagementStore
    let resourceID: String?
    @State private var blockingJob: JobSummary?
    @State private var resolved = false

    var body: some View {
        HStack(spacing: 8) {
            if let job = blockingJob {
                Button(L10n.text("查看挂起任务")) {
                    store.requestedJobID = job.id
                    dismiss()
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .accessibilityIdentifier("authorization.conflict.openJob")
            } else if resolved {
                Button(L10n.text("前往任务")) {
                    store.requestedSection = "jobs"
                    dismiss()
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .accessibilityIdentifier("authorization.conflict.openJobs")
            }
        }
        .task(id: resourceID) {
            resolved = false
            blockingJob = nil
            if let resourceID {
                blockingJob = await store.pendingAuthorizationJob(forResourceID: resourceID)
            }
            resolved = true
        }
    }
}
