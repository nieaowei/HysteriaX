import SwiftUI

struct AuthorizationGroupDeletionView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ManagementStore
    let group: AuthorizationGroupSummary
    var onDeleted: () -> Void = {}

    @State private var isDeleting = false
    @State private var result: AuthorizationGroupMutationResponse?
    @State private var errorMessage: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(L10n.text("删除结果")).font(.title2.bold())
            if let result {
                Text(L10n.text("新增 {0} 条连接授权 · 移除 {1} 条连接授权", String(result.additionsCount), String(result.removalsCount)))
                    .font(.callout.weight(.medium))
                    .accessibilityIdentifier("authorizationGroups.delete.resultSummary")
                ScrollView {
                    AuthorizationMutationReceiptView(store: store, createdCredentials: result.createdCredentials, revocationJobIDs: result.revocationJobIds)
                }
            } else if isDeleting {
                ProgressView(L10n.text("正在删除…"))
            }
            if let errorMessage { Text(errorMessage).foregroundStyle(.red).textSelection(.enabled) }
            Spacer()
            HStack {
                Button(L10n.text("关闭"), role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                    .disabled(isDeleting)
                Spacer()
                if errorMessage != nil {
                    Button(L10n.text("重试")) { Task { await delete() } }
                        .disabled(isDeleting || !store.isConnected)
                }
            }
        }
        .padding(24)
        .frame(width: 620, height: 460)
        .interactiveDismissDisabled(isDeleting)
        .task { await delete() }
    }

    private func delete() async {
        guard !isDeleting, result == nil else { return }
        isDeleting = true
        errorMessage = nil
        defer { isDeleting = false }
        do {
            result = try await store.deleteAuthorizationGroup(group)
            onDeleted()
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}
