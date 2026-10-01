import SwiftUI

struct NodesView: View {
    @Bindable var store: ManagementStore
    @State private var showingAddNode = false
    @State private var showingConfiguration = false
    @State private var configurationNodeID: String?
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
                TableColumn("状态", value: \.localizedState)
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
                        Button("配置") {
                            configurationNodeID = node.id
                            showingConfiguration = true
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
        .sheet(isPresented: $showingConfiguration) {
            if let configurationNodeID {
                NodeConfigurationView(store: store, nodeID: configurationNodeID)
            }
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
    @State private var listenAddress = ":443"
    @State private var proxyProbeURL = ""
    @State private var tlsSNI = ""
    @State private var skipCertVerify = false
    @State private var tlsMode = "acme"
    @State private var acmeEmail = ""
    @State private var acmeType = "http"
    @State private var certificatePath = ""
    @State private var privateKeyPath = ""
    @State private var errorMessage: String?
    @State private var createdToken: String?
    @State private var isSaving = false

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("添加节点").font(.title.bold())
            if let createdToken {
                ContentUnavailableView("节点已创建", systemImage: "checkmark.circle", description: Text("节点认证令牌：\n\(createdToken)\n请将令牌保存在安全位置。"))
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
                    Section("Hysteria 连接") {
                        TextField("公开地址", text: $publicHost)
                            .accessibilityLabel("公开地址")
                            .accessibilityIdentifier("node.create.publicHost")
                        TextField("公开端口", text: $publicPort)
                            .accessibilityLabel("公开端口")
                            .accessibilityIdentifier("node.create.publicPort")
                        TextField("监听地址", text: $listenAddress)
                            .accessibilityLabel("监听地址")
                            .accessibilityIdentifier("node.create.listenAddress")
                        TextField("TLS SNI（可选）", text: $tlsSNI)
                            .accessibilityLabel("TLS SNI（可选）")
                            .accessibilityIdentifier("node.create.tlsSNI")
                        Toggle("跳过证书验证", isOn: $skipCertVerify)
                            .accessibilityLabel("跳过证书验证")
                            .accessibilityIdentifier("node.create.skipCertVerify")
                    }
                    Section("部署连通性检查") {
                        TextField("HTTP 探测 URL（可选）", text: $proxyProbeURL, prompt: Text("http://status.example.test/health"))
                            .accessibilityLabel("HTTP 探测 URL（可选）")
                            .accessibilityIdentifier("node.create.proxyProbeURL")
                        Text("默认探测节点的本机统计接口。自定义 ACL 或 outbound 阻止该地址时，填写一个可通过当前路由访问并返回 HTTP 200 的无凭据 URL。")
                            .font(.callout).foregroundStyle(.secondary)
                    }
                    Section("TLS 证书") {
                        Picker("证书来源", selection: $tlsMode) {
                            Text("ACME 自动申请").tag("acme")
                            Text("服务器已有证书").tag("tls")
                        }
                        .accessibilityLabel("证书来源")
                        .accessibilityIdentifier("node.create.tlsMode")
                        if tlsMode == "acme" {
                            TextField("ACME 邮箱", text: $acmeEmail)
                                .accessibilityLabel("ACME 邮箱")
                                .accessibilityIdentifier("node.create.acmeEmail")
                            Picker("验证方式", selection: $acmeType) {
                                Text("HTTP-01（TCP 80）").tag("http")
                                Text("TLS-ALPN-01（TCP 443）").tag("tls")
                            }
                            .accessibilityLabel("验证方式")
                            .accessibilityIdentifier("node.create.acmeType")
                            Text("ACME 域名使用公开连接地址。请先开放对应 TCP 验证端口。")
                                .font(.callout).foregroundStyle(.secondary)
                        } else {
                            TextField("远端证书路径", text: $certificatePath)
                                .accessibilityLabel("远端证书路径")
                                .accessibilityIdentifier("node.create.certificatePath")
                            TextField("远端私钥路径", text: $privateKeyPath)
                                .accessibilityLabel("远端私钥路径")
                                .accessibilityIdentifier("node.create.privateKeyPath")
                            Text("文件需已存在于节点上，并允许 hysteriax 服务账户读取。")
                                .font(.callout).foregroundStyle(.secondary)
                        }
                    }
                }
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
        .frame(width: 600, height: createdToken == nil ? 820 : 360)
    }

    private func save() {
        if let validationError = ProxyProbeURLValidation.error(proxyProbeURL) {
            errorMessage = validationError
            return
        }
        guard let sshPort = Int(sshPort), let publicPort = Int(publicPort) else {
            errorMessage = "端口必须是 1 到 65535 的整数。"
            return
        }
        isSaving = true
        Task {
            defer { isSaving = false }
            do {
                var config: [String: JSONValue] = [:]
                if tlsMode == "acme" {
                    var acme: [String: JSONValue] = [
                        "domains": .array([.string(publicHost)]),
                        "type": .string(acmeType)
                    ]
                    if !acmeEmail.isEmpty { acme["email"] = .string(acmeEmail) }
                    config["acme"] = .object(acme)
                } else {
                    guard !certificatePath.isEmpty, !privateKeyPath.isEmpty else {
                        errorMessage = "请填写节点上的证书和私钥路径。"
                        return
                    }
                    config["tls"] = .object(["cert": .string(certificatePath), "key": .string(privateKeyPath)])
                }
                let response = try await store.createNode(NodeCreateRequest(
                    name: name, sshHost: sshHost, sshPort: sshPort, sshUsername: sshUsername,
                    sshAuthType: sshAuthType, sshSecret: sshSecret,
                    sshPassphrase: sshPassphrase.isEmpty ? nil : sshPassphrase,
                    publicHost: publicHost, publicPort: publicPort, listenAddr: listenAddress,
                    proxyProbeUrl: proxyProbeURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : proxyProbeURL,
                    tlsSNI: tlsSNI.isEmpty ? nil : tlsSNI, tlsSkipVerify: skipCertVerify, config: config
                ))
                createdToken = response.nodeAuthToken
            } catch { errorMessage = error.localizedDescription }
        }
    }
}
