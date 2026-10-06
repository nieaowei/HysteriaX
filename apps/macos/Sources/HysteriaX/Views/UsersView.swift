import AppKit
import SwiftUI
import UniformTypeIdentifiers

struct UsersView: View {
    @Bindable var store: ManagementStore
    var initialSelection: String? = nil
    var onInitialSelectionHandled: () -> Void = {}
    @State private var showingAddUser = false
    @State private var assignmentUser: UserSummary?
    @State private var showingEditUser = false
    @State private var selectedUserID: String?
    @State private var searchText = ""
    @State private var sortOrder = [KeyPathComparator(\UserSummary.name)]
    @State private var assignmentTarget: AssignmentTarget?
    @State private var showingDeleteConfirmation = false
    @State private var alertTitle = ""
    @State private var alertMessage: String?
    @State private var selectedUsage: UserUsageResponse?
    @State private var usageErrorMessage: String?
    @State private var usageLoadingUserID: String?

    private var visibleUsers: [UserSummary] {
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        let filtered = store.users.filter { user in
            query.isEmpty
                || user.name.localizedStandardContains(query)
                || user.id.localizedCaseInsensitiveContains(query)
                || user.assignments.contains { $0.nodeID.localizedCaseInsensitiveContains(query) }
        }
        return filtered.sorted(using: sortOrder)
    }

