import SwiftUI

struct CATrustCleanupView: View {
    let entries: [CATrustCleanup]
    var onJump: (String, String) -> Void

    var body: some View {
        if !entries.isEmpty {
            DisclosureGroup(L10n.text("旧信任清理")) {
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(entries, id: \.version) { entry in
                        VStack(alignment: .leading, spacing: 6) {
                            HStack {
                                Text("v\(entry.version)").font(.headline)
                                Spacer()
                                Text(entry.completedAt == nil ? L10n.text("等待旧信任清理") : L10n.text("已完成"))
                                    .font(.caption).foregroundStyle(.secondary)
                            }
                            LabeledContent(L10n.text("最早清理时间"), value: DateDisplayText.local(entry.nextActionAt))
                            if let completed = entry.completedAt {
                                LabeledContent(L10n.text("完成时间"), value: DateDisplayText.local(completed))
                            }
                            ForEach(entry.nodes, id: \.nodeID) { node in
                                VStack(alignment: .leading, spacing: 4) {
                                    HStack {
                                        Button(node.name ?? node.nodeID) { onJump("node", node.nodeID) }.buttonStyle(.link)
                                        Spacer()
                                        Text(node.completed ? L10n.text("已完成") : node.status.map(JobDisplayText.status) ?? L10n.text("待执行"))
                                            .font(.caption)
                                        if let jobID = node.jobId {
                                            Button(L10n.text("查看任务")) { onJump("job", jobID) }.buttonStyle(.link)
                                        }
                                    }
                                    if let error = node.errorMessage { Text(error).font(.caption).foregroundStyle(.red).textSelection(.enabled) }
                                }
                            }
                            if entry.version != entries.last?.version { Divider() }
                        }
                    }
                    Text(L10n.text("旧客户端及探测证书全部到期后加 5 分钟才可移除旧信任；该任务不阻止后续 CA 发布。"))
                        .font(.caption).foregroundStyle(.secondary)
                }.padding(.top, 8)
            }.accessibilityIdentifier("ca.cleanup")
        }
    }
}
