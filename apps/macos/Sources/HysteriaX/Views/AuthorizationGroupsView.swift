import SwiftUI

struct AuthorizationGroupsView: View {
    @Bindable var store: ManagementStore
    @Bindable var pageState: AuthorizationPageState
    var onOpenUser: (String) -> Void
    var onOpenNode: (String) -> Void = { _ in }

    @State private var detailTab = "members"
    @State private var groupListWidth: CGFloat = 200
    @State private var resizingListStart: CGFloat?
    @State private var deletionTarget: AuthorizationGroupSummary?
    @State private var deletionPreview: AuthorizationChangePreview?
    @State private var isPreparingDeletion = false
    @State private var deletionError: String?

    private var visibleGroups: [AuthorizationGroupSummary] {
        let query = pageState.groupSearchText.trimmingCharacters(in: .whitespacesAndNewlines)
        return store.authorizationGroups
            .filter { query.isEmpty || $0.name.localizedStandardContains(query) || $0.id.localizedCaseInsensitiveContains(query) }
            .sorted(using: pageState.groupSortOrder)
    }

    private var selectedGroup: AuthorizationGroupSummary? {
        store.authorizationGroups.first { $0.id == pageState.selectedGroupID }
    }

    var body: some View {
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
        .onChange(of: pageState.selectedGroupID) { _, _ in detailTab = "members" }
        .onChange(of: pageState.groupSearchText) { _, _ in pageState.selectedGroupID = nil }
        .sheet(item: $pageState.groupEditorTarget) { target in
            AuthorizationGroupEditorView(store: store, group: target.group, intent: target.intent, memberMode: target.memberMode, removedUserIDs: target.removedUserIDs) { id in
                pageState.selectedGroupID = id
            }
        }
        .sheet(item: $deletionTarget) { group in
            if let deletionPreview {
                AuthorizationGroupDeletionPreviewView(
                    store: store,
                    group: group,
                    preview: deletionPreview,
                    onDeleted: {
                        pageState.selectedGroupID = nil
                    }
                )
            } else {
                VStack(alignment: .leading, spacing: 16) {
                    Text(L10n.text("预览删除影响…")).font(.title2.bold())
                    if let deletionError { Text(deletionError).foregroundStyle(.red).textSelection(.enabled) }
                    else if isPreparingDeletion { ProgressView() }
                    HStack {
                        Button(L10n.text("取消"), role: .cancel) { deletionTarget = nil }
                        Spacer()
                        Button(L10n.text("重试")) { prepareDeletion(group) }
                            .disabled(isPreparingDeletion || !store.isConnected)
                    }
                }
                .padding(24)
                .frame(width: 480)
                .task { prepareDeletion(group) }
            }
        }
        .task(id: store.isConnected) {
            if store.isConnected { await store.refreshAuthorizationGroups() }
        }
    }

