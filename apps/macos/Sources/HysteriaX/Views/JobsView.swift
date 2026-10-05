import SwiftUI

enum JobDisplayText {
    static func kind(_ value: String) -> String {
        switch value {
        case "ssh-test": "SSH 测试"
        case "credential-apply": "应用凭据"
        case "deploy": "部署"
        case "sync": "同步"
        case "rollback": "回滚"
        case "kick": "断开客户端"
        case "uninstall": "卸载"
        default: value
        }
    }

    static func status(_ value: String) -> String {
        switch value {
        case "queued": "排队中"
        case "running": "执行中"
        case "succeeded": "成功"
        case "failed": "失败"
        case "rolled_back": "已回滚"
        case "cancelled": "已取消"
        default: value
        }
    }

    static func stage(_ value: String) -> String {
        let labels = [
            "credential_applied": "凭据已应用",
            "credential_deployment_queued": "凭据部署已排队",
            "credential_revocation_queued": "连接撤销已排队",
            "queued": "排队中",
            "running": "执行中",
            "starting": "准备中",
            "retry_wait": "等待重试",
            "waiting_recovery": "等待节点 SSH 恢复",
            "needs_attention": "需要人工处理",
            "restriction_cleared": "限制已解除，跳过断开",
            "recovered": "重启后恢复",
            "loading_connection": "读取 SSH 连接",
            "connecting": "连接节点",
            "checking_environment": "检查节点环境",
            "loading_revision": "读取配置版本",
            "resolving_resources": "检查配置资源",
            "checking_drift": "检查配置漂移",
            "downloading_release": "下载并校验程序",
            "rendering_configuration": "生成服务配置",
            "uploading_files": "上传部署文件",
            "installing_service": "安装系统服务",
            "checking_health": "检查服务健康状态",
            "health_checked": "健康检查通过",
            "rolling_back": "恢复上一版本",
            "rolled_back": "已回滚",
            "rollback_failed": "回滚失败",
            "fingerprint_confirmation_required": "等待确认 SSH 指纹",
            "environment_checked": "环境检查通过",
            "kicking_clients": "断开客户端",
            "checking_clients": "检查客户端状态",
            "clients_offline": "客户端已下线",
            "uninstalling_service": "卸载系统服务",
            "remote_uninstalled": "远端卸载完成",
            "node_deleting": "删除节点中",
            "node_removed": "管理记录已移除",
            "superseded": "已被新任务替代",
            "failed": "失败",
        ]
        return labels[value] ?? value
    }

    static func logMessage(stage: String, message: String) -> String {
        if stage == "checking_clients",
           let count = message.split(separator: " ").first,
           let number = Int(count) {
            return "还有 \(number) 台设备在线，正在再次请求断开。"
        }
        let messages = [
            "Loading the saved SSH connection.": "正在读取保存的 SSH 连接。",
            "Connecting to the node and checking its SSH host key.": "正在连接节点并验证 SSH 主机指纹。",
            "Checking the operating system, architecture, systemd, and sudo access.": "正在检查操作系统、架构、systemd 和 sudo 权限。",
            "Loading the target configuration revision and encrypted resources.": "正在加载目标配置版本和加密资源。",
            "Resolving and validating the configuration resource references.": "正在解析并验证配置资源引用。",
            "Checking the operating system, architecture, systemd, sudo access, disk space, and listener requirements.": "正在检查系统版本、架构、systemd、sudo、磁盘空间和监听端口要求。",
            "Comparing the remote configuration with the last successfully deployed version.": "正在将远端配置与上次成功部署版本比较。",
            "Downloading and verifying the pinned Hysteria release asset.": "正在下载并校验固定版本的 Hysteria 程序。",
            "Rendering the server configuration and systemd unit.": "正在生成服务端配置和 systemd 单元文件。",
            "Uploading the verified binary, configuration, unit, and referenced resources.": "正在上传已校验程序、配置、服务文件和引用资源。",
            "Installing the managed systemd service and applying the new configuration.": "正在安装托管的 systemd 服务并应用配置。",
            "Waiting for the traffic and online statistics APIs to become healthy.": "正在等待流量统计和在线状态接口就绪。",
            "The new service did not become healthy; restoring the previous successful configuration.": "新服务未通过健康检查，正在恢复上一成功配置。",
            "Requesting that the node disconnect the user's active client sessions.": "正在请求节点断开该用户的在线客户端。",
            "The node reports no active client devices for this user.": "节点已确认该用户没有在线客户端。",
            "Connecting to the node and verifying HysteriaX ownership before removal.": "正在连接节点并验证 HysteriaX 所有权。",
            "Checking remote systemd and the managed-install marker.": "正在检查远端 systemd 和托管安装标记。",
            "Stopping the managed service and removing its files and service account.": "正在停止托管服务并移除文件和服务账户。",
        ]
        return messages[message] ?? message
    }
}

