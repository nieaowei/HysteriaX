import SwiftUI

struct AuthorizationPickerOption: Hashable, Identifiable {
    let id: String
    let name: String
    let detail: String?
}

struct AuthorizationMultiSelectSheet: View {
    @Environment(\.dismiss) private var dismiss
    let title: String
    let options: [AuthorizationPickerOption]
    @Binding var selection: Set<String>

    @State private var searchText = ""
    @State private var selectedOnly = false
    @State private var draftSelection: Set<String>

    init(title: String, options: [AuthorizationPickerOption], selection: Binding<Set<String>>) {
        self.title = title
        self.options = options
        self._selection = selection
        self._draftSelection = State(initialValue: selection.wrappedValue)
    }

    private var visibleOptions: [AuthorizationPickerOption] {
        options.filter { option in
            (!selectedOnly || draftSelection.contains(option.id))
                && (searchText.isEmpty
                    || option.name.localizedStandardContains(searchText)
                    || option.id.localizedCaseInsensitiveContains(searchText)
                    || option.detail?.localizedStandardContains(searchText) == true)
        }
    }

    var body: some View {
        NavigationStack {
            List {
                TextField(L10n.text("搜索"), text: $searchText)
                    .accessibilityIdentifier("authorization.selection.search")

                Section {
                    Toggle(L10n.text("仅显示已选"), isOn: $selectedOnly)
                        .accessibilityIdentifier("authorization.selectedOnly")
                    HStack {
                        Text(L10n.text("已选 {0} 项", String(draftSelection.count)))
                            .foregroundStyle(.secondary)
                        Spacer()
                        Button(L10n.text("选择当前筛选结果")) {
                            draftSelection.formUnion(visibleOptions.map(\.id))
                        }
                        .disabled(visibleOptions.isEmpty)
                        .accessibilityIdentifier("authorization.selectAllFiltered")
                    }
                }

                Section {
                    if visibleOptions.isEmpty {
                        ContentUnavailableView(
                            selectedOnly ? L10n.text("没有匹配的已选项") : L10n.text("没有匹配项"),
                            systemImage: "magnifyingglass"
                        )
                    }
                    ForEach(visibleOptions) { option in
                        Toggle(isOn: Binding(
                            get: { draftSelection.contains(option.id) },
                            set: { isSelected in
                                if isSelected { draftSelection.insert(option.id) }
                                else { draftSelection.remove(option.id) }
                            }
                        )) {
                            VStack(alignment: .leading, spacing: 3) {
                                Text(option.name)
                                if let detail = option.detail, !detail.isEmpty {
                                    Text(detail).font(.caption).foregroundStyle(.secondary)
                                }
                            }
                        }
                        .accessibilityIdentifier("authorization.selection.\(option.id)")
                    }
                }
            }
            .navigationTitle(title)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(L10n.text("取消")) { dismiss() }
                        .accessibilityIdentifier("authorization.selection.cancel")
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(L10n.text("完成")) {
                        selection = draftSelection
                        dismiss()
                    }
                        .keyboardShortcut(.defaultAction)
                        .accessibilityIdentifier("authorization.selection.done")
                }
            }
        }
        .frame(minWidth: 520, minHeight: 480)
        .onAppear {
            draftSelection = selection
            searchText = ""
            selectedOnly = false
        }
    }
}