    @ViewBuilder
    private var selectedDetail: some View {
        if let selectedGroup {
            groupDetail(selectedGroup)
                .id(selectedGroup.id)
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("authorizationGroups.detail")
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
                    Button(L10n.text("名称")) { pageState.groupSortOrder = [KeyPathComparator(\.name)] }
                    Button(L10n.text("更新时间")) { pageState.groupSortOrder = [KeyPathComparator(\.updatedAt, order: .reverse)] }
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
                if let error = store.authorizationGroupsError {
                    VStack(spacing: 10) {
                        Label(L10n.text("无法读取授权组"), systemImage: "exclamationmark.triangle")
                        Button(L10n.text("重试")) { Task { await store.refreshAuthorizationGroups() } }
                            .disabled(!store.isConnected || store.isLoadingAuthorizationGroups)
                    }
                    .padding(12).help(error)
                } else if store.authorizationGroups.isEmpty, store.isLoadingAuthorizationGroups {
                    ProgressView(L10n.text("读取授权组…"))
                } else if store.authorizationGroups.isEmpty {
                    Text(L10n.text("还没有授权组")).foregroundStyle(.secondary)
                } else if visibleGroups.isEmpty {
                    Text(L10n.text("没有匹配的授权组")).foregroundStyle(.secondary)
                }
            }
        }
    }

    @ViewBuilder
    private func groupDetail(_ group: AuthorizationGroupSummary) -> some View {
        VStack(alignment: .leading, spacing: 0) {
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
            .padding(16)
            Divider()
            Picker(L10n.text("授权组详情"), selection: $detailTab) {
                Text(L10n.text("成员")).tag("members")
                Text(L10n.text("节点")).tag("nodes")
                Text(L10n.text("记录")).tag("records")
            }
            .pickerStyle(.segmented)
            .padding(.horizontal, 16).padding(.top, 12)
            .accessibilityIdentifier("authorizationGroups.detailTabs")
            if detailTab == "members" {
                AuthorizationGroupMembersView(group: group, users: store.users, canEdit: store.isConnected, onOpen: onOpenUser) { mode, removed in
                    pageState.groupEditorTarget = AuthorizationGroupEditorTarget(group: group, intent: .users, memberMode: mode, removedUserIDs: removed)
                }
                .id(group.id)
            } else {
                ScrollView {
                    if detailTab == "nodes" {
                        nodes(group)
                    } else {
                        LazyVGrid(columns: [GridItem(.adaptive(minimum: 200), alignment: .leading)], alignment: .leading, spacing: 12) {
                            groupMetric(L10n.text("授权组 ID"), value: group.id)
                            groupMetric(L10n.text("创建时间"), value: DateDisplayText.local(group.createdAt))
                            groupMetric(L10n.text("更新时间"), value: DateDisplayText.local(group.updatedAt))
                            groupMetric(L10n.text("版本"), value: String(group.revision))
                        }
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
            Button(L10n.text("删除授权组"), role: .destructive) { prepareDeletion(group) }
                .foregroundStyle(.red)
                .accessibilityIdentifier("authorizationGroups.delete")
        }
        .controlSize(.small)
        .disabled(!store.isConnected)
    }

    private func nodes(_ group: AuthorizationGroupSummary) -> some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 0) {
                if group.nodeIds.isEmpty {
                    Text(L10n.text("此授权组没有节点。"))
                        .foregroundStyle(.secondary).padding(.vertical, 8)
                }
                ForEach(Array(group.nodeIds.sorted().enumerated()), id: \.element) { index, nodeID in
                    if index > 0 { Divider() }
                    Button {
                        onOpenNode(nodeID)
                    } label: {
                        HStack(spacing: 10) {
                            Image(systemName: "server.rack").foregroundStyle(.secondary)
                            Text(store.nodes.first(where: { $0.id == nodeID })?.name ?? nodeID)
                                .foregroundStyle(Color.accentColor)
                            Spacer()
                            Text(nodeID.prefix(8)).font(.caption.monospaced()).foregroundStyle(.secondary)
                            Image(systemName: "chevron.right").font(.caption).foregroundStyle(.secondary)
                        }
                        .padding(.vertical, 8)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .disabled(!store.nodes.contains { $0.id == nodeID })
                    .accessibilityIdentifier("authorizationGroups.node.\(nodeID)")
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        } label: {
            Label(L10n.text("授权节点（{0}）", String(group.nodeIds.count)), systemImage: "server.rack")
        }
    }

    private func groupMetric(_ title: String, value: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            Text(value).font(.callout).textSelection(.enabled)
        }
    }

    private func prepareDeletion(_ group: AuthorizationGroupSummary) {
        guard !isPreparingDeletion else { return }
        isPreparingDeletion = true
        deletionError = nil
        deletionTarget = group
        deletionPreview = nil
        Task {
            defer { isPreparingDeletion = false }
            do {
                deletionPreview = try await store.previewAuthorizationGroupDeletion(group)
            } catch {
                deletionError = error.localizedDescription
            }
        }
    }
}
