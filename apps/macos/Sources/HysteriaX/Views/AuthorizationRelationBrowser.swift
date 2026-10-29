import SwiftUI

/// Server pages for members/candidates; only explicit selections and draft changes stay in memory.
struct AuthorizationRelationBrowser: View {
    @Bindable var store: ManagementStore
    let group: AuthorizationGroupSummary
    let users: Bool
    var editing = false
    @Binding var mode: AuthorizationMemberEditorMode
    @Binding var draft: AuthorizationRelationDraft
    var onOpen: (String) -> Void = { _ in }
    var onEdit: ((AuthorizationMemberEditorMode, Set<String>) -> Void)?

    @State private var response: AuthorizationRelationPage?
    @State private var loadedKey: String?
    @State private var search = ""
    @State private var enabled = "all"
    @State private var sort = "name"
    @State private var descending = false
    @State private var page = 1
    @State private var selectedOnly = false
    @State private var selected: [String: AuthorizationRelationRow] = [:]
    @State private var loading = false
    @State private var error: String?
    @State private var retry = UUID()

    private var query: String { search.trimmingCharacters(in: .whitespacesAndNewlines) }
    private var key: String {
        store.listPageKey([group.id, String(group.revision), String(users), mode.rawValue, String(page), query, enabled, sort, String(descending), String(selectedOnly), String(store.isConnected), store.dataRefreshToken.uuidString, retry.uuidString])
    }
    private var local: Bool { selectedOnly || mode == .changes }
    private var localRows: [AuthorizationRelationRow] {
        let source = selectedOnly ? Array(selected.values) : draft.rows
        return source.filter { row in
            (query.isEmpty || row.name.localizedStandardContains(query) || row.id.localizedCaseInsensitiveContains(query))
                && (enabled == "all" || row.enabled == (enabled == "enabled"))
        }.sorted { a, b in
            let first = sort == "id" ? a.id : sort == "expires_at" ? (a.expiresAt ?? "~") : sort == "state" ? (a.state ?? "") : a.name
            let second = sort == "id" ? b.id : sort == "expires_at" ? (b.expiresAt ?? "~") : sort == "state" ? (b.state ?? "") : b.name
            return descending ? (first == second ? a.id > b.id : first > second) : (first == second ? a.id < b.id : first < second)
        }
    }
    private var total: Int { local ? localRows.count : (loadedKey == key ? response?.total ?? 0 : 0) }
    private var pages: Int { max(1, (total + 99) / 100) }
    private var rows: [AuthorizationRelationRow] {
        if local { return Array(localRows.dropFirst((min(page, pages) - 1) * 100).prefix(100)) }
        return loadedKey == key ? response?.rows ?? [] : []
    }
    private var selection: Binding<Set<String>> {
        Binding(get: { Set(selected.keys).intersection(rows.map(\.id)) }, set: { ids in
            let visible = Set(rows.map(\.id))
            for id in visible.subtracting(ids) { selected[id] = nil }
            for row in rows where ids.contains(row.id) { selected[row.id] = row }
        })
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            header
            filters
            HStack {
                Toggle(L10n.text("仅显示已选"), isOn: $selectedOnly)
                    .accessibilityIdentifier("authorization.members.selectedOnly")
                Spacer()
                Menu(L10n.text("选择")) {
                    Button(L10n.text("选择当前页")) { for row in rows { selected[row.id] = row } }
                    Button(L10n.text("取消全部选择")) { selected = [:] }
                }
                Text(users ? L10n.text("已选 {0} 人", String(selected.count)) : L10n.text("已选 {0} 项", String(selected.count))).foregroundStyle(.secondary)
                    .accessibilityIdentifier("authorization.members.selectionCount")
            }.font(.caption)
            memberTable
                .overlay {
                    if loading { ProgressView() }
                    else if rows.isEmpty, error == nil { Text(L10n.text("没有匹配项")).foregroundStyle(.secondary) }
                }
            if let error {
                HStack {
                    Text(error).foregroundStyle(.red).lineLimit(2).help(error)
                    Button(L10n.text("重试")) { retry = UUID() }.disabled(!store.isConnected)
                }
            }
            AuthorizationPagination(page: Binding(get: { min(page, pages) }, set: { page = $0 }), pages: pages, total: total, disabled: loading || !store.isConnected, identifier: "authorization.members")
            if editing {
                Text(L10n.text("选择会跨搜索和分页保留；添加、移除只修改草稿，保存后才会生效。"))
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
        .onChange(of: search) { _, _ in page = 1 }
        .onChange(of: enabled) { _, _ in page = 1 }
        .onChange(of: sort) { _, _ in page = 1 }
        .onChange(of: descending) { _, _ in page = 1 }
        .onChange(of: selectedOnly) { _, _ in page = 1 }
        .onChange(of: mode) { _, _ in page = 1; selected = [:]; selectedOnly = false }
        .onChange(of: store.serviceAddress) { _, _ in response = nil; loadedKey = nil; selected = [:] }
        .task(id: key) { await load() }
    }

    private var header: some View {
        HStack {
            if editing {
                Picker(L10n.text("成员管理"), selection: $mode) {
                    Text(L10n.text("成员")).tag(AuthorizationMemberEditorMode.members)
                    Text(L10n.text("添加成员")).tag(AuthorizationMemberEditorMode.add)
                    Text(L10n.text("仅看变更（{0}）", String(draft.rows.count))).tag(AuthorizationMemberEditorMode.changes)
                }.pickerStyle(.segmented).accessibilityIdentifier("authorization.members.mode")
                Button(selectionActionTitle) { applySelection() }
                    .disabled(selected.isEmpty || loading)
                    .accessibilityIdentifier(mode == .changes ? "authorization.members.undo" : mode == .add ? "authorization.members.add" : "authorization.members.remove")
            } else {
                Text(users ? L10n.text("成员用户（{0}）", String(group.userCount)) : L10n.text("授权节点（{0}）", String(group.nodeCount))).font(.headline)
                Spacer()
                if let onEdit {
                    Button(L10n.text("添加成员")) { onEdit(.add, []) }
                        .accessibilityIdentifier("authorizationGroups.addMembers")
                    Button(L10n.text("移除所选 {0} 人", String(selected.count)), role: .destructive) { onEdit(.changes, Set(selected.keys)) }
                        .disabled(selected.isEmpty).accessibilityIdentifier("authorizationGroups.removeMembers")
                }
            }
        }
    }

    private var selectionActionTitle: String {
        if mode == .changes { return L10n.text("撤销所选变更") }
        if mode == .add {
            return users ? L10n.text("添加所选 {0} 人", String(selected.count)) : L10n.text("添加所选 {0} 项", String(selected.count))
        }
        return users ? L10n.text("移除所选 {0} 人", String(selected.count)) : L10n.text("移除所选 {0} 项", String(selected.count))
    }

    private var filters: some View {
        HStack {
            TextField(L10n.text("搜索"), text: $search).textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("authorization.members.search")
            if users {
                Picker(L10n.text("状态"), selection: $enabled) {
                    Text(L10n.text("全部")).tag("all")
                    Text(L10n.text("已启用")).tag("enabled")
                    Text(L10n.text("已停用")).tag("disabled")
                }.frame(width: 130)
            }
            Picker(L10n.text("排序"), selection: $sort) {
                Text(L10n.text("名称")).tag("name")
                Text("ID").tag("id")
                if users { Text(L10n.text("到期时间")).tag("expires_at") }
                else { Text(L10n.text("状态")).tag("state") }
            }.frame(width: 150)
            Button { descending.toggle() } label: { Image(systemName: descending ? "arrow.down" : "arrow.up") }
                .accessibilityLabel(L10n.text("排序"))
        }
    }

    private var memberTable: some View {
        GeometryReader { geometry in
            table(width: geometry.size.width)
        }.frame(minHeight: 100)
    }

    private func table(width: CGFloat) -> some View {
        let available = max(width - (editing ? 112 : 96), 310)
        let idWidth = min(160, max(70, available * 0.23))
        let expiryWidth = users ? min(160, max(90, available * 0.27)) : 0
        let nameWidth = max(90, available - idWidth - expiryWidth - 60 - (editing ? 60 : 0))
        return Table(rows, selection: selection) {
            TableColumn(L10n.text("名称")) { row in
                Text(row.name).lineLimit(1).help(row.name).accessibilityIdentifier("authorizationGroups.member.\(row.id)")
            }.width(min: 90, ideal: nameWidth, max: nameWidth)
            TableColumn("ID") { row in
                Text(row.id).font(.caption.monospaced()).lineLimit(1).truncationMode(.middle).help(row.id)
            }.width(min: 70, ideal: idWidth, max: idWidth)
            TableColumn(L10n.text("状态")) { row in
                Text(row.enabled.map { $0 ? L10n.text("已启用") : L10n.text("已停用") } ?? row.state ?? L10n.text("—"))
            }.width(60)
            if users {
                TableColumn(L10n.text("到期时间")) { row in Text(row.expiresAt.map { DateDisplayText.local($0) } ?? L10n.text("—")) }
                    .width(min: 90, ideal: expiryWidth, max: expiryWidth)
            }
            if editing {
                TableColumn(L10n.text("变更")) { row in
                    Text(draft.added[row.id] != nil ? L10n.text("待添加") : draft.removed[row.id] != nil ? L10n.text("待移除") : "")
                }.width(60)
            }
        }
        .frame(minHeight: 100)
        .accessibilityIdentifier("authorization.members.table")
        .contextMenu(forSelectionType: String.self) { ids in
            if ids.count == 1, let id = ids.first { Button(L10n.text("查看详情")) { onOpen(id) } }
            if !editing, let onEdit, !ids.isEmpty { Button(L10n.text("移除所选 {0} 人", String(ids.count)), role: .destructive) { onEdit(.changes, ids) } }
        } primaryAction: { ids in if ids.count == 1, let id = ids.first { onOpen(id) } }
    }

    private func applySelection() {
        if mode == .changes { draft.undo(Set(selected.keys)) }
        else {
            for row in selected.values { draft.setMember(mode == .add, row: row, originallyMember: mode == .members) }
        }
        selected = [:]
    }

    private func load() async {
        let request = key
        guard !local else { loading = false; error = nil; return }
        guard store.isConnected else { loading = false; error = L10n.text("管理服务当前不可用；恢复连接后才能读取详情或提交写操作。"); return }
        loading = true; error = nil
        defer { if request == key { loading = false } }
        do {
            if !query.isEmpty { try await Task.sleep(for: .milliseconds(250)) }
            let value = try await store.authorizationRelationPage(groupID: group.id, users: users, page: page, query: query, member: mode != .add, sort: sort, order: descending ? "desc" : "asc", enabled: enabled == "all" ? nil : enabled == "enabled")
            guard !Task.isCancelled, request == key else { return }
            if editing, value.revision != group.revision {
                error = L10n.text("授权组版本已变化，请重新打开编辑。")
            }
            page = value.page; response = value; loadedKey = key
        } catch {
            guard !Task.isCancelled, !isRequestCancellation(error), request == key else { return }
            self.error = error.localizedDescription
        }
    }
}

struct AuthorizationPagination: View {
    @Binding var page: Int
    let pages: Int
    let total: Int
    var disabled = false
    var identifier = "authorization"

    var body: some View {
        HStack(spacing: 8) {
            Button { page = max(1, page - 1) } label: { Image(systemName: "chevron.left") }
                .disabled(disabled || page <= 1).accessibilityLabel(L10n.text("上一页"))
                .accessibilityIdentifier("\(identifier).previousPage")
            Text("\(page) / \(pages)").monospacedDigit()
            Button { page = min(pages, page + 1) } label: { Image(systemName: "chevron.right") }
                .disabled(disabled || page >= pages).accessibilityLabel(L10n.text("下一页"))
                .accessibilityIdentifier("\(identifier).nextPage")
            Spacer(minLength: 0)
            Text("\(total)").foregroundStyle(.secondary).monospacedDigit()
        }.font(.caption).accessibilityElement(children: .contain).accessibilityIdentifier("\(identifier).pagination")
    }
}