    var body: some View {
        MainVerticalSplitView(hasDetail: store.users.contains { $0.id == selectedUserID }) {
            Table(visibleUsers, selection: $selectedUserID, sortOrder: $sortOrder) {
                TableColumn("名称", value: \.name) { user in
                    Text(user.name)
                        .accessibilityLabel(user.name)
                        .accessibilityIdentifier("users.row.\(user.id)")
                }
                TableColumn("状态") { user in
                    Text(user.enabled ? "启用" : "已停用")
                        .accessibilityLabel(user.enabled ? "启用" : "已停用")
                        .accessibilityIdentifier("users.status.\(user.id)")
                }
                TableColumn("用量 / 额度") { user in
                    QuotaProgressView(usageBytes: user.usageBytes, quotaBytes: user.quotaBytes)
                }
                .width(min: 180, ideal: 220)
                TableColumn("节点") { user in Text("\(user.assignments.count)") }
                TableColumn("到期") { user in Text(user.expiresAt.map { DateDisplayText.local($0) } ?? "不限") }
            }
            .frame(minHeight: 180)
            .task(id: initialSelection) {
                guard let initialSelection else { return }
                searchText = ""
                selectedUserID = initialSelection
                onInitialSelectionHandled()
            }
            .overlay {
                if store.users.isEmpty {
                    ContentUnavailableView("还没有用户", systemImage: "person.2", description: Text("添加用户后可以分配节点并生成订阅。"))
                } else if visibleUsers.isEmpty {
                    ContentUnavailableView("没有匹配的用户", systemImage: "magnifyingglass")
                }
            }
        } detail: {
            if let user = store.users.first(where: { $0.id == selectedUserID }) {
                userDetailPane(user)
                    .id(user.id)
                    .accessibilityIdentifier("users.detail")
            }
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button { showingAddUser = true } label: { Label("添加用户", systemImage: "plus") }
                    .keyboardShortcut("n", modifiers: .command)
                    .disabled(!store.isConnected)
            }
        }
        .searchable(text: $searchText, prompt: "搜索用户")
        .onChange(of: searchText) { _, _ in selectedUserID = nil }
        .onChange(of: selectedUserID) { _, _ in
            selectedUsage = nil
            usageErrorMessage = nil
            usageLoadingUserID = nil
        }
        .task(id: "\(selectedUserID ?? "")|\(store.isConnected)") {
            guard store.isConnected, let selectedUserID else {
                selectedUsage = nil
                usageErrorMessage = nil
                usageLoadingUserID = nil
                return
            }
            while !Task.isCancelled {
                await loadUsage(selectedUserID)
                try? await Task.sleep(for: .seconds(15))
            }
        }
        .sheet(isPresented: $showingAddUser) { UserFormView(store: store) }
        .sheet(isPresented: $showingEditUser) {
            if let user = store.users.first(where: { $0.id == selectedUserID }) {
                UserEditFormView(store: store, user: user)
            }
        }
        .sheet(item: $assignmentUser) { user in
            UserNodeAssignmentsView(store: store, user: user)
        }
        .sheet(item: $assignmentTarget) { target in
            UserAssignmentFormView(store: store, user: target.user, node: target.node, isUpdating: target.isUpdating) { credential in
                if let credential {
                    copy(credential, message: "\(target.node.name) 的连接密码已复制。请立即保存；服务端不会再次显示明文。")
                } else {
                    alertTitle = "mTLS 证书已更新"
                    alertMessage = "新的客户端证书和私钥已加密保存，并已排队断开现有连接。"
                }
            }
        }
        .confirmationDialog("删除用户？", isPresented: $showingDeleteConfirmation, titleVisibility: .visible) {
            Button("删除用户", role: .destructive) {
                if let user = store.users.first(where: { $0.id == selectedUserID }) { delete(user) }
            }
        } message: {
            Text("用户的订阅和节点凭据会撤销，并排队断开在线设备。")
        }
        .alert(alertTitle, isPresented: Binding(get: { alertMessage != nil }, set: { if !$0 { alertMessage = nil } })) {
            Button("好", role: .cancel) { alertMessage = nil }
        } message: { Text(alertMessage ?? "") }
    }

    private func userDetailPane(_ user: UserSummary) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 14) {
                DetailHeaderLayout {
                    userDetailTitle(user)
                    userDetailActions(user)
                }
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), alignment: .leading)], alignment: .leading, spacing: 12) {
                    userMetric("已用流量", value: formatBytes(user.usageBytes))
                    userMetric("流量额度", value: user.quotaBytes.map(formatBytes) ?? "不限")
                    userMetric("到期时间", value: user.expiresAt.map { DateDisplayText.local($0) } ?? "不限")
                    userMetric("分配节点", value: "\(user.assignments.count) 个")
                }
            }
            .padding(16)
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    OverviewColumnsLayout(wideColumns: 2, wideMinimum: 576, narrowColumns: 1) {
                        userQuota(user)
                        userAssignments(user)
                    }
                    if store.isConnected, let usage = selectedUsage, usage.userId == user.id,
                       let pending = usage.pendingRevocations, !pending.isEmpty {
                        GroupBox {
                            VStack(alignment: .leading, spacing: 10) {
                                ForEach(pending, id: \.jobId) { item in
                                    HStack(alignment: .top, spacing: 12) {
                                        VStack(alignment: .leading, spacing: 4) {
                                            Text(nodeDisplayName(item.nodeID)).font(.callout.weight(.medium))
                                            Text(pendingRevocationLabel(item))
                                                .font(.caption)
                                                .foregroundStyle(item.status == "failed" ? Color.red : Color.orange)
                                        }
                                        Spacer(minLength: 8)
                                        Text(DateDisplayText.local(item.updatedAt)).font(.caption).foregroundStyle(.secondary)
                                    }
                                }
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.vertical, 4)
                        } label: {
                            Label("待撤权任务（\(pending.count)）", systemImage: "hourglass")
                        }
                    }
                    DisclosureGroup("用户记录") {
                        LazyVGrid(columns: [GridItem(.adaptive(minimum: 200), alignment: .leading)], alignment: .leading, spacing: 12) {
                            userMetric("用户 ID", value: user.id)
                            userMetric("创建时间", value: DateDisplayText.local(user.createdAt))
                            userMetric("更新时间", value: DateDisplayText.local(user.updatedAt))
                        }
                        .padding(.top, 8)
                    }
                }
                .padding(16)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    private func userDetailTitle(_ user: UserSummary) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Text(user.name).font(.headline).lineLimit(2).textSelection(.enabled)
                Text(user.enabled ? "启用" : "已停用")
                    .font(.caption.weight(.medium))
                    .foregroundStyle(user.enabled ? Color.green : Color.secondary)
                    .padding(.horizontal, 8).padding(.vertical, 3)
                    .background(.quaternary, in: Capsule())
            }
            Text("\(user.assignments.count) 个节点")
                .font(.caption).foregroundStyle(.secondary)
                .accessibilityLabel("\(user.name) · \(user.assignments.count) 个节点")
                .accessibilityIdentifier("user.selected.summary")
        }
    }

    private func userDetailActions(_ user: UserSummary) -> some View {
        HStack(spacing: 8) {
            Button("分配节点…") { assignmentUser = user }
                .accessibilityLabel("分配节点")
                .accessibilityIdentifier("users.assignNodeMenu")
            Menu("mTLS 证书") {
                ForEach(user.assignments, id: \.nodeID) { assignment in
                    if let node = store.nodes.first(where: { $0.id == assignment.nodeID }) {
                        Button(node.name) {
                            assignmentTarget = AssignmentTarget(user: user, node: node, isUpdating: true)
                        }
                        .accessibilityLabel(node.name)
                        .accessibilityIdentifier("users.mtlsNode.\(node.id)")
                    }
                }
            }
            .accessibilityLabel("mTLS 证书")
            .accessibilityIdentifier("users.mtlsMenu")
            .disabled(user.assignments.isEmpty)
            Menu("订阅") {
                Button("复制当前订阅地址") { copyCurrentSubscription(user) }
                    .accessibilityLabel("复制当前订阅地址")
                    .accessibilityIdentifier("users.subscription.copy")
                Button("生成/轮换订阅地址") { rotateSubscription(user) }
                    .accessibilityLabel("生成/轮换订阅地址")
                    .accessibilityIdentifier("users.subscription.rotate")
                Menu("复制指定格式地址") {
                    ForEach(SubscriptionFileFormat.allCases, id: \.rawValue) { format in
                        Button(format.title) { copyCurrentSubscription(user, format: format) }
                            .accessibilityIdentifier("users.subscription.copy.\(format.rawValue)")
                    }
                }
                Button("导出 Mihomo YAML…") { exportSubscription(user, format: .mihomo) }
                    .accessibilityLabel("导出 Mihomo YAML")
                    .accessibilityIdentifier("users.subscription.export")
                Menu("导出其他格式") {
                    ForEach(SubscriptionFileFormat.allCases.filter { $0 != .mihomo }, id: \.rawValue) { format in
                        Button("\(format.title)…") { exportSubscription(user, format: format) }
                            .accessibilityIdentifier("users.subscription.export.\(format.rawValue)")
                    }
                }
            }
            .accessibilityLabel("订阅")
            .accessibilityIdentifier("users.subscriptionMenu")
            Menu("用户") {
                Button("编辑用户…") { showingEditUser = true }
                    .accessibilityLabel("编辑用户")
                    .accessibilityIdentifier("users.actions.edit")
                Button("轮换连接密码") { rotateCredentials(user) }
                    .accessibilityLabel("轮换连接密码")
                    .accessibilityIdentifier("users.actions.rotateCredentials")
                Button("重置额度") { resetQuota(user) }
                    .accessibilityLabel("重置额度")
                    .accessibilityIdentifier("users.actions.resetQuota")
                Button(user.enabled ? "停用" : "启用") { setEnabled(!user.enabled, for: user) }
                    .accessibilityLabel(user.enabled ? "停用" : "启用")
                    .accessibilityIdentifier("users.actions.toggleEnabled")
                Divider()
                Button("删除用户…", role: .destructive) { showingDeleteConfirmation = true }
                    .accessibilityLabel("删除用户")
                    .accessibilityIdentifier("users.actions.delete")
            }
            .accessibilityLabel("用户操作")
            .accessibilityIdentifier("users.actionsMenu")
        }
        .fixedSize()
        .disabled(!store.isConnected)
    }

    private func userQuota(_ user: UserSummary) -> some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 14) {
                QuotaProgressView(usageBytes: user.usageBytes, quotaBytes: user.quotaBytes)
                userMetric("额度周期开始", value: DateDisplayText.local(user.quotaResetAt))
                Divider()
                usageStatus(for: user)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, 4)
        } label: {
            Label("额度与采样", systemImage: "chart.bar")
        }
    }

    private func userAssignments(_ user: UserSummary) -> some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 0) {
                if user.assignments.isEmpty {
                    Text("尚未分配节点").foregroundStyle(.secondary).padding(.vertical, 8)
                }
                ForEach(Array(user.assignments.enumerated()), id: \.element.nodeID) { index, assignment in
                    if index > 0 { Divider() }
                    HStack(alignment: .top, spacing: 12) {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(nodeDisplayName(assignment.nodeID)).font(.callout.weight(.medium))
                            Text(assignment.mtlsCredentialId == nil ? "连接密码" : "mTLS · v\(assignment.mtlsCredentialVersion ?? 1)")
                                .font(.caption).foregroundStyle(.secondary)
                            Text("分配于 \(DateDisplayText.local(assignment.createdAt))")
                                .font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer(minLength: 8)
                        if let node = store.nodes.first(where: { $0.id == assignment.nodeID }) {
                            Button("证书…") {
                                assignmentTarget = AssignmentTarget(user: user, node: node, isUpdating: true)
                            }
                            .disabled(!store.isConnected)
                            .help("管理该节点的 mTLS 证书")
                            .accessibilityIdentifier("users.assignmentCertificate.\(node.id)")
                        }
                    }
                    .padding(.vertical, 8)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        } label: {
            Label("已分配节点（\(user.assignments.count)）", systemImage: "server.rack")
        }
    }

    private func userMetric(_ title: String, value: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            Text(value).font(.callout).textSelection(.enabled)
        }
    }

    private func formatBytes(_ bytes: Int64) -> String {
        ByteCountFormatter.string(fromByteCount: bytes, countStyle: .binary)
    }

    private func copyCurrentSubscription(_ user: UserSummary, format: SubscriptionFileFormat? = nil) {
        Task {
            do {
                let url = try await store.currentSubscriptionURL(for: user, format: format)
                copy(url, message: "当前有效订阅地址已复制；现有令牌没有轮换。")
            } catch { showError(error) }
        }
    }

    private func rotateSubscription(_ user: UserSummary) {
        Task {
            do {
                let url = try await store.rotateSubscription(for: user)
                copy(url, message: "新的订阅地址已复制，旧地址已撤销。")
            } catch { showError(error) }
        }
    }

    private func exportSubscription(_ user: UserSummary, format: SubscriptionFileFormat) {
        Task {
            do {
                let data = try await store.subscriptionFile(for: user, format: format)
                let panel = NSSavePanel()
                let safeName = user.name
                    .replacingOccurrences(of: "/", with: "-")
                    .replacingOccurrences(of: ":", with: "-")
                panel.nameFieldStringValue = "\(safeName)-\(format.rawValue).\(format.fileExtension)"
                panel.allowedContentTypes = [UTType(filenameExtension: format.fileExtension) ?? .plainText]
                guard panel.runModal() == .OK, let destination = panel.url else { return }
                try data.write(to: destination, options: .atomic)
                alertTitle = "配置已导出"
                alertMessage = "\(format.title) 已保存到所选位置。该文件包含连接凭据，请妥善保管。"
            } catch { showError(error) }
        }
    }

    private func rotateCredentials(_ user: UserSummary) {
        Task {
            do {
                let credentials = try await store.rotateCredentials(for: user)
                copy(credentials, message: "新连接密码已复制。旧密码已撤销，请立即保存新值。")
            } catch { showError(error) }
        }
    }

    private func resetQuota(_ user: UserSummary) {
        Task {
            do {
                try await store.resetQuota(for: user)
                alertTitle = "额度已重置"
                alertMessage = "新的额度周期已从零开始。"
            } catch { showError(error) }
        }
    }

    private func setEnabled(_ enabled: Bool, for user: UserSummary) {
        Task {
            do {
                try await store.setEnabled(enabled, for: user)
                alertTitle = enabled ? "用户已启用" : "用户已停用"
                alertMessage = enabled ? "用户可以重新连接。" : "已拒绝新认证，并排队踢下线。"
            } catch { showError(error) }
        }
    }

    private func delete(_ user: UserSummary) {
        Task {
            do {
                try await store.delete(user)
                selectedUserID = nil
            } catch { showError(error) }
        }
    }

    @ViewBuilder
    private func usageStatus(for user: UserSummary) -> some View {
        if !store.isConnected {
            Text("离线时无法刷新采样和待撤权状态。")
                .font(.caption)
                .foregroundStyle(.secondary)
        } else if let usage = selectedUsage, usage.userId == user.id {
            VStack(alignment: .leading, spacing: 12) {
                Label(
                    freshnessLabel(usage.dataFreshness.status),
                    systemImage: usage.dataFreshness.status == "fresh" ? "checkmark.circle.fill" : "exclamationmark.triangle.fill"
                )
                .font(.callout).foregroundStyle(freshnessColor(usage.dataFreshness.status))
                userMetric("最近采样", value: DateDisplayText.local(usage.dataFreshness.lastSampleAt))
                if usage.dataFreshness.openGaps > 0 {
                    Label("未解决统计缺口 \(usage.dataFreshness.openGaps)", systemImage: "chart.bar.fill")
                        .font(.caption).foregroundStyle(.orange)
                } else {
                    Text("无未解决统计缺口").font(.caption).foregroundStyle(.secondary)
                }
                if !usage.byNode.isEmpty {
                    DisclosureGroup("各节点采样（\(usage.byNode.count)）") {
                        VStack(alignment: .leading, spacing: 10) {
                            ForEach(usage.byNode, id: \.nodeID) { item in
                                VStack(alignment: .leading, spacing: 4) {
                                    HStack {
                                        Text(nodeDisplayName(item.nodeID)).font(.callout)
                                        Spacer()
                                        Text(item.assigned.map { $0 ? "当前" : "历史" } ?? "节点")
                                            .font(.caption).foregroundStyle(.secondary)
                                    }
                                    Text(DateDisplayText.local(item.sampledAt)).font(.caption).foregroundStyle(.secondary)
                                }
                            }
                        }
                        .padding(.top, 8)
                    }
                } else if usage.dataFreshness.status == "not_collected" {
                    Text("各节点尚无流量采样记录。").font(.caption).foregroundStyle(.secondary)
                }
            }
        } else if let usageErrorMessage {
            Label("无法读取流量统计状态：\(usageErrorMessage)", systemImage: "exclamationmark.triangle.fill")
                .font(.caption)
                .foregroundStyle(.orange)
        } else if usageLoadingUserID == user.id {
            ProgressView("读取采样和撤权状态…")
                .controlSize(.small)
        }
    }

    private func loadUsage(_ userID: String) async {
        usageLoadingUserID = userID
        usageErrorMessage = nil
        do {
            let response = try await store.userUsage(userID)
            guard !Task.isCancelled, selectedUserID == userID else { return }
            selectedUsage = response
            usageLoadingUserID = nil
        } catch {
            guard !Task.isCancelled, selectedUserID == userID else { return }
            usageErrorMessage = error.localizedDescription
            usageLoadingUserID = nil
        }
    }

    private func freshnessLabel(_ status: String) -> String {
        switch status {
        case "fresh": "统计新鲜"
        case "stale": "统计数据陈旧"
        case "not_collected": "尚未采集"
        default: "统计状态：\(status)"
        }
    }

    private func freshnessColor(_ status: String) -> Color {
        switch status {
        case "fresh": .green
        case "stale": .orange
        default: .gray
        }
    }

    private func pendingRevocationLabel(_ pending: PendingRevocation) -> String {
        let action: String
        if pending.status == "failed" { action = "撤权失败" }
        else if pending.stage == "retry_wait" { action = "等待重试" }
        else if pending.status == "running" { action = "正在断开" }
        else { action = "等待断开" }
        return "\(action) · \(DateDisplayText.local(pending.updatedAt))"
    }

    private func nodeDisplayName(_ nodeID: String) -> String {
        store.nodes.first(where: { $0.id == nodeID })?.name ?? String(nodeID.prefix(8))
    }

    private func copy(_ value: String, message: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(value, forType: .string)
        alertTitle = "已复制到剪贴板"
        alertMessage = message
    }

    private func showError(_ error: Error) {
        alertTitle = "操作失败"
        alertMessage = error.localizedDescription
    }
}

