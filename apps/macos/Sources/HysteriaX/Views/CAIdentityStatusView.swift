import SwiftUI

struct CAIdentityStatusView: View {
    let detail: CredentialDetail
    var onJump: (String, String) -> Void = { _, _ in }
    var body: some View {
        if let ca = detail.ca {
            GroupBox(L10n.text("CA 身份")) {
                VStack(alignment: .leading, spacing: 8) {
                    LabeledContent(L10n.text("CA 来源"), value: ca.source == "generated" ? L10n.text("系统生成") : L10n.text("导入证书链与私钥"))
                    LabeledContent(L10n.text("当前发布版本"), value: "v\(ca.publishedVersion)")
                    LabeledContent(L10n.text("客户端证书策略"), value: L10n.text("{0} 天有效，提前 {1} 小时续期", String(ca.clientValidityDays), String(ca.renewBeforeHours)))
                    if let version = detail.versions.first(where: { $0.version == ca.publishedVersion }), let expiry = version.metadata["expires_at"]?.stringValue {
                        LabeledContent(L10n.text("CA 到期"), value: DateDisplayText.local(expiry))
                    }
                    if detail.latestVersion != ca.publishedVersion {
                        LabeledContent(L10n.text("准备中版本"), value: "v\(detail.latestVersion)")
                        if let version = detail.versions.first(where: { $0.version == detail.latestVersion }), let expiry = version.metadata["expires_at"]?.stringValue {
                            LabeledContent(L10n.text("准备中 CA 到期"), value: DateDisplayText.local(expiry))
                        }
                    }
                    CATrustCleanupView(entries: ca.cleanup ?? [], onJump: onJump)
                    if let error = ca.maintenanceError { Text(error).foregroundStyle(.red).textSelection(.enabled) }
                }.frame(maxWidth: .infinity, alignment: .leading).padding(.vertical, 4)
            }.accessibilityIdentifier("ca.status")
        }
    }
}

struct CARotationBatchView: View {
    let batch: CredentialBatch
    let rotation: CARotationState
    let isConnected: Bool
    let archived: Bool
    var onJump: (String, String) -> Void
    var onRetry: (String) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            LabeledContent(L10n.text("更新阶段"), value: phaseTitle)
            LabeledContent(L10n.text("共享证书准备"), value: "\(rotation.status.preparedUserCount) / \(rotation.status.eligibleUserCount)")
            if rotation.phase != "complete" {
                LabeledContent(L10n.text("旧材料可用期限"), value: DateDisplayText.local(rotation.status.deadline))
                Text(L10n.text("剩余 {0} 天；旧材料过期后可能无法连接。", String(rotation.status.daysRemaining)))
                    .font(.caption).foregroundStyle(rotation.status.daysRemaining < 7 ? Color.red : Color.secondary)
            }
            if let outcome = rotation.outcome {
                Text(outcome == "superseded_expired_publication" ? L10n.text("已到期的已发布目标被后续批次替换。") : L10n.text("未发布目标被后续批次替换。"))
                    .font(.caption).foregroundStyle(.secondary)
            }
            if let error = rotation.errorMessage { Text(error).font(.caption).foregroundStyle(.red).textSelection(.enabled) }
            ForEach(rotation.nodes, id: \.nodeID) { node in
                VStack(alignment: .leading, spacing: 4) {
                    HStack {
                        Button(node.name ?? node.nodeID) { onJump("node", node.nodeID) }.buttonStyle(.link)
                        Spacer()
                        Text(node.trustPrepared ? L10n.text("信任已验证") : L10n.text("等待信任准备"))
                            .font(.caption).foregroundStyle(node.trustPrepared ? Color.secondary : Color.orange)
                        if let job = node.jobId {
                            Button(L10n.text("查看任务")) { onJump("job", job) }.buttonStyle(.link)
                        }
                    }
                    if let status = node.status {
                        Text(JobDisplayText.status(status) + (node.stage.map { " · " + JobDisplayText.stage($0) } ?? ""))
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    if let error = node.errorMessage { Text(error).font(.caption).foregroundStyle(.red) }
                }
            }
            if rotation.phase != "complete" && rotation.errorMessage != nil {
                Button(L10n.text("重试失败项")) { onRetry(batch.id) }
                    .disabled(!isConnected || archived).accessibilityIdentifier("ca.rotation.retry")
            }
        }.accessibilityIdentifier("ca.rotation.\(rotation.id)")
    }
    private var phaseTitle: String {
        switch rotation.phase {
        case "trust": L10n.text("准备节点信任")
        case "identities": L10n.text("准备共享证书")
        case "publish": L10n.text("发布共享证书")
        case "probes": L10n.text("切换探测身份")
        case "complete": L10n.text("已完成")
        default: L10n.text("维护异常")
        }
    }
}
