import SwiftUI

struct NodesView: View {
    @Bindable var store: ManagementStore
    var initialSelection: String? = nil
    var onInitialSelectionHandled: () -> Void = {}
    @State private var showingAddNode = false
    @State private var configurationNode: NodeSummary?
    @State private var serverConfigurationNode: NodeSummary?
    @State private var selection: String?
    @State private var searchText = ""
    @State private var sortOrder = [KeyPathComparator(\NodeSummary.name)]
    @State private var actionError: String?
    @State private var showingDeleteConfirmation = false
    @State private var deletionNode: NodeSummary?
    @State private var showingRecordRemovalConfirmation = false

    private var visibleNodes: [NodeSummary] {
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        let filtered = store.nodes.filter { node in
            query.isEmpty
                || node.name.localizedStandardContains(query)
                || node.ssh?.host.localizedCaseInsensitiveContains(query) == true
                || node.id.localizedCaseInsensitiveContains(query)
                || node.state.localizedCaseInsensitiveContains(query)
        }
        return filtered.sorted(using: sortOrder)
    }

    var body: some View {
        VStack(spacing: 0) {
            Table(visibleNodes, selection: $selection, sortOrder: $sortOrder) {
                TableColumn("名称", value: \.name) { node in
                    Text(node.name)
                        .accessibilityLabel(node.name)
                        .accessibilityIdentifier("nodes.row.\(node.id)")
                }
                TableColumn("IP / 主机", value: \.displayHost) { node in
                    Text(node.displayHost)
                        .monospaced()
                        .textSelection(.enabled)
                        .lineLimit(1)
                        .help(node.ssh?.host ?? "暂无 SSH 主机地址")
                }
                .width(min: 120, ideal: 180)
                TableColumn("状态", value: \.localizedState)
                TableColumn("有效期") { node in Text(PackageDisplay.expiry(node.package)) }
                TableColumn("套餐流量") { node in
                    VStack(alignment: .leading, spacing: 2) {
                        QuotaProgressView(
                            usageBytes: node.packageUsage?.usageBytes,
                            quotaBytes: node.package?.quotaBytes
                        )
                        if let next = node.packageUsage?.nextResetAt {
                            Text("重置：\(DateDisplayText.local(next))").font(.caption).foregroundStyle(.secondary)
                        }
                        if let alert = node.packageUsage?.alerts.first {
                            Text(PackageDisplay.warning(alert.kind)).font(.caption).foregroundStyle(.orange)
                        }
                    }
                }
                .width(min: 180, ideal: 220)
                TableColumn("配置版本") { node in Text("\(node.revision)") }
                TableColumn("已部署") { node in Text(node.deployedRevision.map(String.init) ?? "—") }
                TableColumn("最近采样") { node in
                    VStack(alignment: .leading, spacing: 2) {
                        Text(DateDisplayText.local(node.lastSampleAt))
                        Text(store.isConnected ? node.localizedFreshness : "离线缓存")
                            .font(.caption)
                            .foregroundStyle(store.isConnected ? node.freshnessColor : Color.gray)
                    }
                }
                TableColumn("统计缺口") { node in
                    Text(node.openGaps.map(String.init) ?? "—")
                        .foregroundStyle((node.openGaps ?? 0) > 0 ? Color.orange : Color.gray)
                }
                TableColumn("待撤权") { node in
                    Text(node.pendingRevocations.map(String.init) ?? "—")
                        .foregroundStyle((node.pendingRevocations ?? 0) > 0 ? Color.orange : Color.gray)
                }
            }
            .task(id: initialSelection) {
                guard let initialSelection else { return }
                searchText = ""
                selection = initialSelection
                onInitialSelectionHandled()
            }
            .overlay {
                if store.nodes.isEmpty {
                    ContentUnavailableView("还没有节点", systemImage: "server.rack", description: Text("添加一台服务器，填写 SSH 和公开连接信息。"))
                } else if visibleNodes.isEmpty {
                    ContentUnavailableView("没有匹配的节点", systemImage: "magnifyingglass")
                }
            }
            .safeAreaInset(edge: .bottom) {
                if let node = store.nodes.first(where: { $0.id == selection }) {
                    HStack(spacing: 10) {
                        Text("\(node.name) · 修订版 \(node.revision)").foregroundStyle(.secondary)
                        Spacer()
                        Button("SSH 测试") { run(node, action: "ssh-test") }
                        Button("服务器配置") { serverConfigurationNode = node }
                        Button("代理配置") {
                            configurationNode = node
                        }
                        Button("部署") { run(node, action: "deploy") }
                        Button("同步") { run(node, action: "sync") }
                        Button("回滚") { run(node, action: "rollback") }
                            .disabled(node.deployedRevision == nil)
                        Button("删除节点", role: .destructive) { deletionNode = node; showingDeleteConfirmation = true }
                    }
                    .disabled(!store.isConnected)
                    .padding(12)
                    .background(.bar)
                }
            }
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button { showingAddNode = true } label: { Label("添加节点", systemImage: "plus") }
                    .keyboardShortcut("n", modifiers: .command)
                    .disabled(!store.isConnected)
            }
        }
        .searchable(text: $searchText, prompt: "搜索节点")
        .onChange(of: searchText) { _, _ in selection = nil }
        .sheet(isPresented: $showingAddNode) { NodeFormView(store: store) }
        .sheet(item: $serverConfigurationNode) { node in
            ServerConfigurationView(store: store, nodeID: node.id)
        }
        .sheet(item: $configurationNode) { node in
            ProxyConfigurationView(store: store, nodeID: node.id)
        }
        .alert("节点操作失败", isPresented: Binding(get: { actionError != nil }, set: { if !$0 { actionError = nil } })) {
            Button("好", role: .cancel) { actionError = nil }
        } message: { Text(actionError ?? "") }
        .confirmationDialog("删除节点？", isPresented: $showingDeleteConfirmation, titleVisibility: .visible) {
            Button("卸载并删除", role: .destructive) {
                guard let node = deletionNode else { return }
                Task {
                    do { try await store.deleteNode(node); selection = nil }
                    catch { actionError = error.localizedDescription }
                }
            }
            if store.supportsNodeRecordRemoval {
                Button("仅移除管理记录", role: .destructive) { showingRecordRemovalConfirmation = true }
                    .accessibilityIdentifier("nodes.remove-record")
            }
        } message: {
            Text("卸载并删除需要 SSH 连接。仅移除管理记录不需要 SSH，远端服务可能继续运行。")
        }
        .alert("仅移除管理记录？", isPresented: $showingRecordRemovalConfirmation) {
            Button("取消", role: .cancel) {}
            Button("移除记录", role: .destructive) {
                guard let node = deletionNode else { return }
                Task {
                    do { try await store.removeNodeRecord(node); selection = nil }
                    catch { actionError = error.localizedDescription }
                }
            }
            .accessibilityIdentifier("nodes.confirm-remove-record")
        } message: {
            Text("将从管理服务中移除「\(deletionNode?.name ?? "")」及其用户分配和待办，保留任务历史。此操作不会卸载远端服务；失联服务器上的服务可能仍在运行。")
        }
    }

    private func run(_ node: NodeSummary, action: String) {
        Task {
            do { try await store.runNodeAction(node, action: action) }
            catch { actionError = error.localizedDescription }
        }
    }
}