private struct AssignmentTarget: Identifiable {
    let user: UserSummary
    let node: NodeSummary
    let isUpdating: Bool

    var id: String { "\(user.id):\(node.id):\(isUpdating)" }
}

private struct UserAssignmentFormView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ManagementStore
    let user: UserSummary
    let node: NodeSummary
    let isUpdating: Bool
    let onComplete: (String?) -> Void

    @State private var mtlsCredentialID = ""
    @State private var certificateData: Data?
    @State private var privateKeyData: Data?
    @State private var certificateName = ""
    @State private var privateKeyName = ""
    @State private var importPurpose = "certificate"
    @State private var showingImporter = false
    @State private var errorMessage: String?
    @State private var isSaving = false

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(isUpdating ? "更新 mTLS 证书" : "分配节点")
                .font(.title.bold())
            Text("\(user.name) → \(node.name)")
                .foregroundStyle(.secondary)
                .accessibilityLabel("\(user.name) → \(node.name)")
                .accessibilityIdentifier("user.assignment.summary")
            Text("普通节点可留空。启用 mTLS 的节点需要匹配的客户端证书和私钥；订阅通过 HTTPS 返回这两份 PEM 内容，管理服务中会加密存储。")
                .font(.callout)
                .foregroundStyle(.secondary)
            Form {
                CredentialPickerView(store: store, selection: $mtlsCredentialID, kinds: ["tls_identity"], ownerUserID: user.id, title: "已有 mTLS 凭据")
                LabeledContent("客户端证书") {
                    HStack {
                        Text(certificateName.isEmpty ? "未选择" : certificateName)
                            .foregroundStyle(.secondary)
                        Spacer()
                        Button("选择证书…") { choose("certificate") }
                    }
                }
                LabeledContent("客户端私钥") {
                    HStack {
                        Text(privateKeyName.isEmpty ? "未选择" : privateKeyName)
                            .foregroundStyle(.secondary)
                        Spacer()
                        Button("选择私钥…") { choose("private-key") }
                    }
                }
            }
            if let errorMessage {
                Text(errorMessage).foregroundStyle(.red).font(.callout)
            }
            HStack {
                Button("取消") { dismiss() }.keyboardShortcut(.cancelAction)
                Spacer()
                Button(isSaving ? "正在保存…" : (isUpdating ? "更新证书" : "分配节点")) { save() }
                    .disabled(isSaving || !store.isConnected)
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(24)
        .frame(width: 520, height: 360)
        .fileImporter(isPresented: $showingImporter, allowedContentTypes: [.data], allowsMultipleSelection: false) { result in
            importFile(result)
        }
    }

    private func choose(_ purpose: String) {
        mtlsCredentialID = ""
        importPurpose = purpose
        showingImporter = true
    }

    private func importFile(_ result: Result<[URL], Error>) {
        do {
            guard let url = try result.get().first else { return }
            let hasAccess = url.startAccessingSecurityScopedResource()
            defer { if hasAccess { url.stopAccessingSecurityScopedResource() } }
            let data = try Data(contentsOf: url, options: .mappedIfSafe)
            guard !data.isEmpty, data.count <= 1_048_576, String(data: data, encoding: .utf8) != nil else {
                errorMessage = "客户端证书和私钥必须是小于 1 MiB 的 UTF-8 PEM 文件。"
                return
            }
            if importPurpose == "certificate" {
                certificateData = data
                certificateName = url.lastPathComponent
            } else {
                privateKeyData = data
                privateKeyName = url.lastPathComponent
            }
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func save() {
        if mtlsCredentialID.isEmpty && ((certificateData == nil) != (privateKeyData == nil) || (isUpdating && certificateData == nil)) {
            errorMessage = "请同时选择客户端证书和私钥。"
            return
        }
        let certificate = certificateData.flatMap { String(data: $0, encoding: .utf8) }
        let privateKey = privateKeyData.flatMap { String(data: $0, encoding: .utf8) }
        isSaving = true
        Task {
            defer { isSaving = false }
            do {
                if isUpdating {
                    try await store.updateAssignmentClientCertificate(
                        user,
                        for: node,
                        clientCertificate: certificate ?? "",
                        clientPrivateKey: privateKey ?? "",
                        mtlsCredentialID: mtlsCredentialID.isEmpty ? nil : mtlsCredentialID
                    )
                    onComplete(nil)
                } else {
                    let credential = try await store.assign(
                        user,
                        to: node,
                        clientCertificate: certificate,
                        clientPrivateKey: privateKey,
                        mtlsCredentialID: mtlsCredentialID.isEmpty ? nil : mtlsCredentialID
                    )
                    onComplete(credential)
                }
                dismiss()
            } catch {
                errorMessage = error.localizedDescription
            }
        }
    }
}

private struct UserFormView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ManagementStore
    @State private var name = ""
    @State private var quotaGB = ""
    @State private var expiresAt = false
    @State private var expiration = Date.now.addingTimeInterval(30 * 24 * 60 * 60)
    @State private var errorMessage: String?
    @State private var isSaving = false

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("添加用户").font(.title.bold())
            Form {
                TextField("用户名称", text: $name)
                    .accessibilityLabel("用户名称")
                    .accessibilityIdentifier("user.create.name")
                TextField("总流量额度（GB，可留空）", text: $quotaGB)
                    .accessibilityLabel("总流量额度（GB，可留空）")
                    .accessibilityIdentifier("user.create.quotaGB")
                Toggle("设置到期时间", isOn: $expiresAt)
                    .accessibilityLabel("设置到期时间")
                    .accessibilityIdentifier("user.create.hasExpiry")
                if expiresAt {
                    DatePicker("到期时间", selection: $expiration, in: Date.now...)
                        .accessibilityLabel("到期时间")
                        .accessibilityIdentifier("user.create.expiration")
                }
            }
            .formStyle(.grouped)
            if let errorMessage { Text(errorMessage).foregroundStyle(.red).font(.callout) }
            HStack {
                Button("取消") { dismiss() }.keyboardShortcut(.cancelAction)
                Spacer()
                Button(isSaving ? "正在保存…" : "创建用户") { save() }.disabled(isSaving).keyboardShortcut(.defaultAction)
            }
        }
        .padding(24)
        .frame(width: 480, height: 360)
    }

    private func save() {
        let quota: Int64?
        if quotaGB.trimmingCharacters(in: .whitespaces).isEmpty { quota = nil }
        else if let gigabytes = Double(quotaGB), gigabytes >= 0 { quota = Int64(gigabytes * 1_000_000_000) }
        else { errorMessage = "额度请输入有效的非负数字。"; return }
        isSaving = true
        Task {
            defer { isSaving = false }
            do {
                let date = expiresAt ? ISO8601DateFormatter().string(from: expiration) : nil
                try await store.createUser(UserCreateRequest(name: name, enabled: true, expiresAt: date, quotaBytes: quota))
                dismiss()
            } catch { errorMessage = error.localizedDescription }
        }
    }
}

