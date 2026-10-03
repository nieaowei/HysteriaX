import SwiftUI

struct ServerConfigurationView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ManagementStore
    let nodeID: String
    @State private var detail: NodeDetail?
    @State private var draft = ServerConfigurationDraft()
    @State private var isLoading = true
    @State private var isSaving = false
    @State private var message: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                VStack(alignment: .leading, spacing: 4) {
                    Text("服务器配置").font(.title.bold())
                    Text(detail.map { "\($0.name) · 修订版 \($0.revision)" } ?? "读取服务器信息…")
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button("重新加载") { Task { await load() } }
                    .disabled(isLoading || isSaving || !store.isConnected)
                Button("关闭") { dismiss() }.keyboardShortcut(.cancelAction)
                    .disabled(isSaving)
            }
            if isLoading {
                ProgressView("读取服务器配置…").frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if let detail {
                Form {
                    Section("基本信息与 SSH") {
                        TextField("服务器名称", text: $draft.name)
                        TextField("SSH 地址", text: $draft.sshHost)
                        TextField("SSH 端口", text: $draft.sshPort)
                        TextField("SSH 用户", text: $draft.sshUsername)
                        Picker("认证方式", selection: $draft.sshAuthType) {
                            Text("密码").tag("password")
                            Text("私钥").tag("private_key")
                        }
                        .onChange(of: draft.sshAuthType) { _, _ in
                            draft.sshSecret = ""
                            draft.sshPassphrase = ""
                            draft.clearPassphrase = false
                        }
                        if draft.sshAuthType == "password" {
                            SecureField("新 SSH 密码（留空保留）", text: $draft.sshSecret)
                        } else {
                            Text("新 OpenSSH 私钥（留空保留）").font(.callout)
                            TextEditor(text: $draft.sshSecret)
                                .font(.system(.body, design: .monospaced))
                                .frame(minHeight: 110)
                                .accessibilityLabel("新 OpenSSH 私钥")
                            SecureField("新私钥口令（留空保留）", text: $draft.sshPassphrase)
                                .disabled(draft.clearPassphrase)
                            Toggle("清除已有私钥口令", isOn: $draft.clearPassphrase)
                        }
                        LabeledContent("已信任的 SSH 指纹", value: detail.ssh.hostFingerprint ?? "尚未确认")
                            .textSelection(.enabled)
                        Text("已有密码和私钥不会回显。保存连接信息后，可在节点列表执行 SSH 测试；首次连接或指纹变更需在任务详情中确认。")
                            .font(.callout).foregroundStyle(.secondary)
                        HStack {
                            Spacer()
                            Button(isSaving ? "正在保存…" : "保存基本信息及 SSH") { save() }
                                .disabled(isSaving || !store.isConnected)
                        }
                    }
                    .disabled(isSaving)
                    Section("有效期与流量套餐") {
                        NodePackageManagementView(store: store, detail: detail, onUpdated: { self.detail = $0 }, onSavingChanged: { isSaving = $0 })
                    }
                    .disabled(isSaving)
                }
                .formStyle(.grouped)
            } else {
                ContentUnavailableView("无法加载服务器配置", systemImage: "server.rack", description: Text(message ?? "请稍后重试。"))
            }
            if let message { Text(message).font(.callout).foregroundStyle(.secondary) }
        }
        .padding(24)
        .frame(minWidth: 640, idealWidth: 720, minHeight: 660)
        .interactiveDismissDisabled(isSaving)
        .task(id: nodeID) { await load() }
    }

    private func load() async {
        isLoading = true
        detail = nil
        message = nil
        defer { isLoading = false }
        do {
            let loaded = try await store.nodeDetail(nodeID)
            detail = loaded
            draft = ServerConfigurationDraft(loaded)
        } catch { message = error.localizedDescription }
    }

    private func save() {
        guard let detail else { return }
        do {
            let request = try draft.request(revision: detail.revision, originalAuthType: detail.ssh.authType)
            isSaving = true
            message = nil
            Task {
                defer { isSaving = false }
                do {
                    try await store.updateServerConfiguration(nodeID: nodeID, request: request)
                    let updated = try await store.nodeDetail(nodeID)
                    self.detail = updated
                    draft = ServerConfigurationDraft(updated)
                    message = "基本信息及 SSH 已保存。"
                } catch { message = error.localizedDescription }
            }
        } catch { message = error.localizedDescription }
    }
}
