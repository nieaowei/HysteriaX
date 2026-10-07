import SwiftUI

struct NodesView: View {
    @Bindable var store: ManagementStore
    var initialSelection: String? = nil
    var onInitialSelectionHandled: () -> Void = {}
    var onOpenJob: (String) -> Void = { _ in }
    @State private var submittingActions: [String: String] = [:]
    @State private var submittedActions: [String: SubmittedNodeAction] = [:]
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
        MainVerticalSplitView(hasDetail: store.nodes.contains { $0.id == selection }) {
            Table(visibleNodes, selection: $selection, sortOrder: $sortOrder) {
                TableColumn(L10n.text("名称"), value: \.name) { node in
                    Text(node.name)
                        .accessibilityLabel(node.name)
                        .accessibilityIdentifier("nodes.row.\(node.id)")
                }
                .width(min: 110, ideal: 150)
                TableColumn(L10n.text("SSH 地址"), value: \.displayHost) { node in
                    Text(node.displayHost)
                        .monospaced()
                        .textSelection(.enabled)
                        .lineLimit(1)
                        .help(node.ssh?.host ?? L10n.text("暂无 SSH 主机地址"))
                }
                .width(min: 110, ideal: 150)
                TableColumn(L10n.text("状态"), value: \.localizedState) { node in
                    Text(node.localizedState).foregroundStyle(node.stateColor)
                }
                .width(min: 90, ideal: 110)
                TableColumn(L10n.text("配置")) { node in
                    VStack(alignment: .leading, spacing: 3) {
                        Text(L10n.text("目标 v{0}", String(describing: (node.revision)))).monospacedDigit()
                        Text(node.deployedRevision.map { L10n.text("已部署 v{0}", String(describing: ($0))) } ?? L10n.text("尚未部署"))
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
                .width(min: 90, ideal: 110)
                TableColumn(L10n.text("套餐")) { node in
                    VStack(alignment: .leading, spacing: 3) {
                        QuotaProgressView(usageBytes: node.packageUsage?.usageBytes, quotaBytes: node.package?.quotaBytes)
                        Text(PackageDisplay.expiry(node.package))
                            .font(.caption)
                            .foregroundStyle(node.packageUsage?.restricted == true ? Color.red : Color.secondary)
                    }
                }
                .width(min: 150, ideal: 180)
                TableColumn(L10n.text("采样")) { node in
                    VStack(alignment: .leading, spacing: 3) {
                        Text(store.isConnected ? node.localizedFreshness : L10n.text("离线缓存"))
                            .foregroundStyle(store.isConnected ? node.freshnessColor : Color.secondary)
                        if (node.openGaps ?? 0) > 0 || (node.pendingRevocations ?? 0) > 0 {
                            Text(L10n.text("缺口 {0} · 待撤权 {1}", String(describing: (node.openGaps ?? 0)), String(describing: (node.pendingRevocations ?? 0))))
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
                    ContentUnavailableView(L10n.text("还没有节点"), systemImage: "server.rack", description: Text(L10n.text("添加一台服务器，填写 SSH 和公开连接信息。")))
                } else if visibleNodes.isEmpty {
                    ContentUnavailableView(L10n.text("没有匹配的节点"), systemImage: "magnifyingglass")
                }
            }
        } detail: {
            if let node = store.nodes.first(where: { $0.id == selection }) {
                nodeDetailPane(node)
                    .id(node.id)
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier("nodes.detail")
            }
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button { showingAddNode = true } label: { Label(L10n.text("添加节点"), systemImage: "plus") }
                    .keyboardShortcut("n", modifiers: .command)
                    .disabled(!store.isConnected)
            }
        }
        .searchable(text: $searchText, prompt: L10n.text("搜索节点"))
        .onChange(of: searchText) { _, _ in selection = nil }
        .onChange(of: store.serviceAddress) { _, _ in
            submittingActions = [:]
            submittedActions = [:]
        }
        .onChange(of: store.jobs.map(\.id)) { _, ids in
            let knownIDs = Set(ids)
            submittedActions = submittedActions.filter { !knownIDs.contains($0.value.jobID) }
        }
        .onChange(of: selection) { _, _ in detail = nil; detailError = nil }
        .task(id: detailRequestKey) { await loadDetail() }
        .sheet(isPresented: $showingAddNode) { NodeFormView(store: store) }
        .sheet(item: $serverConfigurationNode) { node in
            ServerConfigurationView(store: store, nodeID: node.id)
        }
        .sheet(item: $configurationNode) { node in
            ProxyConfigurationView(store: store, nodeID: node.id)
        }
        .alert(L10n.text("节点操作失败"), isPresented: Binding(get: { actionError != nil }, set: { if !$0 { actionError = nil } })) {
            Button(L10n.text("好"), role: .cancel) { actionError = nil }
        } message: { Text(actionError ?? "") }
        .confirmationDialog(L10n.text("删除节点？"), isPresented: $showingDeleteConfirmation, titleVisibility: .visible) {
            Button(L10n.text("卸载并删除"), role: .destructive) {
                guard let node = deletionNode else { return }
                Task {
                    do { try await store.deleteNode(node); selection = nil }
                    catch { actionError = error.localizedDescription }
                }
            }
            if store.supportsNodeRecordRemoval {
                Button(L10n.text("仅移除管理记录"), role: .destructive) { showingRecordRemovalConfirmation = true }
                    .accessibilityIdentifier("nodes.remove-record")
            }
        } message: {
            Text(L10n.text("卸载并删除需要 SSH 连接。仅移除管理记录不需要 SSH，远端服务可能继续运行。"))
        }
        .alert(L10n.text("仅移除管理记录？"), isPresented: $showingRecordRemovalConfirmation) {
            Button(L10n.text("取消"), role: .cancel) {}
            Button(L10n.text("移除记录"), role: .destructive) {
                guard let node = deletionNode else { return }
                Task {
                    do { try await store.removeNodeRecord(node); selection = nil }
                    catch { actionError = error.localizedDescription }
                }
            }
            .accessibilityIdentifier("nodes.confirm-remove-record")
        } message: {
            Text(L10n.text("将从管理服务中移除「{0}」及其用户分配和待办，保留任务历史。此操作不会卸载远端服务；失联服务器上的服务可能仍在运行。", String(describing: (deletionNode?.name ?? ""))))
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
                DetailHeaderLayout {
                    nodeTitle(node)
                    nodeActions(node)
                }
                LazyVGrid(columns: Array(repeating: GridItem(.flexible(minimum: 0), spacing: 12, alignment: .leading), count: node.dnsBinding == nil ? 4 : 5), alignment: .leading, spacing: 8) {
                    nodeHeaderMetric(L10n.text("目标配置"), value: "v\(node.revision)")
                    nodeHeaderMetric(L10n.text("已部署配置"), value: node.deployedRevision.map { "v\($0)" } ?? L10n.text("尚未部署"))
                    nodeHeaderMetric(L10n.text("套餐有效期"), value: PackageDisplay.expiry(node.package))
                    if let binding = node.dnsBinding {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(L10n.text("节点域名")).font(.caption).foregroundStyle(.secondary)
                            Button {
                                store.showDNSRecord(binding.recordIds.first)
                            } label: {
                                Text(binding.hostname).lineLimit(1).truncationMode(.middle)
                            }
                            .buttonStyle(.link)
                            .font(.callout)
                            .help(L10n.text("{0} · 查看 DNS 记录", String(describing: (binding.hostname))))
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    nodeHeaderMetric(L10n.text("分配用户"), value: L10n.text("{0} 人", String(describing: (assignedUsers(node).count))))
                }
            }
            .padding(16)
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    OverviewColumnsLayout(wideColumns: 2, wideMinimum: 576, narrowColumns: 1) {
                        nodeConnections(node)
                        nodeTasksAndPackage(node)
                    }
                    GroupBox {
                        LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), alignment: .leading)], alignment: .leading, spacing: 12) {
                            nodeMetric(L10n.text("采样状态"), value: store.isConnected ? node.localizedFreshness : L10n.text("离线缓存"))
                            nodeMetric(L10n.text("最近采样"), value: DateDisplayText.local(node.lastSampleAt))
                            nodeMetric(L10n.text("统计缺口"), value: node.openGaps.map(String.init) ?? L10n.text("未知"))
                            nodeMetric(L10n.text("待撤权任务"), value: node.pendingRevocations.map(String.init) ?? L10n.text("未知"))
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.vertical, 4)
                    } label: { Label(L10n.text("采样与待办"), systemImage: "waveform.path") }
                    DisclosureGroup(L10n.text("分配用户（{0}）", String(describing: (assignedUsers(node).count)))) {
                        LazyVGrid(columns: [GridItem(.adaptive(minimum: 180), alignment: .leading)], alignment: .leading, spacing: 10) {
                            ForEach(assignedUsers(node)) { user in
                                HStack {
                                    Text(user.name)
                                    Spacer()
                                    Text(user.enabled ? L10n.text("启用") : L10n.text("已停用")).font(.caption).foregroundStyle(.secondary)
                                }
                            }
                            if assignedUsers(node).isEmpty { Text(L10n.text("尚未分配用户")).foregroundStyle(.secondary) }
                        }
                        .padding(.top, 8)
                    }
                    DisclosureGroup(L10n.text("节点记录")) {
                        VStack(alignment: .leading, spacing: 10) {
                            nodeMetric(L10n.text("节点 ID"), value: node.id)
                            if let detail, detail.id == node.id {
                                nodeMetric(L10n.text("创建时间"), value: DateDisplayText.local(detail.createdAt))
                                nodeMetric(L10n.text("更新时间"), value: DateDisplayText.local(detail.updatedAt))
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
            Button(L10n.text("SSH 测试")) { run(node, action: "ssh-test") }
                .disabled(operationInProgress(node))
            Menu(L10n.text("配置")) {
                Button(L10n.text("服务器配置")) { serverConfigurationNode = node }
                Button(L10n.text("代理配置")) { configurationNode = node }
            }.accessibilityIdentifier("node.configure.\(node.id)")
            Button(node.deployedRevision == nil ? L10n.text("部署") : L10n.text("同步")) {
                run(node, action: node.deployedRevision == nil ? "deploy" : "sync")
            }
            .disabled(operationInProgress(node))
            Menu(L10n.text("更多")) {
                Button(L10n.text("部署")) { run(node, action: "deploy") }.disabled(operationInProgress(node))
                Button(L10n.text("同步")) { run(node, action: "sync") }.disabled(operationInProgress(node))
                Button(L10n.text("回滚")) { run(node, action: "rollback") }.disabled(node.deployedRevision == nil || operationInProgress(node))
                Divider()
                Button(L10n.text("删除节点"), role: .destructive) { deletionNode = node; showingDeleteConfirmation = true }
                    .disabled(operationInProgress(node))
            }
        }
        .fixedSize()
        .disabled(!store.isConnected)
    }

    private func nodeConnections(_ node: NodeSummary) -> some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 12) {
                if let ssh = node.ssh {
                    nodeMetric(L10n.text("SSH 连接"), value: "\(ssh.username)@\(ssh.host):\(ssh.port)")
                    let credential = store.credentials.first { $0.id == ssh.credentialId }
                    nodeMetric(L10n.text("SSH 凭据"), value: "\(credential?.name ?? CredentialDisplay.kind(ssh.authType)) · v\(ssh.credentialVersion)")
                    DisclosureGroup(L10n.text("主机指纹")) {
                        Text(ssh.hostFingerprint ?? L10n.text("尚未确认"))
                            .font(.system(.caption, design: .monospaced)).textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading).padding(.top, 6)
                    }
                } else { Text(L10n.text("暂无 SSH 连接信息")).foregroundStyle(.secondary) }
                Divider()
                if let detail, detail.id == node.id {
                    nodeMetric(L10n.text("公开连接"), value: "\(detail.connection.host):\(detail.connection.port)")
                    nodeMetric(L10n.text("监听地址"), value: detail.connection.listenAddress)
                    if let sni = detail.connection.tlsSNI, !sni.isEmpty { nodeMetric("TLS SNI", value: sni) }
                } else if let detailError {
                    Text(L10n.text("无法读取公开连接：{0}", String(describing: (detailError)))).font(.caption).foregroundStyle(.orange)
                    Button(L10n.text("重试读取")) { Task { await loadDetail() } }.disabled(!store.isConnected)
                } else if store.isConnected {
                    ProgressView(L10n.text("读取公开连接…")).controlSize(.small)
                } else { Text(L10n.text("连接服务后可读取公开连接信息。")).font(.caption).foregroundStyle(.secondary) }
            }
            .frame(maxWidth: .infinity, alignment: .leading).padding(.vertical, 4)
        } label: { Label(L10n.text("连接信息"), systemImage: "network") }
    }

    private func nodeTasksAndPackage(_ node: NodeSummary) -> some View {
        VStack(alignment: .leading, spacing: 16) {
            nodeTasks(node)
            nodePackage(node)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func nodePackage(_ node: NodeSummary) -> some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 12) {
                QuotaProgressView(usageBytes: node.packageUsage?.usageBytes, quotaBytes: node.package?.quotaBytes)
                NodePackageStatus(package: node.package, usage: node.packageUsage)
            }
            .frame(maxWidth: .infinity, alignment: .leading).padding(.vertical, 4)
        } label: { Label(L10n.text("套餐与流量"), systemImage: "chart.bar") }
    }

    private func assignedUsers(_ node: NodeSummary) -> [UserSummary] {
        store.users.filter { $0.assignments.contains { $0.nodeID == node.id } }
    }

    private func nodeHeaderMetric(_ title: String, value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            Text(value).font(.callout).textSelection(.enabled)
                .help(value)
        }
        .lineLimit(1)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func nodeMetric(_ title: String, value: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            Text(value).font(.callout).textSelection(.enabled)
        }
    }

    private func relatedTasks(_ node: NodeSummary) -> [JobSummary] {
        NodeTaskFeedback.tasks(for: node.id, in: store.jobs)
    }

    private func pendingSubmission(_ node: NodeSummary) -> SubmittedNodeAction? {
        guard let submitted = submittedActions[node.id], !store.jobs.contains(where: { $0.id == submitted.jobID }) else { return nil }
        return submitted
    }

    private func operationInProgress(_ node: NodeSummary) -> Bool {
        submittingActions[node.id] != nil || pendingSubmission(node) != nil || relatedTasks(node).contains(where: NodeTaskFeedback.blocksOperation)
    }

    @ViewBuilder private func nodeTasks(_ node: NodeSummary) -> some View {
        let tasks = relatedTasks(node)
        let displayed = Array(tasks.prefix(3))
        let submitting = submittingActions[node.id]
        let submitted = pendingSubmission(node)
        if !tasks.isEmpty || submitting != nil || submitted != nil {
            GroupBox {
                VStack(alignment: .leading, spacing: 10) {
                    if !store.isConnected {
                        Label(L10n.text("离线快照"), systemImage: "wifi.slash")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    if let submitting {
                        HStack(spacing: 8) {
                            ProgressView().controlSize(.small)
                            Text(L10n.text("{0} · 正在提交", String(describing: (JobDisplayText.kind(submitting)))))
                        }
                        .font(.callout)
                        .accessibilityIdentifier("nodes.action.submitting")
                    } else if let submitted {
                        HStack(spacing: 8) {
                            Button(JobDisplayText.kind(submitted.action)) { onOpenJob(submitted.jobID) }
                                .buttonStyle(.link).fontWeight(.medium)
                            Text(JobDisplayText.status(submitted.status)).foregroundStyle(.blue)
                            Spacer(minLength: 8)
                            nodeTaskTime(submitted.submittedAt)
                        }
                        .font(.callout)
                        .accessibilityIdentifier("nodes.action.accepted")
                    }
                    ForEach(displayed) { job in
                        VStack(alignment: .leading, spacing: 4) {
                            HStack(spacing: 8) {
                                if NodeTaskFeedback.isActive(job) && store.isConnected {
                                    ProgressView().controlSize(.small)
                                }
                                Button(JobDisplayText.kind(job.kind)) { onOpenJob(job.id) }
                                    .buttonStyle(.link).fontWeight(.medium)
                                    .lineLimit(1).help(JobDisplayText.kind(job.kind))
                                    .accessibilityLabel(L10n.text("查看{0}任务详情", String(describing: (JobDisplayText.kind(job.kind)))))
                                    .accessibilityIdentifier("nodes.job.\(job.id)")
                                Text(JobDisplayText.status(job.status)).foregroundStyle(taskColor(job))
                                    .fixedSize()
                                Spacer(minLength: 8)
                                nodeTaskTime(DateDisplayText.parse(job.createdAt))
                            }
                            .font(.callout)
                            if let message = job.errorMessage, !message.isEmpty {
                                Text(message).font(.caption).foregroundStyle(.orange)
                                    .lineLimit(1).help(message).textSelection(.enabled)
                            } else if NodeTaskFeedback.isActive(job) {
                                Text(JobDisplayText.stage(job.stage)).font(.caption).foregroundStyle(.secondary)
                                    .lineLimit(1).help(JobDisplayText.stage(job.stage))
                            }
                        }
                    }
                    if tasks.count > displayed.count {
                        Text(L10n.text("另有 {0} 项任务", String(describing: (tasks.count - displayed.count))))
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.vertical, 4)
            } label: {
                Label(L10n.text("当前与最近任务"), systemImage: "list.bullet.rectangle")
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityIdentifier("nodes.tasks")
        }
    }

    private func nodeTaskTime(_ date: Date?) -> some View {
        Text(date.map { $0.formatted(.dateTime.locale(L10n.locale).month(.twoDigits).day(.twoDigits).hour(.twoDigits(amPM: .omitted)).minute(.twoDigits)) } ?? "—")
            .font(.caption).monospacedDigit().foregroundStyle(.secondary)
            .fixedSize()
            .help(date.map { L10n.text("提交时间：{0}", String(describing: (L10n.date($0, date: .complete, time: .standard)))) } ?? L10n.text("提交时间未知"))
    }

    private func taskColor(_ job: JobSummary) -> Color {
        switch job.status {
        case "succeeded": .green
        case "queued", "running": .blue
        case "failed", "rolled_back": .orange
        default: .secondary
        }
    }

    private func run(_ node: NodeSummary, action: String) {
        guard store.isConnected, !operationInProgress(node) else { return }
        let service = store.serviceAddress
        let submittedAt = Date()
        submittingActions[node.id] = action
        submittedActions[node.id] = nil
        Task {
            do {
                let receipt = try await store.runNodeAction(node, action: action)
                guard store.serviceAddress == service else { return }
                submittedActions[node.id] = SubmittedNodeAction(action: action, jobID: receipt.jobId, status: receipt.status, submittedAt: submittedAt)
                submittingActions[node.id] = nil
                await store.refresh()
            } catch {
                guard store.serviceAddress == service else { return }
                submittingActions[node.id] = nil
                actionError = error.localizedDescription
            }
        }
    }

    private struct SubmittedNodeAction {
        let action: String
        let jobID: String
        let status: String
        let submittedAt: Date
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
            "new": L10n.text("未部署"),
            "needs_fingerprint": L10n.text("待确认指纹"),
            "fingerprint_changed": L10n.text("指纹已变更"),
            "ready": L10n.text("待部署"),
            "syncing": L10n.text("同步中"),
            "deployed": L10n.text("已部署"),
            "rolled_back": L10n.text("已回滚"),
            "sync_failed": L10n.text("同步失败"),
            "rollback_failed": L10n.text("回滚失败"),
            "drift": L10n.text("远端配置已变更"),
            "unreachable": L10n.text("无法连接"),
            "deleting": L10n.text("卸载中"),
            "delete_failed": L10n.text("卸载失败"),
        ]
        return labels[state] ?? state
    }

    var localizedFreshness: String {
        switch dataFreshness {
        case .some("fresh"): L10n.text("统计正常")
        case .some("stale"): L10n.text("采样已陈旧")
        case .some("not_collected"): L10n.text("尚未采集")
        case .none: "—"
        case .some(_): L10n.text("统计状态未知")
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
            Text(L10n.text("添加节点")).font(.title.bold())
            if createdNodeID != nil {
                ContentUnavailableView(L10n.text("节点已创建"), systemImage: "checkmark.circle", description: Text(L10n.text("节点认证令牌：\n{0}\n请将令牌保存在安全位置，并在代理配置页设置 TLS 证书后再部署。", String(describing: (createdToken ?? L10n.text("此前请求已创建节点，令牌不会重复显示。"))))))
                HStack { Spacer(); Button(L10n.text("完成")) { dismiss() }.keyboardShortcut(.defaultAction) }
            } else {
                Form {
                    Section(L10n.text("SSH 连接")) {
                        TextField(L10n.text("节点名称"), text: $name)
                            .accessibilityLabel(L10n.text("节点名称"))
                            .accessibilityIdentifier("node.create.name")
                        TextField(L10n.text("SSH 地址"), text: $sshHost)
                            .onChange(of: sshHost) { oldValue, newValue in
                                if publicHost.isEmpty || publicHost == oldValue {
                                    publicHost = newValue
                                }
                            }
                            .accessibilityLabel(L10n.text("SSH 地址"))
                            .accessibilityIdentifier("node.create.sshHost")
                        TextField(L10n.text("SSH 端口"), text: $sshPort)
                            .accessibilityLabel(L10n.text("SSH 端口"))
                            .accessibilityIdentifier("node.create.sshPort")
                        TextField(L10n.text("SSH 用户"), text: $sshUsername)
                            .accessibilityLabel(L10n.text("SSH 用户"))
                            .accessibilityIdentifier("node.create.sshUsername")
                        CredentialPickerView(store: store, selection: $sshCredentialId, kinds: ["ssh_private_key", "ssh_password"], title: L10n.text("SSH 凭据"))
                    }
                    Section(L10n.text("有效期与流量套餐")) {
                        if store.supportsNodePackages {
                            NodePackageFields(draft: $packageDraft)
                            if packageDraft.hasQuota { TextField(L10n.text("已有用量（GB）"), text: $initialUsageGB) }
                        } else { Text(L10n.text("升级管理服务后可设置有效期和流量套餐。")).foregroundStyle(.secondary) }
                    }
                    Section(L10n.text("公开连接")) {
                        if store.supportsDNSManagement {
                            DNSAllocationFields(store: store, draft: $dnsDraft, sshHost: sshHost)
                        }
                        if dnsDraft.mode == "external" {
                            TextField(L10n.text("公开地址"), text: $publicHost)
                                .accessibilityLabel(L10n.text("公开地址"))
                                .accessibilityIdentifier("node.create.publicHost")
                        }
                        TextField(L10n.text("公开端口"), text: $publicPort)
                            .accessibilityLabel(L10n.text("公开端口"))
                            .accessibilityIdentifier("node.create.publicPort")
                        Text(dnsDraft.mode == "external" ? L10n.text("公开地址默认跟随 SSH 地址，可手动修改。公开端口用于初始化监听端口；创建后可在代理配置中设置端口联动、TLS 证书及其他 Hysteria 参数。") : L10n.text("公开地址使用所分配的域名。公开端口用于初始化监听端口；创建后在代理配置中设置 TLS 证书及其他 Hysteria 参数。"))
                            .font(.callout).foregroundStyle(.secondary)
                    }
                }
                .formStyle(.grouped)
                if let errorMessage { Text(errorMessage).foregroundStyle(.red).font(.callout) }
                HStack {
                    Button(L10n.text("取消")) { dismiss() }.keyboardShortcut(.cancelAction)
                    Spacer()
                    Button(isSaving ? L10n.text("正在保存…") : L10n.text("创建节点")) { save() }
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
            errorMessage = L10n.text("SSH 和公开端口都必须是 1 到 65535 的整数。")
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
