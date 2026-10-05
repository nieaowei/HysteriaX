import SwiftUI

struct SSHHostFingerprintChangeView: View {
    @Bindable var store: ManagementStore
    let job: JobSummary
    let change: SSHHostFingerprintChange
    @State private var showingConfirmation = false
    @State private var verified = false
    @State private var saving = false
    @State private var saved = false
    @State private var errorMessage: String?

    private var node: NodeSummary? { store.nodes.first { $0.id == job.nodeID } }
    private var canConfirm: Bool {
        guard let node, job.retryJobId == nil else { return false }
        return change.canConfirm(state: node.state, savedFingerprint: node.ssh?.hostFingerprint)
    }

    var body: some View {
        GroupBox("SSH 主机指纹已更换") {
            VStack(alignment: .leading, spacing: 10) {
                fingerprints
                Text("连接已中止。请通过服务器控制台或其他可信渠道核对新指纹，确认是预期的主机密钥更换后再保存。")
                    .foregroundStyle(.secondary)
                if saved || node?.ssh?.hostFingerprint == change.observed {
                    Label("新指纹已保存", systemImage: "checkmark.circle")
                    Text(job.kind == "uninstall" ? "请返回节点列表重试删除。" : "可使用下方的重试按钮继续任务；原失败记录会保留。")
                        .font(.caption).foregroundStyle(.secondary)
                } else if canConfirm {
                    Button("核对并信任新指纹…") {
                        verified = false
                        errorMessage = nil
                        showingConfirmation = true
                    }
                    .disabled(!store.isConnected || saving)
                    .accessibilityIdentifier("jobs.fingerprintChange.\(job.id)")
                } else {
                    Text("节点或任务状态已变化，请重新运行 SSH 测试获取当前指纹。")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .sheet(isPresented: $showingConfirmation) {
            VStack(alignment: .leading, spacing: 16) {
                Text("确认更换 SSH 主机指纹").font(.headline)
                Text(node?.name ?? job.nodeName ?? job.nodeID ?? "")
                if let ssh = node?.ssh { Text("\(ssh.username)@\(ssh.host):\(ssh.port)").foregroundStyle(.secondary) }
                fingerprints
                Text("主机重装或密钥轮换可能导致指纹变化，也可能意味着连接到了其他主机。保存后，后续 SSH 连接会使用新指纹进行校验。")
                Toggle("我已通过可信渠道核对新指纹，确认此次变更", isOn: $verified)
                    .accessibilityIdentifier("jobs.fingerprintVerified.\(job.id)")
                    .disabled(saving)
                if let errorMessage { Text(errorMessage).foregroundStyle(.orange).textSelection(.enabled) }
                HStack {
                    if saving { ProgressView().controlSize(.small) }
                    Spacer()
                    Button("取消", role: .cancel) { showingConfirmation = false }
                        .keyboardShortcut(.cancelAction).disabled(saving)
                    Button("信任并保存新指纹") { save() }
                        .disabled(!verified || saving || !store.isConnected || !canConfirm)
                        .accessibilityIdentifier("jobs.saveFingerprintChange.\(job.id)")
                }
            }
            .padding(24)
            .frame(width: 580)
            .interactiveDismissDisabled(saving)
        }
    }

    private var fingerprints: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("原指纹").font(.caption).foregroundStyle(.secondary)
            Text(change.expected).font(.system(.body, design: .monospaced)).textSelection(.enabled)
            Text("新指纹").font(.caption).foregroundStyle(.secondary)
            Text(change.observed).font(.system(.body, design: .monospaced)).textSelection(.enabled)
        }
    }

    private func save() {
        saving = true
        errorMessage = nil
        Task {
            defer { saving = false }
            do {
                try await store.confirmChangedHostFingerprint(job)
                saved = true
                showingConfirmation = false
            } catch { errorMessage = error.localizedDescription }
        }
    }
}