enum DateDisplayText {
    static func parse(_ value: String?) -> Date? { DateDisplayParser.shared.parse(value) }

    static func local(_ value: String?) -> String {
        guard let value else { return "—" }
        guard let date = parse(value) else { return value }
        return date.formatted(date: .abbreviated, time: .shortened)
    }
}

private extension JobSummary {
    var displayNodeName: String { nodeName ?? nodeID ?? "—" }
    var localizedKind: String { JobDisplayText.kind(kind) }
    var localizedStatus: String { JobDisplayText.status(status) }
    var localizedStage: String { JobDisplayText.stage(stage) }
}

struct JobsView: View {
    @Bindable var store: ManagementStore
    var initialSelection: String? = nil
    var onInitialSelectionHandled: () -> Void = {}
    @State private var selectedJobID: String?
    @State private var searchText = ""
    @State private var sortOrder = [KeyPathComparator(\JobSummary.createdAt, order: .reverse)]
    @State private var selectedJobDetail: JobDetailResponse?
    @State private var isLoadingJobDetail = false
    @State private var confirmedFingerprintJobID: String?
    @State private var actionError: String?

    private var visibleJobs: [JobSummary] {
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        let filtered = store.jobs.filter { job in
            query.isEmpty
                || job.id.localizedCaseInsensitiveContains(query)
                || job.kind.localizedCaseInsensitiveContains(query)
                || job.stage.localizedCaseInsensitiveContains(query)
                || job.status.localizedCaseInsensitiveContains(query)
                || (job.nodeName?.localizedCaseInsensitiveContains(query) ?? false)
                || (job.nodeID?.localizedCaseInsensitiveContains(query) ?? false)
                || (job.errorMessage?.localizedCaseInsensitiveContains(query) ?? false)
        }
        return filtered.sorted(using: sortOrder)
    }

    private var selectedJob: JobSummary? {
        visibleJobs.first(where: { $0.id == selectedJobID }) ?? (selectedJobDetail?.job.id == selectedJobID ? selectedJobDetail?.job : nil)
    }

