import SwiftUI

struct AuthorizationGroupsView: View {
    @Bindable var store: ManagementStore
    @Bindable var pageState: AuthorizationPageState
    var onOpenUser: (String) -> Void
    var onOpenNode: (String) -> Void = { _ in }

    @State private var groupListWidth: CGFloat = 200
    @State private var resizingListStart: CGFloat?
    @State private var deletionTarget: AuthorizationGroupSummary?

    @State private var response: AuthorizationGroupsPage?
    @State private var loadedKey: String?
    @State private var error: String?
    @State private var loading = false
    @State private var retry = UUID()

    private var key: String { store.listPageKey([pageState.groupSearchText, pageState.groupSort, String(pageState.groupPage)]) }
    private var requestKey: String { "\(key)|\(store.isConnected)|\(store.dataRefreshToken)|\(retry)" }
    private var visibleGroups: [AuthorizationGroupSummary] {
        if !store.isConnected { return Array(store.authorizationGroups.prefix(50)) }
        return loadedKey == key ? response?.items ?? [] : []
    }
    private var selectedGroup: AuthorizationGroupSummary? {
        visibleGroups.first { $0.id == pageState.selectedGroupID }
    }

    var body: some View {
        directoryContent
            .onChange(of: pageState.groupSort) { _, _ in pageState.groupPage = 1 }
            .onChange(of: store.serviceAddress) { _, _ in
                response = nil; loadedKey = nil; pageState.selectedGroupID = nil; pageState.groupPage = 1
            }
            .task(id: requestKey) { await loadPage() }
    }

    private func loadPage() async {
        let request = requestKey
        guard store.isConnected else { loading = false; return }
        loading = true; error = nil
        defer { if request == requestKey { loading = false } }
        do {
            if !pageState.groupSearchText.isEmpty { try await Task.sleep(for: .milliseconds(250)) }
            let value = try await store.authorizationGroupsPage(page: pageState.groupPage, query: pageState.groupSearchText, sort: pageState.groupSort, order: pageState.groupSort == "updated_at" ? "desc" : "asc")
            guard !Task.isCancelled, request == requestKey else { return }
            pageState.groupPage = value.page; response = value; loadedKey = key
        } catch {
            guard !Task.isCancelled, !isRequestCancellation(error), request == requestKey else { return }
            self.error = error.localizedDescription
        }
    }

