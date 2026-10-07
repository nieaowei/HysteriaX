import AppKit
import SwiftUI

struct AuthorizationMutationReceiptView: View {
    @Bindable var store: ManagementStore
    let createdCredentials: [AuthorizationCreatedCredential]
    let revocationJobIDs: [String]

    @State private var message: String?

    private var matchingJobs: [JobSummary] {
        store.jobs.filter { revocationJobIDs.contains($0.id) }
    }

    var body: some View {
        GroupBox {
            LazyVStack(alignment: .leading, spacing: 12) {
                Label(L10n.text("授权已更新"), systemImage: "checkmark.circle.fill")
                    .font(.callout.weight(.semibold))
                    .foregroundStyle(.green)

                if !revocationJobIDs.isEmpty {
                    Label(revocationStatus, systemImage: matchingJobs.contains(where: { $0.status == "failed" }) ? "exclamationmark.triangle.fill" : "hourglass")
                        .font(.callout)
                        .foregroundStyle(matchingJobs.contains(where: { $0.status == "failed" }) ? Color.orange : Color.secondary)
                    if matchingJobs.contains(where: { $0.status == "failed" }) {
                        Text(L10n.text("请在任务页检查失败的撤权任务并重试。"))
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    DisclosureGroup(L10n.text("撤权任务 ID（{0}）", String(revocationJobIDs.count))) {
                        LazyVStack(alignment: .leading, spacing: 6) {
                            ForEach(revocationJobIDs, id: \.self) { id in
                                let job = matchingJobs.first { $0.id == id }
                                HStack {
                                    Text(job?.kind ?? L10n.text("撤权任务"))
                                    Spacer()
                                    Text(job.map { JobDisplayText.status($0.status) } ?? L10n.text("等待读取"))
                                        .foregroundStyle(job?.status == "failed" ? Color.orange : Color.secondary)
                                }
                                Text(id).font(.caption.monospaced()).textSelection(.enabled)
                            }
                        }
                        .padding(.top, 6)
                    }
                    .font(.caption)
                } else {
                    Text(L10n.text("没有需要撤销的现有连接。"))
                        .font(.caption).foregroundStyle(.secondary)
                }

                if !createdCredentials.isEmpty {
                    Divider()
                    Text(L10n.text("新建连接密码只显示这一次。请逐项复制并安全保存。"))
                        .font(.callout).foregroundStyle(.secondary)
                    ForEach(Array(Set(createdCredentials.map(\.userId))).sorted(), id: \.self) { id in
                        HStack {
                            Text(userName(id))
                            Spacer()
                            Button(L10n.text("复制订阅")) {
                                Task {
                                    do {
                                        let user = try await store.authorizationUserDetail(id)
                                        let url = try await store.currentSubscriptionURL(for: user)
                                        NSPasteboard.general.clearContents()
                                        NSPasteboard.general.setString(url, forType: .string)
                                        message = L10n.text("已复制到剪贴板")
                                    } catch { message = error.localizedDescription }
                                }
                            }
                            .disabled(!store.isConnected)
                        }
                    }
                    if let message { Text(message).font(.caption).foregroundStyle(.secondary) }
                    DisclosureGroup(L10n.text("连接密码")) {
                        LazyVStack(alignment: .leading, spacing: 10) {
                            ForEach(Array(createdCredentials.enumerated()), id: \.offset) { _, item in
                                VStack(alignment: .leading, spacing: 4) {
                                    Text("\(userName(item.userId)) · \(nodeName(item.nodeID))")
                                        .font(.callout.weight(.medium))
                                    Text(item.hy2Credential)
                                        .font(.system(.caption, design: .monospaced))
                                        .textSelection(.enabled)
                                        .frame(maxWidth: .infinity, alignment: .leading)
                                        .padding(8)
                                        .background(.quaternary, in: RoundedRectangle(cornerRadius: 6))
                                }
                            }
                        }
                    }

                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, 4)
        }
        .accessibilityIdentifier("authorization.mutationReceipt")
    }

    private var revocationStatus: String {
        if matchingJobs.contains(where: { $0.status == "failed" }) {
            return L10n.text("部分撤权任务失败。用户权限已更新，请检查任务状态。")
        }
        if matchingJobs.count == revocationJobIDs.count && !matchingJobs.isEmpty && matchingJobs.allSatisfy({ $0.status == "succeeded" }) {
            return L10n.text("撤权任务已完成，在线连接已断开。")
        }
        if matchingJobs.contains(where: { $0.status == "running" }) {
            return L10n.text("撤权任务正在运行，在线连接将在执行后断开。")
        }
        return L10n.text("撤权任务已排队；在线连接将在任务执行后断开。")
    }

    private func userName(_ id: String) -> String {
        store.users.first(where: { $0.id == id })?.name ?? id
    }

    private func nodeName(_ id: String) -> String {
        store.nodes.first(where: { $0.id == id })?.name ?? id
    }
}