    private var detailRequestKey: String {
        guard let selectedJobID else { return "none" }
        let updatedAt = store.jobs.first { $0.id == selectedJobID }?.updatedAt ?? "detail"
        return "\(selectedJobID):\(updatedAt):\(store.isConnected)"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Table(visibleJobs, selection: $selectedJobID, sortOrder: $sortOrder) {
                TableColumn("节点", value: \.displayNodeName)
                TableColumn("类型", value: \.localizedKind)
                TableColumn("阶段", value: \.localizedStage)
                TableColumn("状态", value: \.localizedStatus)
                TableColumn("创建时间") { job in Text(DateDisplayText.local(job.createdAt)) }
            }
            .frame(minHeight: 180)
            .task(id: initialSelection) {
                guard let initialSelection else { return }
                searchText = ""
                selectedJobID = initialSelection
                onInitialSelectionHandled()
            }
            .overlay {
                if store.jobs.isEmpty {
                    ContentUnavailableView("暂无任务", systemImage: "list.bullet.rectangle", description: Text("发起节点操作后，任务会显示在这里。"))
                } else if visibleJobs.isEmpty {
                    ContentUnavailableView("没有匹配的任务", systemImage: "magnifyingglass")
                }
            }
            if let job = selectedJob {
                Divider()
                jobDetailPane(job)
                    .id(job.id)
                    .frame(maxHeight: 400)
                    .accessibilityIdentifier("jobs.detail")
            }
        }
        .searchable(text: $searchText, prompt: "搜索任务")
        .onChange(of: searchText) { _, _ in selectedJobID = nil }
        .task(id: detailRequestKey) { await loadSelectedJobDetail() }
        .alert("任务操作失败", isPresented: Binding(get: { actionError != nil }, set: { if !$0 { actionError = nil } })) {
            Button("好", role: .cancel) { actionError = nil }
        } message: { Text(actionError ?? "") }
    }

    private func jobDetailPane(_ job: JobSummary) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 14) {
                ViewThatFits(in: .horizontal) {
                    HStack(alignment: .top, spacing: 16) {
                        jobTitle(job)
                        Spacer(minLength: 20)
                        jobActions(job)
                    }
                    VStack(alignment: .leading, spacing: 10) {
                        jobTitle(job)
                        jobActions(job)
                    }
                }
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), alignment: .leading)], alignment: .leading, spacing: 12) {
                    jobMetric("当前阶段", value: JobDisplayText.stage(job.result?.stage ?? job.stage))
                    jobMetric("尝试次数", value: String(job.attempts))
                    jobMetric("创建时间", value: DateDisplayText.local(job.createdAt))
                    jobMetric("开始时间", value: DateDisplayText.local(currentDetail(job)?.startedAt))
                }
            }
            .padding(16)
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    if let error = job.errorMessage {
                        GroupBox {
                            Text(error).foregroundStyle(.orange).textSelection(.enabled)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.vertical, 4)
                        } label: {
                            Label("失败原因", systemImage: "exclamationmark.triangle")
                        }
                    }
                    if let probe = job.result?.result?.proxyProbe {
                        GroupBox("客户端探测") {
                            Text(proxyProbeSummary(probe))
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.vertical, 4)
                        }
                    }
                    fingerprintResult(job)
                    if job.retryOfJobId != nil {
                        Label("此任务为关联重试，成功后会移除原失败提醒。", systemImage: "arrow.triangle.2.circlepath")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    executionRecords(job)
                    DisclosureGroup("任务标识") {
                        VStack(alignment: .leading, spacing: 10) {
                            jobMetric("任务 ID", value: job.id)
                            if let nodeID = job.nodeID { jobMetric("节点 ID", value: nodeID) }
                        }
                        .padding(.top, 8)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
                .padding(16)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    private func jobTitle(_ job: JobSummary) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            VStack(alignment: .leading, spacing: 4) {
                Text(JobDisplayText.kind(job.kind)).font(.headline)
                Text(job.nodeName ?? job.nodeID ?? "未关联节点")
                    .font(.callout).foregroundStyle(.secondary).lineLimit(2).textSelection(.enabled)
            }
            Text(JobDisplayText.status(job.status))
                .font(.caption.weight(.medium))
                .foregroundStyle(jobStatusColor(job.status))
                .padding(.horizontal, 8).padding(.vertical, 3)
                .background(jobStatusColor(job.status).opacity(0.1), in: Capsule())
        }
    }

    @ViewBuilder private func jobActions(_ job: JobSummary) -> some View {
        HStack(spacing: 8) {
            if let retryID = job.retryJobId {
                Button("查看重试任务") { selectedJobID = retryID }
                    .accessibilityIdentifier("jobs.retryLink.\(job.id)")
            }
            if let action = retryAction(for: job),
               let node = store.nodes.first(where: { $0.id == job.nodeID }) {
                Button("重试：\(retryLabel(action))") { retry(job, on: node, action: action) }
                    .accessibilityIdentifier("jobs.retry.\(job.id)")
                    .disabled(!store.isConnected || !store.supportsJobRetryLinks || ["needs_fingerprint", "fingerprint_changed"].contains(node.state))
                    .help(store.supportsJobRetryLinks ? "创建关联重试，成功后自动移除失败提醒" : "管理服务需更新后才能关联重试")
            }
        }
        .fixedSize()
    }

    @ViewBuilder private func fingerprintResult(_ job: JobSummary) -> some View {
        if let change = SSHHostFingerprintChange(job: job) {
            SSHHostFingerprintChangeView(store: store, job: job, change: change)
                .id("\(store.serviceAddress):\(job.id)")
        }
        if let fingerprint = job.result?.result?.fingerprint {
            GroupBox("SSH 主机指纹") {
                VStack(alignment: .leading, spacing: 10) {
                    Text(fingerprint).font(.system(.callout, design: .monospaced)).textSelection(.enabled)
                    Text("首次测试只读取主机密钥，尚未发送 SSH 凭据。核对后选择信任并保存，之后的连接才会认证。")
                        .font(.callout).foregroundStyle(.secondary)
                    if confirmedFingerprintJobID != job.id,
                       let nodeID = job.nodeID,
                       let node = store.nodes.first(where: { $0.id == nodeID }),
                       node.state == "needs_fingerprint" {
                        Button("信任并保存此指纹") {
                            Task {
                                do {
                                    try await store.confirmHostFingerprint(nodeID: nodeID, fingerprint: fingerprint)
                                    confirmedFingerprintJobID = job.id
                                } catch { actionError = error.localizedDescription }
                            }
                        }
                        .disabled(!store.isConnected)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.vertical, 4)
            }
        }
    }

    private func executionRecords(_ job: JobSummary) -> some View {
        GroupBox {
            if isLoadingJobDetail {
                ProgressView("读取任务记录…").controlSize(.small)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.vertical, 8)
            } else if let detail = currentDetail(job), !detail.logs.isEmpty {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(detail.logs.indices, id: \.self) { index in
                        let entry = detail.logs[index]
                        HStack(alignment: .top, spacing: 12) {
                            Text(String(index + 1))
                                .font(.caption.monospacedDigit()).foregroundStyle(.secondary)
                                .frame(width: 24, height: 24)
                                .background(.quaternary, in: Circle())
                            VStack(alignment: .leading, spacing: 6) {
                                HStack(alignment: .firstTextBaseline, spacing: 10) {
                                    Text(JobDisplayText.stage(entry["stage"]?.stringValue ?? ""))
                                        .font(.callout.weight(.medium))
                                    if let attempt = entry["attempt"]?.integerValue {
                                        Text("第 \(attempt) 次尝试").font(.caption).foregroundStyle(.secondary)
                                    }
                                    Spacer(minLength: 8)
                                    Text(DateDisplayText.local(entry["created_at"]?.stringValue))
                                        .font(.caption).foregroundStyle(.secondary)
                                }
                                Text(JobDisplayText.logMessage(
                                    stage: entry["stage"]?.stringValue ?? "",
                                    message: entry["message"]?.stringValue ?? ""
                                ))
                                .font(.callout).textSelection(.enabled)
                            }
                        }
                        .padding(.vertical, 10)
                        if index < detail.logs.count - 1 { Divider().padding(.leading, 36) }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                Text(store.isConnected ? "暂无执行记录" : "服务断开时保留任务摘要；恢复连接后可读取执行记录。")
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.vertical, 8)
            }
        } label: {
            Label("执行记录", systemImage: "list.bullet.rectangle")
        }
    }

    private func currentDetail(_ job: JobSummary) -> JobDetailResponse? {
        selectedJobDetail?.job.id == job.id ? selectedJobDetail : nil
    }

    private func jobMetric(_ title: String, value: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            Text(value).font(.callout).textSelection(.enabled)
        }
    }

    private func jobStatusColor(_ status: String) -> Color {
        switch status {
        case "succeeded": .green
        case "failed": .orange
        case "running": .blue
        default: .secondary
        }
    }

    private func loadSelectedJobDetail() async {
        guard let selectedJobID, store.isConnected else {
            selectedJobDetail = nil
            return
        }
        isLoadingJobDetail = true
        defer { isLoadingJobDetail = false }
        do {
            selectedJobDetail = try await store.jobDetail(selectedJobID)
        } catch {
            selectedJobDetail = nil
            actionError = error.localizedDescription
        }
    }

    private func retryAction(for job: JobSummary) -> String? {
        guard (job.status == "failed" || (["cancelled", "rolled_back"].contains(job.status) && job.retryOfJobId != nil)), job.nodeID != nil, job.retryJobId == nil else { return nil }
        switch job.kind {
        case "ssh-test": return "ssh-test"
        case "deploy", "sync": return "sync"
        case "rollback": return "rollback"
        case "kick": return "kick"
        default: return nil
        }
    }

    private func retryLabel(_ action: String) -> String {
        switch action {
        case "ssh-test": "SSH 测试"
        case "rollback": "回滚"
        case "kick": "踢下线"
        default: "同步"
        }
    }

    private func retry(_ job: JobSummary, on node: NodeSummary, action: String) {
        Task {
            do {
                try await store.retryJob(job, on: node)
                selectedJobID = nil
            } catch { actionError = error.localizedDescription }
        }
    }

    private func proxyProbeSummary(_ probe: [String: JSONValue]) -> String {
        guard probe["status"]?.stringValue == "passed" else { return "未通过" }
        switch probe["route_check"]?.stringValue {
        case "tcp_forwarding": return "TCP 转发已验证"
        case "custom_tcp_forwarding": return "自定义 HTTP 目标转发已验证"
        case "authenticated_session": return "客户端已认证；自定义路由下未验证目标转发"
        default: return "客户端连接已验证"
        }
    }
}
