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
    @State private var detail: NodeDetail?
    @State private var detailError: String?
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
                || node.localizedState.localizedCaseInsensitiveContains(query)
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
                .width(min: 110, ideal: 150)
                TableColumn("SSH 地址", value: \.displayHost) { node in
                    Text(node.displayHost)
                        .monospaced()
                        .textSelection(.enabled)
                        .lineLimit(1)
                        .help(node.ssh?.host ?? "暂无 SSH 主机地址")
                }
                .width(min: 110, ideal: 150)
                TableColumn("状态", value: \.localizedState) { node in
                    Text(node.localizedState).foregroundStyle(node.stateColor)
                }
                .width(min: 90, ideal: 110)
                TableColumn("配置") { node in
                    VStack(alignment: .leading, spacing: 3) {
                        Text("目标 v\(node.revision)").monospacedDigit()
                        Text(node.deployedRevision.map { "已部署 v\($0)" } ?? "尚未部署")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
                .width(min: 90, ideal: 110)
                TableColumn("套餐") { node in
                    VStack(alignment: .leading, spacing: 3) {
                        QuotaProgressView(usageBytes: node.packageUsage?.usageBytes, quotaBytes: node.package?.quotaBytes)
                        Text(PackageDisplay.expiry(node.package))
                            .font(.caption)
                            .foregroundStyle(node.packageUsage?.restricted == true ? Color.red : Color.secondary)
                    }
                }
                .width(min: 150, ideal: 180)
                TableColumn("采样") { node in
                    VStack(alignment: .leading, spacing: 3) {
                        Text(store.isConnected ? node.localizedFreshness : "离线缓存")
                            .foregroundStyle(store.isConnected ? node.freshnessColor : Color.secondary)
                        if (node.openGaps ?? 0) > 0 || (node.pendingRevocations ?? 0) > 0 {
                            Text("缺口 \(node.openGaps ?? 0) · 待撤权 \(node.pendingRevocations ?? 0)")
                                .font(.caption).foregroundStyle(.orange)
                        } else {
                            Text(DateDisplayText.local(node.lastSampleAt)).font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }
                .width(min: 130, ideal: 160)
            }
            .frame(minHeight: 180)
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
            if let node = store.nodes.first(where: { $0.id == selection }) {
                Divider()
                nodeDetailPane(node)
                    .id(node.id)
                    .frame(maxHeight: 400)
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier("nodes.detail")
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
        .onChange(of: selection) { _, _ in detail = nil; detailError = nil }
        .task(id: detailRequestKey) { await loadDetail() }
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

    private var detailRequestKey: String {
        let node = store.nodes.first { $0.id == selection }
        return "\(store.serviceAddress):\(selection ?? ""):\(node?.revision ?? 0):\(store.isConnected)"
    }

    private func loadDetail() async {
        guard let selection, store.isConnected else { detail = nil; return }
        let service = store.serviceAddress
        do {
            let loaded = try await store.nodeDetail(selection)
            guard !Task.isCancelled, self.selection == selection, store.serviceAddress == service else { return }
            detail = loaded
            detailError = nil
        } catch {
            guard !Task.isCancelled, self.selection == selection, store.serviceAddress == service else { return }
            detailError = error.localizedDescription
        }
    }

    private func nodeDetailPane(_ node: NodeSummary) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 14) {
                ViewThatFits(in: .horizontal) {
                    HStack(alignment: .top, spacing: 16) {
                        nodeTitle(node)
                        Spacer(minLength: 20)
                        nodeActions(node)
                    }
                    VStack(alignment: .leading, spacing: 10) {
                        nodeTitle(node)
                        nodeActions(node)
                    }
                }
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), alignment: .leading)], alignment: .leading, spacing: 12) {
                    nodeMetric("目标配置", value: "v\(node.revision)")
                    nodeMetric("已部署配置", value: node.deployedRevision.map { "v\($0)" } ?? "尚未部署")
                    nodeMetric("套餐有效期", value: PackageDisplay.expiry(node.package))
                    if let binding = node.dnsBinding {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("节点域名").font(.caption).foregroundStyle(.secondary)
                            Button {
                                store.showDNSRecord(binding.recordIds.first)
                            } label: {
                                Text(binding.hostname)
                            }
                            .buttonStyle(.link)
                            .help("查看 DNS 记录")
                        }
                    }
                    nodeMetric("分配用户", value: "\(assignedUsers(node).count) 人")
                }
            }
            .padding(16)
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    ViewThatFits(in: .horizontal) {
                        HStack(alignment: .top, spacing: 16) {
                            nodeConnections(node).frame(minWidth: 280, maxWidth: .infinity)
                            nodePackage(node).frame(minWidth: 280, maxWidth: .infinity)
                        }
                        VStack(alignment: .leading, spacing: 16) {
                            nodeConnections(node)
                            nodePackage(node)
                        }
                    }
                    GroupBox {
                        LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), alignment: .leading)], alignment: .leading, spacing: 12) {
                            nodeMetric("采样状态", value: store.isConnected ? node.localizedFreshness : "离线缓存")
                            nodeMetric("最近采样", value: DateDisplayText.local(node.lastSampleAt))
                            nodeMetric("统计缺口", value: node.openGaps.map(String.init) ?? "未知")
                            nodeMetric("待撤权任务", value: node.pendingRevocations.map(String.init) ?? "未知")
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.vertical, 4)
                    } label: { Label("采样与待办", systemImage: "waveform.path") }
                    DisclosureGroup("分配用户（\(assignedUsers(node).count)）") {
                        LazyVGrid(columns: [GridItem(.adaptive(minimum: 180), alignment: .leading)], alignment: .leading, spacing: 10) {
                            ForEach(assignedUsers(node)) { user in
                                HStack {
                                    Text(user.name)
                                    Spacer()
                                    Text(user.enabled ? "启用" : "已停用").font(.caption).foregroundStyle(.secondary)
                                }
                            }
                            if assignedUsers(node).isEmpty { Text("尚未分配用户").foregroundStyle(.secondary) }
                        }
                        .padding(.top, 8)
                    }
                    DisclosureGroup("节点记录") {
                        VStack(alignment: .leading, spacing: 10) {
                            nodeMetric("节点 ID", value: node.id)
                            if let detail, detail.id == node.id {
                                nodeMetric("创建时间", value: DateDisplayText.local(detail.createdAt))
                                nodeMetric("更新时间", value: DateDisplayText.local(detail.updatedAt))
                            }
                        }
                        .padding(.top, 8)
                    }
                }
                .padding(16)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    private func nodeTitle(_ node: NodeSummary) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text(node.name).font(.headline).lineLimit(2).textSelection(.enabled)
            Text(node.localizedState).font(.caption.weight(.medium))
                .foregroundStyle(node.stateColor)
                .padding(.horizontal, 8).padding(.vertical, 3)
                .background(node.stateColor.opacity(0.1), in: Capsule())
        }
    }

    private func nodeActions(_ node: NodeSummary) -> some View {
        HStack(spacing: 8) {
            Button("SSH 测试") { run(node, action: "ssh-test") }
            Menu("配置") {
                Button("服务器配置") { serverConfigurationNode = node }
                Button("代理配置") { configurationNode = node }
            }.accessibilityIdentifier("node.configure.\(node.id)")
            Button(node.deployedRevision == nil ? "部署" : "同步") {
                run(node, action: node.deployedRevision == nil ? "deploy" : "sync")
            }
            Menu("更多") {
                Button("部署") { run(node, action: "deploy") }
                Button("同步") { run(node, action: "sync") }
                Button("回滚") { run(node, action: "rollback") }.disabled(node.deployedRevision == nil)
                Divider()
                Button("删除节点", role: .destructive) { deletionNode = node; showingDeleteConfirmation = true }
            }
        }
        .fixedSize()
        .disabled(!store.isConnected)
    }

    private func nodeConnections(_ node: NodeSummary) -> some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 12) {
                if let ssh = node.ssh {
                    nodeMetric("SSH 连接", value: "\(ssh.username)@\(ssh.host):\(ssh.port)")
                    let credential = store.credentials.first { $0.id == ssh.credentialId }
                    nodeMetric("SSH 凭据", value: "\(credential?.name ?? CredentialDisplay.kind(ssh.authType)) · v\(ssh.credentialVersion)")
                    DisclosureGroup("主机指纹") {
                        Text(ssh.hostFingerprint ?? "尚未确认")
                            .font(.system(.caption, design: .monospaced)).textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading).padding(.top, 6)
                    }
                } else { Text("暂无 SSH 连接信息").foregroundStyle(.secondary) }
                Divider()
                if let detail, detail.id == node.id {
                    nodeMetric("公开连接", value: "\(detail.connection.host):\(detail.connection.port)")
                    nodeMetric("监听地址", value: detail.connection.listenAddress)
                    if let sni = detail.connection.tlsSNI, !sni.isEmpty { nodeMetric("TLS SNI", value: sni) }
                } else if let detailError {
                    Text("无法读取公开连接：\(detailError)").font(.caption).foregroundStyle(.orange)
                    Button("重试读取") { Task { await loadDetail() } }.disabled(!store.isConnected)
                } else if store.isConnected {
                    ProgressView("读取公开连接…").controlSize(.small)
                } else { Text("连接服务后可读取公开连接信息。").font(.caption).foregroundStyle(.secondary) }
            }
            .frame(maxWidth: .infinity, alignment: .leading).padding(.vertical, 4)
        } label: { Label("连接信息", systemImage: "network") }
    }

    private func nodePackage(_ node: NodeSummary) -> some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 12) {
                QuotaProgressView(usageBytes: node.packageUsage?.usageBytes, quotaBytes: node.package?.quotaBytes)
                NodePackageStatus(package: node.package, usage: node.packageUsage)
            }
            .frame(maxWidth: .infinity, alignment: .leading).padding(.vertical, 4)
        } label: { Label("套餐与流量", systemImage: "chart.bar") }
    }

    private func assignedUsers(_ node: NodeSummary) -> [UserSummary] {
        store.users.filter { $0.assignments.contains { $0.nodeID == node.id } }
    }

    private func nodeMetric(_ title: String, value: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            Text(value).font(.callout).textSelection(.enabled)
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
    var stateColor: Color {
        switch state {
        case "deployed": .green
        case "syncing", "deleting": .blue
        case "sync_failed", "rollback_failed", "delete_failed", "unreachable", "fingerprint_changed": .orange
        default: .secondary
        }
    }

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
    @State private var dnsDraft = DNSAllocationDraft()
    @State private var createdNodeID: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("添加节点").font(.title.bold())
            if createdNodeID != nil {
                ContentUnavailableView("节点已创建", systemImage: "checkmark.circle", description: Text("节点认证令牌：\n\(createdToken ?? "此前请求已创建节点，令牌不会重复显示。")\n请将令牌保存在安全位置，并在代理配置页设置 TLS 证书后再部署。"))
                HStack { Spacer(); Button("完成") { dismiss() }.keyboardShortcut(.defaultAction) }
            } else {
                Form {
                    Section("SSH 连接") {
                        TextField("节点名称", text: $name)
                            .accessibilityLabel("节点名称")
                            .accessibilityIdentifier("node.create.name")
                        TextField("SSH 地址", text: $sshHost)
                            .onChange(of: sshHost) { oldValue, newValue in
                                if publicHost.isEmpty || publicHost == oldValue {
                                    publicHost = newValue
                                }
                            }
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
                        if store.supportsDNSManagement {
                            DNSAllocationFields(store: store, draft: $dnsDraft, sshHost: sshHost)
                        }
                        if dnsDraft.mode == "external" {
                            TextField("公开地址", text: $publicHost)
                                .accessibilityLabel("公开地址")
                                .accessibilityIdentifier("node.create.publicHost")
                        }
                        TextField("公开端口", text: $publicPort)
                            .accessibilityLabel("公开端口")
                            .accessibilityIdentifier("node.create.publicPort")
                        Text(dnsDraft.mode == "external" ? "公开地址默认跟随 SSH 地址，可手动修改。公开端口用于初始化监听端口；创建后可在代理配置中设置端口联动、TLS 证书及其他 Hysteria 参数。" : "公开地址使用所分配的域名。公开端口用于初始化监听端口；创建后在代理配置中设置 TLS 证书及其他 Hysteria 参数。")
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
        .frame(width: 640, height: createdNodeID == nil ? 660 : 360)
        .task { await store.refreshDNS() }
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
                let allocation = try dnsDraft.allocation(zones: store.dnsZones, records: store.dnsRecords)
                let response = try await store.createNode(NodeCreateRequest(
                    dnsAllocation: allocation,
                    package: store.supportsNodePackages ? packageDraft.package() : nil,
                    initialUsageBytes: store.supportsNodePackages ? NodePackageDraft.bytes(initialUsageGB) : nil,
                    name: name, sshHost: sshHost, sshPort: sshPort, sshUsername: sshUsername,
                    publicHost: publicHost, publicPort: publicPort, listenAddr: ":\(publicPort)",
                    sshCredentialId: sshCredentialId, sshCredentialVersion: store.credentials.first(where: { $0.id == sshCredentialId })?.latestVersion ?? 1
                ))
                createdToken = response.nodeAuthToken
                createdNodeID = response.node?.id
            } catch { errorMessage = error.localizedDescription }
        }
    }
}
