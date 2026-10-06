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
    @State private var showingDNSBindingEditor = false

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
                        CredentialPickerView(store: store, selection: $draft.sshCredentialId, kinds: ["ssh_private_key", "ssh_password"], title: "SSH 凭据")
                            .onChange(of: draft.sshCredentialId) { _, id in
                                draft.sshCredentialVersion = store.credentials.first(where: { $0.id == id })?.latestVersion ?? 1
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
                    if store.supportsDNSManagement {
                        Section("公网地址与域名") {
                            LabeledContent("目标地址", value: detail.connection.host)
                            LabeledContent("已发布地址", value: detail.publishedConnection?.publicHost ?? "尚未部署")
                            if let binding = detail.dnsBinding {
                                ForEach(binding.records) { record in
                                    HStack {
                                        Text("\(record.recordType) · \(record.stateLabel) · \(record.resolutionLabel)")
                                        Spacer()
                                        Button("查看 DNS 记录") { store.showDNSRecord(record.id); dismiss() }
                                    }
                                }
                            }
                            Button("分配或更换域名…") { showingDNSBindingEditor = true }
                            Text("域名变更单独保存。").font(.caption).foregroundStyle(.secondary)
                        }
                        .disabled(isSaving)
                    }
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
        .sheet(isPresented: $showingDNSBindingEditor, onDismiss: { Task { await refreshAfterDNSBinding() } }) {
            DNSBindingEditorView(store: store, nodeID: nodeID)
        }
        .task(id: nodeID) { await load() }
    }

    private func refreshAfterDNSBinding() async {
        do { detail = try await store.nodeDetail(nodeID) }
        catch { message = error.localizedDescription }
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
