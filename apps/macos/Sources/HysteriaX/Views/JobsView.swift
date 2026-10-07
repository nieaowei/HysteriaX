import SwiftUI

enum DateDisplayText {
    static func parse(_ value: String?) -> Date? { DateDisplayParser.shared.parse(value) }

    static func local(_ value: String?) -> String {
        guard let value else { return "—" }
        guard let date = parse(value) else { return value }
        return L10n.date(date, date: .abbreviated, time: .shortened)
    }
}

private extension JobSummary {
    var displayNodeName: String { resourceName ?? nodeName ?? nodeID ?? "—" }
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
        MainVerticalSplitView(hasDetail: selectedJob != nil) {
            Table(visibleJobs, selection: $selectedJobID, sortOrder: $sortOrder) {
                TableColumn(L10n.text("节点"), value: \.displayNodeName)
                TableColumn(L10n.text("类型"), value: \.localizedKind)
                TableColumn(L10n.text("阶段"), value: \.localizedStage)
                TableColumn(L10n.text("状态"), value: \.localizedStatus)
                TableColumn(L10n.text("创建时间")) { job in Text(DateDisplayText.local(job.createdAt)) }
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
                    ContentUnavailableView(L10n.text("暂无任务"), systemImage: "list.bullet.rectangle", description: Text(L10n.text("发起节点操作后，任务会显示在这里。")))
                } else if visibleJobs.isEmpty {
                    ContentUnavailableView(L10n.text("没有匹配的任务"), systemImage: "magnifyingglass")
                }
            }
        } detail: {
            if let job = selectedJob {
                jobDetailPane(job)
                    .id(job.id)
                    .accessibilityIdentifier("jobs.detail")
            }
        }
        .searchable(text: $searchText, prompt: L10n.text("搜索任务"))
        .onChange(of: searchText) { _, _ in selectedJobID = nil }
        .task(id: detailRequestKey) { await loadSelectedJobDetail() }
        .alert(L10n.text("任务操作失败"), isPresented: Binding(get: { actionError != nil }, set: { if !$0 { actionError = nil } })) {
            Button(L10n.text("好"), role: .cancel) { actionError = nil }
        } message: { Text(JobDisplayText.errorMessage(actionError ?? "")) }
    }

    private func jobDetailPane(_ job: JobSummary) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 14) {
                DetailHeaderLayout {
                    jobTitle(job)
                    jobActions(job)
                }
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), alignment: .leading)], alignment: .leading, spacing: 12) {
                    jobMetric(L10n.text("当前阶段"), value: JobDisplayText.stage(job.result?.stage ?? job.stage))
                    jobMetric(L10n.text("尝试次数"), value: String(job.attempts))
                    jobMetric(L10n.text("创建时间"), value: DateDisplayText.local(job.createdAt))
                    jobMetric(L10n.text("开始时间"), value: DateDisplayText.local(currentDetail(job)?.startedAt))
                }
            }
            .padding(16)
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    if let error = job.errorMessage {
                        GroupBox {
                            Text(JobDisplayText.errorMessage(error)).foregroundStyle(.orange).textSelection(.enabled)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.vertical, 4)
                        } label: {
                            Label(L10n.text("失败原因"), systemImage: "exclamationmark.triangle")
                        }
                    }
                    if let probe = job.result?.result?.proxyProbe {
                        GroupBox(L10n.text("客户端探测")) {
                            Text(proxyProbeSummary(probe))
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.vertical, 4)
                        }
                    }
                    fingerprintResult(job)
                    if job.retryOfJobId != nil {
                        Label(L10n.text("此任务为关联重试，成功后会移除原失败提醒。"), systemImage: "arrow.triangle.2.circlepath")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    executionRecords(job)
                    DisclosureGroup(L10n.text("任务标识")) {
                        VStack(alignment: .leading, spacing: 10) {
                            jobMetric(L10n.text("任务 ID"), value: job.id)
                            if let nodeID = job.nodeID { jobMetric(L10n.text("节点 ID"), value: nodeID) }
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
                Text(job.resourceName ?? job.nodeName ?? job.nodeID ?? L10n.text("未关联节点"))
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
                Button(L10n.text("查看重试任务")) { selectedJobID = retryID }
                    .accessibilityIdentifier("jobs.retryLink.\(job.id)")
            }
            if job.kind.hasPrefix("dns-"), job.kind != "dns-credential-apply", job.status == "failed", job.retryJobId == nil {
                Button(L10n.text("重试 DNS 操作")) {
                    Task { do { try await store.retryDNSJob(job) } catch { actionError = error.localizedDescription } }
                }.disabled(!store.isConnected || !store.supportsDNSManagement)
            }
            if let action = retryAction(for: job),
               let node = store.nodes.first(where: { $0.id == job.nodeID }) {
                Button(L10n.text("重试：{0}", String(describing: (retryLabel(action))))) { retry(job, on: node, action: action) }
                    .accessibilityIdentifier("jobs.retry.\(job.id)")
                    .disabled(!store.isConnected || !store.supportsJobRetryLinks || ["needs_fingerprint", "fingerprint_changed"].contains(node.state))
                    .help(store.supportsJobRetryLinks ? L10n.text("创建关联重试，成功后自动移除失败提醒") : L10n.text("管理服务需更新后才能关联重试"))
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
            GroupBox(L10n.text("SSH 主机指纹")) {
                VStack(alignment: .leading, spacing: 10) {
                    Text(fingerprint).font(.system(.callout, design: .monospaced)).textSelection(.enabled)
                    Text(L10n.text("首次测试只读取主机密钥，尚未发送 SSH 凭据。核对后选择信任并保存，之后的连接才会认证。"))
                        .font(.callout).foregroundStyle(.secondary)
                    if confirmedFingerprintJobID != job.id,
                       let nodeID = job.nodeID,
                       let node = store.nodes.first(where: { $0.id == nodeID }),
                       node.state == "needs_fingerprint" {
                        Button(L10n.text("信任并保存此指纹")) {
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
                ProgressView(L10n.text("读取任务记录…")).controlSize(.small)
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
                                        Text(L10n.text("第 {0} 次尝试", String(describing: (attempt)))).font(.caption).foregroundStyle(.secondary)
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
                Text(store.isConnected ? L10n.text("暂无执行记录") : L10n.text("服务断开时保留任务摘要；恢复连接后可读取执行记录。"))
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.vertical, 8)
            }
        } label: {
            Label(L10n.text("执行记录"), systemImage: "list.bullet.rectangle")
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
        case "ssh-test": L10n.text("SSH 测试")
        case "rollback": L10n.text("回滚")
        case "kick": L10n.text("踢下线")
        default: L10n.text("同步")
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
        guard probe["status"]?.stringValue == "passed" else { return L10n.text("未通过") }
        switch probe["route_check"]?.stringValue {
        case "tcp_forwarding": return L10n.text("TCP 转发已验证")
        case "custom_tcp_forwarding": return L10n.text("自定义 HTTP 目标转发已验证")
        case "authenticated_session": return L10n.text("客户端已认证；自定义路由下未验证目标转发")
        default: return L10n.text("客户端连接已验证")
        }
    }
}