    private var directoryContent: some View {
        // Keep the data/content subtree outside the per-frame geometry closure.
        let sidebarContent = groupList
        let detailContent = selectedDetail
        return GeometryReader { geometry in
            let listWidth = min(240, max(180, groupListWidth))
            HStack(spacing: 0) {
                sidebarContent
                    .frame(width: listWidth, height: geometry.size.height)
                Divider()
                    .frame(width: 6, height: geometry.size.height)
                    .contentShape(Rectangle())
                    .gesture(DragGesture(minimumDistance: 0)
                        .onChanged { value in
                            if resizingListStart == nil { resizingListStart = groupListWidth }
                            groupListWidth = min(240, max(180, (resizingListStart ?? groupListWidth) + value.translation.width))
                        }
                        .onEnded { _ in resizingListStart = nil })
                    .accessibilityLabel(L10n.text("调整授权组列表宽度"))
                    .accessibilityValue(String(Int(listWidth)))
                    .accessibilityAdjustableAction { direction in
                        switch direction {
                        case .increment: groupListWidth = min(240, groupListWidth + 12)
                        case .decrement: groupListWidth = max(180, groupListWidth - 12)
                        @unknown default: break
                        }
                    }
                detailContent
                .frame(width: max(0, geometry.size.width - listWidth - 6), height: geometry.size.height, alignment: .topLeading)
            }
            .frame(width: geometry.size.width, height: geometry.size.height)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .toolbar {
            ToolbarItem(id: "authorizationGroups.create", placement: .primaryAction) {
                Button { pageState.groupEditorTarget = AuthorizationGroupEditorTarget(group: nil, intent: .create) } label: {
                    Label(L10n.text("创建授权组"), systemImage: "plus")
                }
                .keyboardShortcut("n", modifiers: .command)
                .disabled(!store.isConnected)
                .accessibilityIdentifier("authorizationGroups.create")
            }
        }
        .searchable(text: $pageState.groupSearchText, placement: .toolbar, prompt: L10n.text("搜索授权组"))
        .onChange(of: pageState.groupSearchText) { _, _ in pageState.selectedGroupID = nil; pageState.groupPage = 1 }
        .sheet(item: $pageState.groupEditorTarget) { target in
            AuthorizationGroupEditorView(store: store, group: target.group, intent: target.intent, memberMode: target.memberMode, removedUserIDs: target.removedUserIDs) { id in
                pageState.selectedGroupID = id
            }
        }
        .sheet(item: $deletionTarget) { group in
            AuthorizationGroupDeletionView(store: store, group: group) {
                pageState.selectedGroupID = nil
            }
        }
    }

    @ViewBuilder
    private var selectedDetail: some View {
        if let groupID = pageState.selectedGroupID {
            AuthorizationGroupDetailPane(
                store: store,
                pageState: pageState,
                groupID: groupID,
                initialGroup: selectedGroup,
                onOpenUser: onOpenUser,
                onOpenNode: onOpenNode,
                onDelete: { deletionTarget = $0 },
                onRefreshList: { retry = UUID() }
            )
            .id(groupID)
        } else {
            ContentUnavailableView(L10n.text("选择授权组"), systemImage: "person.3", description: Text(L10n.text("在左侧选择授权组，查看成员和节点。")))
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private var groupList: some View {
        VStack(spacing: 0) {
            HStack {
                Text(L10n.text("授权组")).font(.headline)
                Spacer()
                Menu {
                    Button(L10n.text("名称")) { pageState.groupSort = "name" }
                    Button(L10n.text("更新时间")) { pageState.groupSort = "updated_at" }
                } label: {
                    Image(systemName: "arrow.up.arrow.down")
                }
                .accessibilityLabel(L10n.text("排序"))
            }
            .padding(12)
            Divider()
            List(visibleGroups, selection: $pageState.selectedGroupID) { group in
                HStack(spacing: 8) {
                    Image(systemName: "person.3").foregroundStyle(.secondary)
                    VStack(alignment: .leading, spacing: 4) {
                        Text(group.name).lineLimit(1)
                            .accessibilityIdentifier("authorizationGroups.row.\(group.id)")
                        Text(L10n.text("{0} 位用户 · {1} 个节点", String(group.userCount), String(group.nodeCount)))
                            .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    }
                }
                .padding(.vertical, 3)
                .tag(group.id)
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            .accessibilityIdentifier("authorizationGroups.list")
            .overlay {
                if let error {
                    VStack(spacing: 10) {
                        Label(L10n.text("无法读取授权组"), systemImage: "exclamationmark.triangle")
                        Button(L10n.text("重试")) { retry = UUID() }
                            .disabled(!store.isConnected || loading)
                    }
                    .padding(12).help(error)
                } else if visibleGroups.isEmpty, loading {
                    ProgressView(L10n.text("读取授权组…"))
                } else if visibleGroups.isEmpty {
                    Text(pageState.groupSearchText.isEmpty ? L10n.text("还没有授权组") : L10n.text("没有匹配的授权组")).foregroundStyle(.secondary)
                }
            }
            Divider()
            AuthorizationPagination(page: $pageState.groupPage, pages: max(1, ((response?.total ?? 0) + 49) / 50), total: response?.total ?? 0, disabled: loading || !store.isConnected, identifier: "authorizationGroups")
                .padding(8)
        }
    }

}

private struct AuthorizationGroupDetailPane: View {
    @Bindable var store: ManagementStore
    @Bindable var pageState: AuthorizationPageState
    let groupID: String
    let initialGroup: AuthorizationGroupSummary?
    var onOpenUser: (String) -> Void
    var onOpenNode: (String) -> Void
    var onDelete: (AuthorizationGroupSummary) -> Void
    var onRefreshList: () -> Void

    @State private var detailTab = "members"
    @State private var detail: AuthorizationGroupSummary?
    @State private var detailError: String?
    @State private var pendingJob: JobSummary?
    @State private var isRetryingPendingJob = false
    @State private var retry = UUID()

    private var detailKey: String {
        "\(store.serviceAddress)|\(groupID)|\(store.isConnected)|\(store.dataRefreshToken)|\(retry)"
    }

    private var displayedGroup: AuthorizationGroupSummary? {
        if let detail, detail.id == groupID { return detail }
        return initialGroup
    }

    var body: some View {
        Group {
            if let group = displayedGroup {
                groupDetail(group)
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier("authorizationGroups.detail")
            } else if let detailError {
                VStack { Text(detailError).foregroundStyle(.red); Button(L10n.text("重试")) { retry = UUID(); onRefreshList() } }
            } else {
                ProgressView()
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .task(id: detailKey) { await loadDetail() }
    }

    private func loadDetail() async {
        let request = detailKey
        detailError = nil
        guard store.isConnected else {
            detail = nil
            pendingJob = nil
            return
        }
        if pendingJob?.resourceId != groupID { pendingJob = nil }
        do {
            async let loadedDetail = store.authorizationGroupDetail(groupID)
            async let loadedPending = store.pendingAuthorizationJob(forResourceID: groupID)
            let value = try await loadedDetail
            let pending = await loadedPending
            guard !Task.isCancelled, request == detailKey else { return }
            detail = value
            pendingJob = pending
        } catch {
            guard !Task.isCancelled, !isRequestCancellation(error), request == detailKey else { return }
            detail = nil
            pendingJob = nil
            detailError = error.localizedDescription
        }
    }

    @ViewBuilder
    private func groupDetail(_ group: AuthorizationGroupSummary) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            DetailHeaderBar {
                VStack(alignment: .leading, spacing: 14) {
                    DetailHeaderLayout {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(group.name).font(.headline).lineLimit(2).textSelection(.enabled)
                            Text(L10n.text("{0} 位用户 · {1} 个节点", String(group.userCount), String(group.nodeCount)))
                                .font(.caption).foregroundStyle(.secondary)
                                .accessibilityIdentifier("authorizationGroups.selected.summary")
                        }
                        groupActions(group).fixedSize(horizontal: true, vertical: false)
                    }
                }
            }
            if let job = pendingJob, job.resourceId == group.id {
                pendingAuthorizationBanner(job)
            }
            Picker(L10n.text("授权组详情"), selection: $detailTab) {
                Text(L10n.text("成员")).tag("members")
                Text(L10n.text("节点")).tag("nodes")
                Text(L10n.text("记录")).tag("records")
            }
            .pickerStyle(.segmented)
            .padding(.horizontal, 16).padding(.top, 12)
            .accessibilityIdentifier("authorizationGroups.detailTabs")
            if detailTab == "members" {
                AuthorizationGroupMembersView(store: store, group: group, canEdit: store.isConnected, onOpen: onOpenUser) { mode, removed in
                    pageState.groupEditorTarget = AuthorizationGroupEditorTarget(group: group, intent: .users, memberMode: mode, removedUserIDs: removed)
                }
                .id(group.id)
            } else if detailTab == "nodes" {
                nodes(group).padding(16)
            } else {
                ScrollView {
                        LazyVGrid(columns: [GridItem(.adaptive(minimum: 200), alignment: .leading)], alignment: .leading, spacing: 12) {
                            groupMetric(L10n.text("授权组 ID"), value: group.id)
                            groupMetric(L10n.text("创建时间"), value: DateDisplayText.local(group.createdAt))
                            groupMetric(L10n.text("更新时间"), value: DateDisplayText.local(group.updatedAt))
                            groupMetric(L10n.text("版本"), value: String(group.revision))
                        }
                }
                .padding(16)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private func groupActions(_ group: AuthorizationGroupSummary) -> some View {
        HStack(spacing: 8) {
            Button(L10n.text("编辑名称")) {
                pageState.groupEditorTarget = AuthorizationGroupEditorTarget(group: group, intent: .rename)
            }
            .accessibilityIdentifier("authorizationGroups.rename")
            Button(L10n.text("管理节点")) {
                pageState.groupEditorTarget = AuthorizationGroupEditorTarget(group: group, intent: .nodes)
            }
            .accessibilityIdentifier("authorizationGroups.manageNodes")
            Button(L10n.text("删除授权组"), role: .destructive) { onDelete(group) }
                .foregroundStyle(.red)
                .accessibilityIdentifier("authorizationGroups.delete")
        }
        .controlSize(.small)
        .disabled(!store.isConnected)
    }

    private func nodes(_ group: AuthorizationGroupSummary) -> some View {
        AuthorizationGroupNodesView(store: store, group: group, onOpen: onOpenNode)
            .frame(minHeight: 360)
    }

    private func pendingAuthorizationBanner(_ job: JobSummary) -> some View {
        let isFailed = job.status == "failed"
        let tone: StatusTone = isFailed ? .warning : .active
        return HStack(alignment: .center, spacing: 12) {
            Image(systemName: isFailed ? "exclamationmark.triangle.fill" : "hourglass")
                .foregroundStyle(tone.color)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(isFailed ? L10n.text("存在失败的授权变更任务") : L10n.text("存在处理中的授权变更任务"))
                    .font(.callout.weight(.medium))
                Text(job.errorMessage.map(JobDisplayText.errorMessage) ?? JobDisplayText.stage(job.stage))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
            Spacer(minLength: 8)
            Button(L10n.text("查看任务")) {
                store.requestedJobID = job.id
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .accessibilityIdentifier("authorizationGroups.pendingJob.open")
            if isFailed && job.retryJobId == nil {
                Button(isRetryingPendingJob ? L10n.text("正在重试…") : L10n.text("重试授权任务")) {
                    retryPendingJob(job)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(isRetryingPendingJob || !store.isConnected)
                .accessibilityIdentifier("authorizationGroups.pendingJob.retry")
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .background(tone.color.opacity(0.08))
        .overlay(alignment: .bottom) { Divider() }
        .accessibilityIdentifier("authorizationGroups.pendingJob.banner")
    }

    private func retryPendingJob(_ job: JobSummary) {
        guard !isRetryingPendingJob else { return }
        isRetryingPendingJob = true
        Task {
            defer { isRetryingPendingJob = false }
            do {
                _ = try await store.retryAuthorizationJob(job)
                store.toast.show(L10n.text("已提交重试任务"), tone: .active)
                retry = UUID()
                onRefreshList()
                await store.refresh()
            } catch {
                store.toast.show(
                    L10n.text("任务操作失败"),
                    detail: JobDisplayText.errorMessage(error.localizedDescription),
                    tone: .danger
                )
            }
        }
    }

    private func groupMetric(_ title: String, value: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            Text(value).font(.callout).textSelection(.enabled)
        }
    }
}
