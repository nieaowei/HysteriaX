import SwiftUI

struct UserAuthorizationGroupMembershipView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ManagementStore
    @State private var user: UserSummary

    @State private var groupIDs: Set<String>
    @State private var selectedNames: [String: String]
    @State private var receipt: AuthorizationChangeReceipt?
    @State private var selectingGroups = false
    @State private var isSaving = false
    @State private var errorMessage: String?

    init(store: ManagementStore, user: UserSummary) {
        self.store = store
        _user = State(initialValue: user)
        _selectedNames = State(initialValue: Dictionary((user.authorizationGroups ?? []).map { ($0.id, $0.name) }, uniquingKeysWith: { first, _ in first }))
        _groupIDs = State(initialValue: Set(user.authorizationGroups?.map(\.id) ?? []))
    }

    var body: some View {
        directoryContent
    }

    private var directoryContent: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(receipt == nil ? L10n.text("管理用户所属授权组") : L10n.text("授权变更已提交"))
                .font(.title2.bold())

            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
            if let receipt {
                AuthorizationMutationReceiptView(store: store, jobID: receipt.jobId)
                    .accessibilityIdentifier("users.groups.resultSummary")
            } else {
                Text(L10n.text("为 {0} 选择所属组。有效节点由所有授权组取并集；离开一个组后，只要其他组仍提供同一节点权限就不会撤权。", String(describing: (user.name))))
                    .font(.callout).foregroundStyle(.secondary)

                GroupBox {
                    VStack(alignment: .leading, spacing: 10) {
                        Text(L10n.text("已选 {0} 个授权组", String(groupIDs.count)))
                        HStack {
                            Text(selectedGroupNames)
                                .font(.caption).foregroundStyle(.secondary)
                                .lineLimit(3)
                            Spacer()
                            Button(L10n.text("选择授权组…")) { selectingGroups = true }
                                .accessibilityIdentifier("users.groups.select")
                                .disabled(isSaving)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.vertical, 4)
                } label: {
                    Label(L10n.text("所属授权组"), systemImage: "person.3")
                }

            }

                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(maxHeight: .infinity)

            if let errorMessage { Text(errorMessage).foregroundStyle(.red).textSelection(.enabled) }

            HStack {
                Button(receipt == nil ? L10n.text("取消") : L10n.text("完成"), role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                    .disabled(isSaving)
                Spacer()
                if receipt == nil {
                    Button(isSaving ? L10n.text("正在保存…") : L10n.text("保存所属组")) { save() }
                        .disabled(isSaving || !store.isConnected)
                        .keyboardShortcut(.defaultAction)
                        .accessibilityIdentifier("users.groups.save")
                }
            }
        }
        .padding(24)
        .frame(width: 740, height: 560)
        .interactiveDismissDisabled(isSaving)
        .sheet(isPresented: $selectingGroups) {
            AuthorizationMultiSelectSheet(
                title: L10n.text("选择授权组"),
                store: store,
                selection: $groupIDs,
                initialNames: selectedNames,
                onNamesChanged: { selectedNames = $0 }
            )
        }
        .onChange(of: groupIDs) { _, _ in
            errorMessage = nil
        }
    }

    private var selectedGroupNames: String {
        let names = groupIDs.sorted().map { id in
            selectedNames[id] ?? id
        }
        return names.isEmpty ? L10n.text("尚未选择授权组") : names.joined(separator: "、")
    }

    private func save() {
        guard !isSaving else { return }
        isSaving = true
        errorMessage = nil
        Task {
            defer { isSaving = false }
            do {
                receipt = try await store.updateUserAuthorizationGroups(user, groupIDs: groupIDs.sorted())
            } catch {
                errorMessage = L10n.text("保存失败，草稿已保留。请检查错误后重试；如有版本冲突，请重新打开编辑。\n{0}", String(describing: (error.localizedDescription)))
            }
        }
    }

}
