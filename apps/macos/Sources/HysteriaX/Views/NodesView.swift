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
                        Text(PackageDisplay.listUsage(node.package, node.packageUsage))
                        if let next = node.packageUsage?.nextResetAt {
                            Text("重置：\(DateDisplayText.local(next))").font(.caption).foregroundStyle(.secondary)
                        }
                        if let alert = node.packageUsage?.alerts.first {
                            Text(PackageDisplay.warning(alert.kind)).font(.caption).foregroundStyle(.orange)
                        }
                    }
                }
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
                        Button("删除节点", role: .destructive) { showingDeleteConfirmation = true }
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
            Button("删除并卸载远端服务", role: .destructive) {
                guard let node = store.nodes.first(where: { $0.id == selection }) else { return }
                Task {
                    do { try await store.deleteNode(node) }
                    catch { actionError = error.localizedDescription }
                }
            }
        } message: {
            Text("已部署节点会先拒绝新认证，再排队停止并移除 HysteriaX 管理的远端文件。")
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
    @State private var sshAuthType = "password"
    @State private var sshSecret = ""
    @State private var sshPassphrase = ""
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
                        Picker("认证方式", selection: $sshAuthType) {
                            Text("密码").tag("password")
                            Text("私钥").tag("private_key")
                        }
                        .accessibilityLabel("认证方式")
                        .accessibilityIdentifier("node.create.sshAuthType")
                        if sshAuthType == "password" {
                            SecureField("SSH 密码", text: $sshSecret)
                                .accessibilityLabel("SSH 密码")
                                .accessibilityIdentifier("node.create.sshPassword")
                        } else {
                            TextEditor(text: $sshSecret)
                                .font(.system(.body, design: .monospaced))
                                .frame(minHeight: 100)
                                .accessibilityLabel("OpenSSH 私钥")
                                .accessibilityIdentifier("node.create.sshPrivateKey")
                                .overlay(alignment: .topLeading) {
                                    if sshSecret.isEmpty { Text("粘贴 OpenSSH 私钥").foregroundStyle(.tertiary).padding(.top, 8).padding(.leading, 5) }
                                }
                            SecureField("私钥口令（可选）", text: $sshPassphrase)
                                .accessibilityLabel("私钥口令（可选）")
                                .accessibilityIdentifier("node.create.sshPassphrase")
                        }
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
                    sshAuthType: sshAuthType, sshSecret: sshSecret,
                    sshPassphrase: sshPassphrase.isEmpty ? nil : sshPassphrase,
                    publicHost: publicHost, publicPort: publicPort, listenAddr: ":\(publicPort)"
                ))
                createdToken = response.nodeAuthToken
            } catch { errorMessage = error.localizedDescription }
        }
    }
}
