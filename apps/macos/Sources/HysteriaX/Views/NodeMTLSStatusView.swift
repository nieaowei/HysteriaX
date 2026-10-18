import SwiftUI

struct NodeMTLSStatusView: View {
    let detail: NodeDetail
    var body: some View {
        if let acme = detail.acmeStatus { NodeACMEStatusView(status: acme) }
        GroupBox(L10n.text("mTLS 维护状态")) {
            VStack(alignment: .leading, spacing: 8) {
                LabeledContent(L10n.text("状态"), value: stateTitle)
                if let mtls = detail.mtls {
                    setting(L10n.text("目标配置"), value: mtls.target)
                    if let deployed = mtls.deployed { setting(L10n.text("已部署配置"), value: deployed) }
                    if !mtls.trust.isEmpty {
                        DisclosureGroup(L10n.text("信任代次")) {
                            ForEach(Array(mtls.trust.enumerated()), id: \.offset) { _, trust in
                                Text("\(trust.caCredentialId) · v\(trust.caCredentialVersion) · \(trust.deployed ? L10n.text("已部署配置") : trust.desired ? L10n.text("目标配置") : L10n.text("历史配置"))")
                                    .font(.caption.monospaced()).textSelection(.enabled)
                            }
                        }
                    }
                }
                if let error = detail.mtlsError { Text(error).font(.callout).foregroundStyle(.red).textSelection(.enabled) }
                if detail.mtlsState == "needs_configuration" {
                    Text(L10n.text("显式选择关闭或有效托管 CA，保存新配置并完成恢复部署。"))
                        .font(.caption).foregroundStyle(.secondary)
                } else if detail.mtlsState == "activating" || detail.mtlsState == "recovery_required" {
                    Text(L10n.text("该节点暂不发布用户订阅或接受用户认证。请查看部署任务并执行显式恢复同步。"))
                        .font(.caption).foregroundStyle(.secondary)
                }
            }.frame(maxWidth: .infinity, alignment: .leading).padding(.vertical, 4)
        }.accessibilityIdentifier("node.mtls.status")
    }
    private var stateTitle: String {
        switch detail.mtlsState ?? "ready" {
        case "ready": L10n.text("就绪")
        case "needs_configuration": L10n.text("待配置")
        case "activating": L10n.text("激活中")
        default: L10n.text("需要恢复")
        }
    }
    private func setting(_ label: String, value: NodeMTLSSettings) -> some View {
        LabeledContent(label, value: value.mode == "managed" ? "\(value.caCredentialId ?? "—") · v\(value.caCredentialVersion ?? 0)" : L10n.text("关闭"))
    }
}

struct NodeACMEStatusView: View {
    let status: NodeACMEStatus
    var body: some View {
        GroupBox(L10n.text("节点 ACME 证书")) {
            VStack(alignment: .leading, spacing: 8) {
                LabeledContent(L10n.text("状态"), value: status.displayState)
                LabeledContent(L10n.text("域名"), value: status.domains.joined(separator: ", "))
                if let expiry = status.notAfter { LabeledContent(L10n.text("证书到期"), value: DateDisplayText.local(expiry)) }
                if let success = status.lastSuccess { LabeledContent(L10n.text("最近签发成功"), value: DateDisplayText.local(success)) }
                if let next = status.nextAttempt { LabeledContent(L10n.text("下次尝试"), value: DateDisplayText.local(next)) }
                LabeledContent(L10n.text("组件版本"), value: status.componentVersion)
                LabeledContent(L10n.text("最后观测时间"), value: DateDisplayText.local(status.collectedAt))
                if let fingerprint = status.fingerprint {
                    LabeledContent(L10n.text("证书指纹"), value: fingerprint).font(.caption.monospaced()).textSelection(.enabled)
                }
                if let error = status.error, !error.isEmpty { Text(error).foregroundStyle(.red).textSelection(.enabled) }
                Text(L10n.text("管理服务离线时节点继续维护证书；此处显示最后一次观测结果。"))
                    .font(.caption).foregroundStyle(.secondary)
            }.frame(maxWidth: .infinity, alignment: .leading).padding(.vertical, 4)
        }.accessibilityIdentifier("node.acme.status")
    }
}