private extension NodeSummary {
    var displayHost: String {
        guard let host = ssh?.host, !host.isEmpty else { return "—" }
        return host
    }

    var localizedState: String {
        let labels = [
            "new": "未部署",
            "needs_fingerprint": "待确认指纹",
            "fingerprint_changed": "指纹已变更",
            "ready": "待部署",
            "syncing": "同步中",
            "deployed": "已部署",
            "rolled_back": "已回滚",
            "sync_failed": "同步失败",
            "rollback_failed": "回滚失败",
            "drift": "远端配置已变更",
            "unreachable": "无法连接",
            "deleting": "卸载中",
            "delete_failed": "卸载失败",
        ]
        return labels[state] ?? state
    }

    var localizedFreshness: String {
        switch dataFreshness {
        case .some("fresh"): "统计正常"
        case .some("stale"): "采样已陈旧"
        case .some("not_collected"): "尚未采集"
        case .none: "—"
        case .some(_): "统计状态未知"
        }
    }

    var freshnessColor: Color {
        switch dataFreshness {
        case .some("fresh"): .green
        case .some("stale"): .orange
        default: .gray
        }
    }
}

private struct NodeFormView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ManagementStore
    @State private var name = ""
    @State private var sshHost = ""
    @State private var sshPort = "22"
    @State private var sshUsername = "root"
    @State private var sshCredentialId = ""
    @State private var publicHost = ""
    @State private var publicPort = "443"
    @State private var errorMessage: String?
    @State private var packageDraft = NodePackageDraft()
    @State private var initialUsageGB = "0"
    @State private var createdToken: String?
    @State private var isSaving = false

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("添加节点").font(.title.bold())
            if let createdToken {
                ContentUnavailableView("节点已创建", systemImage: "checkmark.circle", description: Text("节点认证令牌：\n\(createdToken)\n请将令牌保存在安全位置，并在代理配置页设置 TLS 证书后再部署。"))
                HStack { Spacer(); Button("完成") { dismiss() }.keyboardShortcut(.defaultAction) }
            } else {
                Form {
                    Section("SSH 连接") {
                        TextField("节点名称", text: $name)
                            .accessibilityLabel("节点名称")
                            .accessibilityIdentifier("node.create.name")
                        TextField("SSH 地址", text: $sshHost)
                            .accessibilityLabel("SSH 地址")
                            .accessibilityIdentifier("node.create.sshHost")
                        TextField("SSH 端口", text: $sshPort)
                            .accessibilityLabel("SSH 端口")
                            .accessibilityIdentifier("node.create.sshPort")
                        TextField("SSH 用户", text: $sshUsername)
                            .accessibilityLabel("SSH 用户")
                            .accessibilityIdentifier("node.create.sshUsername")
                        CredentialPickerView(store: store, selection: $sshCredentialId, kinds: ["ssh_private_key", "ssh_password"], title: "SSH 凭据")
                    }
                    Section("有效期与流量套餐") {
                        if store.supportsNodePackages {
                            NodePackageFields(draft: $packageDraft)
                            if packageDraft.hasQuota { TextField("已有用量（GB）", text: $initialUsageGB) }
                        } else { Text("升级管理服务后可设置有效期和流量套餐。").foregroundStyle(.secondary) }
                    }
                    Section("公开连接") {
                        TextField("公开地址", text: $publicHost)
                            .accessibilityLabel("公开地址")
                            .accessibilityIdentifier("node.create.publicHost")
                        TextField("公开端口", text: $publicPort)
                            .accessibilityLabel("公开端口")
                            .accessibilityIdentifier("node.create.publicPort")
                        Text("用户客户端通过此地址和 UDP 端口连接节点。创建后请在代理配置页设置 TLS 证书及其他 Hysteria 参数。")
                            .font(.callout).foregroundStyle(.secondary)
                    }
                }
                .formStyle(.grouped)
                if let errorMessage { Text(errorMessage).foregroundStyle(.red).font(.callout) }
                HStack {
                    Button("取消") { dismiss() }.keyboardShortcut(.cancelAction)
                    Spacer()
                    Button(isSaving ? "正在保存…" : "创建节点") { save() }
                        .disabled(isSaving)
                        .keyboardShortcut(.defaultAction)
                }
            }
        }
        .padding(24)
        .frame(width: 600, height: createdToken == nil ? 620 : 360)
    }

    private func save() {
        guard let sshPort = Int(sshPort), (1...65535).contains(sshPort),
              let publicPort = Int(publicPort), (1...65535).contains(publicPort) else {
            errorMessage = "SSH 和公开端口都必须是 1 到 65535 的整数。"
            return
        }
        isSaving = true
        Task {
            defer { isSaving = false }
            do {
                let response = try await store.createNode(NodeCreateRequest(
                    package: store.supportsNodePackages ? packageDraft.package() : nil,
                    initialUsageBytes: store.supportsNodePackages ? NodePackageDraft.bytes(initialUsageGB) : nil,
                    name: name, sshHost: sshHost, sshPort: sshPort, sshUsername: sshUsername,
                    publicHost: publicHost, publicPort: publicPort, listenAddr: ":\(publicPort)",
                    sshCredentialId: sshCredentialId, sshCredentialVersion: store.credentials.first(where: { $0.id == sshCredentialId })?.latestVersion ?? 1
                ))
                createdToken = response.nodeAuthToken
            } catch { errorMessage = error.localizedDescription }
        }
    }
}
