import SwiftUI

struct SSHJobFingerprintConfirmationView: View {
    @Bindable var store: ManagementStore
    let job: JobSummary
    let fingerprint: String
    @State private var verified = false
    @State private var saving = false
    @State private var saved = false
    @State private var errorMessage: String?

    var body: some View {
        GroupBox(L10n.text("等待确认 SSH 指纹")) {
            VStack(alignment: .leading, spacing: 10) {
                Text(job.nodeName ?? job.nodeID ?? "").font(.headline)
                if let previous = job.result?.result?.expectedFingerprint {
                    Text(L10n.text("原指纹")).font(.caption).foregroundStyle(.secondary)
                    Text(previous).font(.system(.callout, design: .monospaced)).textSelection(.enabled)
                    Text(L10n.text("新指纹")).font(.caption).foregroundStyle(.secondary)
                }
                Text(fingerprint).font(.system(.callout, design: .monospaced)).textSelection(.enabled)
                Text(L10n.text("核对指纹后将重新连接，验证主机密钥，再继续{0}。", JobDisplayText.kind(job.kind)))
                    .foregroundStyle(.secondary)
                if saved {
                    Label(L10n.text("指纹已保存，正在继续原任务。"), systemImage: "checkmark.circle")
                } else {
                    Toggle(L10n.text("我已通过可信渠道核对此 SSH 主机指纹"), isOn: $verified)
                        .disabled(saving)
                        .accessibilityIdentifier("jobs.fingerprintVerified.\(job.id)")
                    HStack {
                        Button(JobDisplayText.fingerprintContinuation(job.kind)) { confirm() }
                            .disabled(!verified || saving || !store.isConnected)
                            .accessibilityIdentifier("jobs.continueJob.\(job.id)")
                        if saving { ProgressView().controlSize(.small) }
                    }
                }
                if let errorMessage { Text(errorMessage).foregroundStyle(.orange).textSelection(.enabled) }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, 4)
        }
    }

    private func confirm() {
        saving = true
        errorMessage = nil
        Task {
            defer { saving = false }
            do {
                try await store.confirmJobFingerprint(job, fingerprint: fingerprint)
                saved = true
            } catch { errorMessage = error.localizedDescription }
        }
    }
}
