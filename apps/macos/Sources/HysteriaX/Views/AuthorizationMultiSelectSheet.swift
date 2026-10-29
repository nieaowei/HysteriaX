import SwiftUI

private struct AuthorizationPickerOption: Identifiable {
    let id: String
    let name: String
    let detail: String?
}

/// A group picker whose search and page bounds are applied by the server.
struct AuthorizationMultiSelectSheet: View {
    @Environment(\.dismiss) private var dismiss
    let title: String
    let store: ManagementStore
    @Binding var selection: Set<String>
    var onNamesChanged: ([String: String]) -> Void
    @State private var search = ""
    @State private var selectedOnly = false
    @State private var names: [String: String]
    @State private var page = 1
    @State private var response: AuthorizationGroupsPage?
    @State private var loadedKey: String?
    @State private var draftSelection: Set<String>
    @State private var loading = false
    @State private var error: String?
    @State private var retry = UUID()

    init(title: String, store: ManagementStore, selection: Binding<Set<String>>, initialNames: [String: String], onNamesChanged: @escaping ([String: String]) -> Void) {
        self.title = title; self.store = store; self._selection = selection
        self.onNamesChanged = onNamesChanged
        _draftSelection = State(initialValue: selection.wrappedValue)
        _names = State(initialValue: initialNames)
    }

    private var key: String { store.listPageKey([search, String(page), String(selectedOnly), String(store.isConnected), retry.uuidString]) }
    private var selectedRows: [AuthorizationPickerOption] {
        draftSelection.map { AuthorizationPickerOption(id: $0, name: names[$0] ?? $0, detail: nil) }
            .filter { search.isEmpty || $0.name.localizedStandardContains(search) || $0.id.localizedCaseInsensitiveContains(search) }
            .sorted { $0.name == $1.name ? $0.id < $1.id : $0.name < $1.name }
    }
    private var total: Int { selectedOnly ? selectedRows.count : response?.total ?? 0 }
    private var pages: Int { max(1, (total + 49) / 50) }
    private var rows: [AuthorizationPickerOption] {
        if selectedOnly { return Array(selectedRows.dropFirst((min(page, pages) - 1) * 50).prefix(50)) }
        return loadedKey == key ? (response?.items ?? []).map { .init(id: $0.id, name: $0.name, detail: L10n.text("{0} 位用户 · {1} 个节点", String($0.userCount), String($0.nodeCount))) } : []
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(title).font(.headline)
            TextField(L10n.text("搜索"), text: $search).textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("authorization.selection.search")
            Toggle(L10n.text("仅显示已选"), isOn: $selectedOnly).accessibilityIdentifier("authorization.selectedOnly")
            HStack {
                Text(L10n.text("已选 {0} 项", String(draftSelection.count)))
                Spacer()
                Button(L10n.text("选择当前页")) {
                    draftSelection.formUnion(rows.map(\.id))
                    for row in rows { names[row.id] = row.name }
                }
                    .disabled(loading || rows.isEmpty).accessibilityIdentifier("authorization.selectAllFiltered")
                Button(L10n.text("取消全部选择")) { draftSelection = [] }
            }
            List(rows) { group in
                Toggle(isOn: Binding(get: { draftSelection.contains(group.id) }, set: { selected in
                    if selected { draftSelection.insert(group.id); names[group.id] = group.name } else { draftSelection.remove(group.id) }
                })) {
                    VStack(alignment: .leading) {
                        Text(group.name)
                        if let detail = group.detail { Text(detail).font(.caption).foregroundStyle(.secondary) }
                    }
                }.accessibilityIdentifier("authorization.selection.\(group.id)")
            }.overlay { if loading { ProgressView() } }
            if let error { HStack { Text(error).foregroundStyle(.red); Button(L10n.text("重试")) { retry = UUID() } } }
            AuthorizationPagination(page: $page, pages: pages, total: total, disabled: loading || !store.isConnected, identifier: "authorization.selection")
            HStack {
                Button(L10n.text("取消"), role: .cancel) { dismiss() }.keyboardShortcut(.cancelAction)
                    .accessibilityIdentifier("authorization.selection.cancel")
                Spacer()
                Button(L10n.text("完成")) { selection = draftSelection; onNamesChanged(names.filter { draftSelection.contains($0.key) }); dismiss() }.keyboardShortcut(.defaultAction)
                    .accessibilityIdentifier("authorization.selection.done")
            }
        }
        .padding(20).frame(width: 600, height: 500)
        .onChange(of: selectedOnly) { _, _ in page = 1 }
        .onChange(of: search) { _, _ in page = 1 }
        .onChange(of: store.serviceAddress) { _, _ in dismiss() }
        .task(id: key) { await load() }
    }

    private func load() async {
        let request = key
        guard !selectedOnly else { loading = false; error = nil; return }
        guard store.isConnected else { loading = false; return }
        loading = true; error = nil
        defer { if request == key { loading = false } }
        do {
            if !search.isEmpty { try await Task.sleep(for: .milliseconds(250)) }
            let result = try await store.authorizationGroupsPage(page: page, query: search)
            guard !Task.isCancelled, request == key else { return }
            page = result.page; response = result; loadedKey = key
        } catch {
            guard !Task.isCancelled, !isRequestCancellation(error), request == key else { return }
            self.error = error.localizedDescription
        }
    }
}