private struct UserEditFormView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ManagementStore
    let user: UserSummary
    @State private var name: String
    @State private var enabled: Bool
    @State private var hasExpiry: Bool
    @State private var expiration: Date
    @State private var hasQuota: Bool
    @State private var quotaBytesText: String
    @State private var errorMessage: String?
    @State private var isSaving = false

    init(store: ManagementStore, user: UserSummary) {
        self.store = store
        self.user = user
        _name = State(initialValue: user.name)
        _enabled = State(initialValue: user.enabled)
        _hasExpiry = State(initialValue: user.expiresAt != nil)
        _expiration = State(
            initialValue: DateDisplayText.parse(user.expiresAt)
                ?? Date.now.addingTimeInterval(30 * 24 * 60 * 60)
        )
        _hasQuota = State(initialValue: user.quotaBytes != nil)
        _quotaBytesText = State(initialValue: user.quotaBytes.map { String($0) } ?? "")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("编辑用户").font(.title.bold())
            Form {
                TextField("用户名称", text: $name)
                    .accessibilityLabel("用户名称")
                    .accessibilityIdentifier("user.edit.name")
                Toggle("启用用户", isOn: $enabled)
                    .accessibilityLabel("启用用户")
                    .accessibilityIdentifier("user.edit.enabled")
                Toggle("设置到期时间", isOn: $hasExpiry)
                    .accessibilityLabel("设置到期时间")
                    .accessibilityIdentifier("user.edit.hasExpiry")
                if hasExpiry {
                    DatePicker("到期时间", selection: $expiration, displayedComponents: [.date, .hourAndMinute])
                        .accessibilityLabel("到期时间")
                        .accessibilityIdentifier("user.edit.expiration")
                }
                Toggle("设置总流量额度", isOn: $hasQuota)
                    .accessibilityLabel("设置总流量额度")
                    .accessibilityIdentifier("user.edit.hasQuota")
                if hasQuota {
                    TextField("总流量额度（字节）", text: $quotaBytesText)
                        .accessibilityLabel("总流量额度（字节）")
                        .accessibilityIdentifier("user.edit.quotaBytes")
                    Text("额度按所有节点累计的上下行流量计算。")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            if let errorMessage {
                Text(errorMessage).foregroundStyle(.red).font(.callout)
            }
            HStack {
                Button("取消") { dismiss() }.keyboardShortcut(.cancelAction)
                Spacer()
                Button(isSaving ? "正在保存…" : "保存用户") { save() }
                    .disabled(isSaving || !store.isConnected)
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(24)
        .frame(width: 520, height: 440)
    }

    private func save() {
        let quota: Int64?
        if hasQuota {
            guard let bytes = Int64(quotaBytesText.trimmingCharacters(in: .whitespacesAndNewlines)), bytes >= 0 else {
                errorMessage = "额度请输入有效的非负字节数。"
                return
            }
            quota = bytes
        } else {
            quota = nil
        }
        isSaving = true
        Task {
            defer { isSaving = false }
            do {
                try await store.updateUserDetails(
                    user,
                    name: name,
                    enabled: enabled,
                    expiresAt: hasExpiry ? expiration : nil,
                    quotaBytes: quota
                )
                dismiss()
            } catch { errorMessage = error.localizedDescription }
        }
    }
}
