import SwiftUI

struct JobExecutionRechecksView: View {
    var store: ManagementStore
    let jobID: String
    @State private var operations: [ExecutionOperation] = []
    @State private var cursor: String?
    @State private var nextCursor: String?
    @State private var isLoading = false
    @State private var verifyingID: String?
    @State private var error: String?
    @State private var result: ExecutionRecheckResult?
    @State private var loadID = UUID()

    private var requestKey: String {
        "\(store.serviceAddress):\(jobID):\(store.isConnected):\(store.dataRefreshToken)"
    }

    var body: some View {
        Group {
            if isLoading || error != nil || !operations.isEmpty || result != nil || cursor != nil {
                GroupBox {
                    VStack(alignment: .leading, spacing: 12) {
                        Text(L10n.text("核验会检查远端完成证明。无法确认时资源继续阻塞；核验不会重发原操作，也不会自动重试任务。"))
                            .font(.callout).foregroundStyle(.secondary)
                        if let result {
                            Label(resultText(result), systemImage: result.resourceBlocked ? "exclamationmark.triangle" : "checkmark.circle")
                                .foregroundStyle(result.resourceBlocked ? Color.orange : Color.secondary)
                                .accessibilityIdentifier("jobs.recheck.result")
                        }
                        if let error {
                            Text(error).foregroundStyle(.orange).textSelection(.enabled)
                        }
                        ForEach(operations, id: \.operationId) { operation in
                            HStack(alignment: .center, spacing: 12) {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(operation.kind == "ssh-command" ? L10n.text("SSH 命令") : L10n.text("外部操作"))
                                    Text(JobDisplayText.status(operation.state)).font(.caption).foregroundStyle(.secondary)
                                    Text(operation.operationId).font(.system(.caption, design: .monospaced)).foregroundStyle(.secondary).textSelection(.enabled)
                                }
                                Spacer(minLength: 8)
                                Button(L10n.text("重新核验")) { Task { await verify(operation) } }
                                    .disabled(!store.isConnected || verifyingID != nil || isLoading)
                                    .accessibilityIdentifier("jobs.recheck.\(operation.operationId)")
                                if verifyingID == operation.operationId { ProgressView().controlSize(.small) }
                            }
                        }
                        HStack {
                            Button(L10n.text("刷新核验状态")) { Task { await load(after: cursor) } }
                            if cursor != nil {
                                Button(L10n.text("首页")) { Task { await load(after: nil) } }
                            }
                            if let nextCursor {
                                Button(L10n.text("下一页")) { Task { await load(after: nextCursor) } }
                            }
                            if isLoading { ProgressView().controlSize(.small) }
                        }
                        .disabled(!store.isConnected || verifyingID != nil || isLoading)
                    }
                    .padding(.vertical, 4)
                    .frame(maxWidth: .infinity, alignment: .leading)
                } label: {
                    Label(L10n.text("远端操作核验"), systemImage: "checkmark.shield")
                }
                .accessibilityIdentifier("jobs.recheck.panel")
            }
        }
        .task(id: requestKey) {
            guard verifyingID == nil else { return }
            await load(after: cursor)
        }
    }

    private func resultText(_ result: ExecutionRecheckResult) -> String {
        if result.state != "verified" {
            return L10n.text("尚未取得完整证明。资源保持阻塞，请恢复连接或人工排查后重新核验。")
        }
        if result.resourceBlocked {
            return L10n.text("该步骤已核验；其他未决操作仍阻塞资源。")
        }
        return result.retryAllowed
            ? L10n.text("核验通过。原任务状态未变，可单独发起重试。")
            : L10n.text("该步骤已核验。请刷新后查看任务的可用操作。")
    }

    private func load(after: String?) async {
        guard store.isConnected else { operations = []; isLoading = false; return }
        let current = UUID()
        loadID = current
        isLoading = true
        error = nil
        defer { if loadID == current { isLoading = false } }
        do {
            let page = try await store.executionOperations(workID: jobID, after: after)
            guard loadID == current, !Task.isCancelled else { return }
            operations = page.items
            nextCursor = page.nextCursor
            cursor = after
        } catch {
            guard loadID == current, !isRequestCancellation(error), !Task.isCancelled else { return }
            self.error = error.localizedDescription
        }
    }

    private func verify(_ operation: ExecutionOperation) async {
        guard verifyingID == nil, store.isConnected else { return }
        verifyingID = operation.operationId
        error = nil
        defer { verifyingID = nil }
        do {
            result = try await store.reverifyExecution(operation)
            await load(after: cursor)
        } catch {
            guard !isRequestCancellation(error) else { return }
            self.error = error.localizedDescription
        }
    }
}
