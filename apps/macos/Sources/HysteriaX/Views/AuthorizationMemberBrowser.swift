import SwiftUI

/// Shared, bounded member table for browsing, adding and reviewing changes.
struct AuthorizationMemberBrowser: View {
    let rows: [AuthorizationMemberRow]
    @Binding var selection: Set<String>
    var showsChanges = false
    var minimumTableHeight: CGFloat = 180
    var onOpen: ((String) -> Void)?
    var onRemove: ((Set<String>) -> Void)?
    var onUndo: ((Set<String>) -> Void)?

    @State private var searchText = ""
    @State private var statusFilter = "all"
    @State private var selectedOnly = false
    @State private var page = 0
    @State private var sortOrder = [KeyPathComparator(\AuthorizationMemberRow.name), KeyPathComparator(\AuthorizationMemberRow.id)]
    private let pageSize = 100

    private var filteredRows: [AuthorizationMemberRow] {
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        return rows.filter { row in
            (query.isEmpty || row.name.localizedStandardContains(query) || row.id.localizedCaseInsensitiveContains(query))
                && (statusFilter == "all" || row.enabled == (statusFilter == "enabled"))
                && (!selectedOnly || selection.contains(row.id))
        }.sorted(using: sortOrder)
    }

    var body: some View {
        let filtered = filteredRows
        let pageCount = max(1, (filtered.count + pageSize - 1) / pageSize)
        let currentPage = min(page, pageCount - 1)
        let visible = Array(filtered.dropFirst(currentPage * pageSize).prefix(pageSize))
        let visibleIDs = Set(visible.map(\.id))
        VStack(alignment: .leading, spacing: 10) {
            DetailHeaderLayout(spacing: 8, stackedSpacing: 8) {
                TextField(L10n.text("搜索成员名称或 ID"), text: $searchText)
                    .textFieldStyle(.roundedBorder)
                    .frame(minWidth: 160, maxWidth: .infinity)
                    .accessibilityIdentifier("authorization.members.search")
                HStack(spacing: 8) {
                    Picker(L10n.text("状态"), selection: $statusFilter) {
                        Text(L10n.text("全部状态")).tag("all")
                        Text(L10n.text("已启用")).tag("enabled")
                        Text(L10n.text("已停用")).tag("disabled")
                    }
                    .labelsHidden()
                    .frame(width: 130)
                    .accessibilityLabel(L10n.text("状态"))
                    Toggle(L10n.text("仅显示已选"), isOn: $selectedOnly)
                        .toggleStyle(.checkbox)
                        .accessibilityIdentifier("authorization.members.selectedOnly")
                }
                .fixedSize(horizontal: true, vertical: false)
            }
            HStack {
                Text(L10n.text("匹配 {0} 人 · 已选 {1} 人", String(filtered.count), String(selection.count)))
                    .font(.caption).foregroundStyle(.secondary)
                    .accessibilityIdentifier("authorization.members.selectionCount")
                Spacer()
                Menu(L10n.text("选择")) {
                    Button(L10n.text("选择当前页 {0} 人", String(visible.count))) { selection.formUnion(visibleIDs) }
                    Button(L10n.text("选择全部匹配 {0} 人", String(filtered.count))) { selection.formUnion(filtered.map(\.id)) }
                    Button(L10n.text("清除选择")) { selection = [] }
                }
                .accessibilityIdentifier("authorization.members.selectMenu")
            }
            GeometryReader { geometry in
                memberTable(rows: visible, visibleIDs: visibleIDs, width: floor(geometry.size.width / 32) * 32)
                    .frame(width: geometry.size.width, height: geometry.size.height)
            }
            .frame(maxWidth: .infinity, minHeight: minimumTableHeight, maxHeight: .infinity)
            .accessibilityIdentifier("authorization.members.table")
            HStack {
                Text(L10n.text("第 {0} / {1} 页 · 每页最多 {2} 人", String(currentPage + 1), String(pageCount), String(pageSize)))
                    .font(.caption).foregroundStyle(.secondary)
                Spacer()
                Button(L10n.text("上一页")) { page = currentPage - 1 }
                    .disabled(currentPage == 0)
                    .accessibilityIdentifier("authorization.members.previousPage")
                Button(L10n.text("下一页")) { page = currentPage + 1 }
                    .disabled(currentPage + 1 >= pageCount)
                    .accessibilityIdentifier("authorization.members.nextPage")
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .onChange(of: searchText) { _, _ in page = 0 }
        .onChange(of: statusFilter) { _, _ in page = 0 }
        .onChange(of: selectedOnly) { _, _ in page = 0 }
        .onChange(of: sortOrder) { _, _ in page = 0 }
        .onChange(of: rows.map(\.id)) { _, ids in
            selection.formIntersection(ids)
            page = min(page, max(0, (filteredRows.count - 1) / pageSize))
        }
    }

    private func memberTable(rows: [AuthorizationMemberRow], visibleIDs: Set<String>, width: CGFloat) -> some View {
        // Native columns add cell padding; also leave room for the scrollbar and borders.
        let available = max(width - (showsChanges ? 112 : 96), showsChanges ? 370 : 310)
        let idWidth = min(160, max(70, available * 0.23))
        let expiryWidth = min(160, max(90, available * 0.27))
        let nameWidth = max(90, available - idWidth - expiryWidth - 60 - (showsChanges ? 60 : 0))
        return Table(rows, selection: Binding(
            get: { selection.intersection(visibleIDs) },
            set: { selection = AuthorizationMemberSelection.merging($0, visibleIDs: visibleIDs, into: selection) }
        ), sortOrder: $sortOrder) {
            TableColumn(L10n.text("名称"), value: \.name) { row in
                Text(row.name).lineLimit(1).help(row.name).accessibilityIdentifier("authorizationGroups.member.\(row.id)")
            }
            .width(min: 90, ideal: nameWidth, max: nameWidth)
            TableColumn(L10n.text("用户 ID"), value: \.id) { row in
                Text(row.id).font(.caption.monospaced()).lineLimit(1).truncationMode(.middle)
                    .textSelection(.enabled).help(row.id)
            }
            .width(min: 70, ideal: idWidth, max: idWidth)
            TableColumn(L10n.text("状态"), value: \.status)
                .width(60)
            TableColumn(L10n.text("到期时间"), value: \.expiresAt) { row in
                Text(row.expiresAt.isEmpty ? L10n.text("—") : DateDisplayText.local(row.expiresAt))
            }
            .width(min: 90, ideal: expiryWidth, max: expiryWidth)
            if showsChanges {
                TableColumn(L10n.text("变更"), value: \.change)
                    .width(60)
            }
        }
        .contextMenu(forSelectionType: String.self) { ids in
            if let id = ids.first, ids.count == 1, let onOpen {
                Button(L10n.text("查看用户")) { onOpen(id) }
            }
            if !ids.isEmpty, let onRemove {
                Button(L10n.text("移除所选 {0} 人", String(ids.count)), role: .destructive) { onRemove(ids) }
            }
            if !ids.isEmpty, let onUndo {
                Button(L10n.text("撤销所选变更")) { onUndo(ids) }
            }
        } primaryAction: { ids in
            if ids.count == 1, let id = ids.first { onOpen?(id) }
        }
        .overlay {
            if rows.isEmpty {
                ContentUnavailableView(L10n.text("没有匹配的成员"), systemImage: "person.2")
            }
        }
    }

}
