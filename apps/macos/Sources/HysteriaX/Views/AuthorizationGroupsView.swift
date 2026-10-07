import SwiftUI

struct AuthorizationGroupsView: View {
    @Bindable var store: ManagementStore
    @Bindable var pageState: AuthorizationPageState
    var onOpenUser: (String) -> Void

    @State private var editorTarget: AuthorizationGroupEditorTarget?
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
        MainVerticalSplitView(hasDetail: selectedGroup != nil) {
            Table(visibleGroups, selection: $pageState.selectedGroupID, sortOrder: $pageState.groupSortOrder) {
                TableColumn(L10n.text("名称"), value: \.name) { group in
                    Text(group.name)
                        .accessibilityIdentifier("authorizationGroups.row.\(group.id)")
                }
                TableColumn(L10n.text("用户")) { group in Text(String(group.userCount)) }
                    .width(min: 70, ideal: 100)
                TableColumn(L10n.text("节点")) { group in Text(String(group.nodeCount)) }
                    .width(min: 70, ideal: 100)
                TableColumn(L10n.text("更新时间")) { group in Text(DateDisplayText.local(group.updatedAt)) }
                    .width(min: 150, ideal: 180)
            }
            .frame(minHeight: 180)
            .overlay {
                if let error = store.authorizationGroupsError {
                    VStack(spacing: 10) {
                        ContentUnavailableView(L10n.text("无法读取授权组"), systemImage: "exclamationmark.triangle", description: Text(error))
                        Button(L10n.text("重试")) { Task { await store.refreshAuthorizationGroups() } }
                            .disabled(!store.isConnected || store.isLoadingAuthorizationGroups)
                    }
                    .padding(20)
                } else if store.authorizationGroups.isEmpty, store.isLoadingAuthorizationGroups {
                    ProgressView(L10n.text("读取授权组…"))
                } else if store.authorizationGroups.isEmpty {
                    ContentUnavailableView(L10n.text("还没有授权组"), systemImage: "person.3", description: Text(L10n.text("创建授权组后，可一次为多位用户授予多个节点的访问权限。")))
                } else if visibleGroups.isEmpty {
                    ContentUnavailableView(L10n.text("没有匹配的授权组"), systemImage: "magnifyingglass")
                }
            }
        } detail: {
            if let selectedGroup {
                groupDetail(selectedGroup)
                    .id(selectedGroup.id)
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier("authorizationGroups.detail")
            }
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button { editorTarget = AuthorizationGroupEditorTarget(group: nil, intent: .create) } label: {
                    Label(L10n.text("创建授权组"), systemImage: "plus")
                }
                .keyboardShortcut("n", modifiers: .command)
                .disabled(!store.isConnected)
                .accessibilityIdentifier("authorizationGroups.create")
            }
        }
        .searchable(text: $pageState.groupSearchText, placement: .toolbar, prompt: L10n.text("搜索授权组"))
        .onChange(of: pageState.groupSearchText) { _, _ in pageState.selectedGroupID = nil }
        .sheet(item: $editorTarget) { target in
            AuthorizationGroupEditorView(store: store, group: target.group, intent: target.intent) { id in
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
                    HStack(spacing: 8) {
                        Button(L10n.text("编辑名称…"), systemImage: "pencil") {
                            editorTarget = AuthorizationGroupEditorTarget(group: group, intent: .rename)
                        }
                        Button(L10n.text("管理用户…"), systemImage: "person.2") {
                            editorTarget = AuthorizationGroupEditorTarget(group: group, intent: .users)
                        }
                        Button(L10n.text("管理节点…"), systemImage: "server.rack") {
                            editorTarget = AuthorizationGroupEditorTarget(group: group, intent: .nodes)
                        }
                        Menu {
                            Button(L10n.text("删除授权组…"), role: .destructive) { prepareDeletion(group) }
                        } label: {
                            Image(systemName: "ellipsis")
                        }
                        .accessibilityLabel(L10n.text("更多授权组操作"))
                    }
                    .controlSize(.small)
                    .disabled(!store.isConnected)
                }
            }
            .padding(16)
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    members(group)
                    nodes(group)
                    DisclosureGroup(L10n.text("授权组记录")) {
                        LazyVGrid(columns: [GridItem(.adaptive(minimum: 200), alignment: .leading)], alignment: .leading, spacing: 12) {
                            groupMetric(L10n.text("授权组 ID"), value: group.id)
                            groupMetric(L10n.text("创建时间"), value: DateDisplayText.local(group.createdAt))
                            groupMetric(L10n.text("更新时间"), value: DateDisplayText.local(group.updatedAt))
                            groupMetric(L10n.text("版本"), value: String(group.revision))
                        }
                        .padding(.top, 8)
                    }
                }
                .padding(16)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    private func members(_ group: AuthorizationGroupSummary) -> some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 0) {
                if group.userIds.isEmpty {
                    Text(L10n.text("此授权组没有成员。"))
                        .foregroundStyle(.secondary).padding(.vertical, 8)
                }
                ForEach(Array(group.userIds.sorted().enumerated()), id: \.element) { index, userID in
                    if index > 0 { Divider() }
                    Button {
                        onOpenUser(userID)
                    } label: {
                        HStack(spacing: 10) {
                            Image(systemName: "person")
                                .foregroundStyle(.secondary)
                            Text(store.users.first(where: { $0.id == userID })?.name ?? userID)
                            Spacer()
                            Image(systemName: "arrow.up.right")
                                .font(.caption).foregroundStyle(.secondary)
                        }
                        .contentShape(Rectangle())
                        .padding(.vertical, 8)
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("authorizationGroups.member.\(userID)")
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        } label: {
            Label(L10n.text("成员用户（{0}）", String(group.userIds.count)), systemImage: "person.2")
        }
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
                    HStack(spacing: 10) {
                        Image(systemName: "server.rack").foregroundStyle(.secondary)
                        Text(store.nodes.first(where: { $0.id == nodeID })?.name ?? nodeID)
                        Spacer()
                        Text(nodeID.prefix(8)).font(.caption.monospaced()).foregroundStyle(.secondary)
                    }
                    .padding(.vertical, 8)
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

struct AuthorizationGroupEditorTarget: Identifiable {
    let group: AuthorizationGroupSummary?
    let intent: AuthorizationGroupEditorIntent
    var id: String { "\(group?.id ?? "new"):\(intent.rawValue)" }
}

enum AuthorizationGroupEditorIntent: String, Equatable {
    case create, rename, users, nodes
}
