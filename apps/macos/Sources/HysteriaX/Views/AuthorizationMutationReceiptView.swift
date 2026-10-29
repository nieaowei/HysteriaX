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
