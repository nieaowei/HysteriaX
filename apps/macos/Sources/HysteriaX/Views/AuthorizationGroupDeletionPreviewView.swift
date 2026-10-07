import SwiftUI

struct AuthorizationGroupDeletionPreviewView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ManagementStore
    @State private var group: AuthorizationGroupSummary
    @State private var preview: AuthorizationChangePreview
    var onDeleted: () -> Void = {}

    @State private var isDeleting = false
    @State private var result: AuthorizationGroupMutationResponse?
    @State private var errorMessage: String?

    init(store: ManagementStore, group: AuthorizationGroupSummary, preview: AuthorizationChangePreview, onDeleted: @escaping () -> Void = {}) {
        self.store = store
        _group = State(initialValue: group)
        _preview = State(initialValue: preview)
        self.onDeleted = onDeleted
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(result == nil ? L10n.text("删除授权组") : L10n.text("删除结果"))
                .font(.title2.bold())
            if let result {
                AuthorizationMutationReceiptView(
                    store: store,
                    createdCredentials: result.createdCredentials,
                    revocationJobIDs: result.revocationJobIds
                )
            } else {
                Text(L10n.text("将删除“{0}”。以下是失去最后一个授权来源的用户—节点连接；其他授权组提供的权限会继续保留。", String(describing: (group.name))))
                    .foregroundStyle(.secondary)
                Label(L10n.text("{0} 条有效连接将撤权", String(preview.removalsCount)), systemImage: "minus.circle")
                    .font(.callout.weight(.medium))
                if preview.removals.isEmpty {
                    Text(L10n.text("没有连接会失去访问权限。"))
                        .font(.callout).foregroundStyle(.secondary)
                } else {
                    DisclosureGroup(L10n.text("查看将撤权的连接（{0}）", String(preview.removals.count)), isExpanded: .constant(true)) {
                        ScrollView {
                            VStack(alignment: .leading, spacing: 8) {
                                ForEach(Array(preview.removals.enumerated()), id: \.offset) { _, pair in
                                    HStack {
                                        Text(userName(pair.userId))
                                        Image(systemName: "arrow.right").foregroundStyle(.secondary)
                                        Text(nodeName(pair.nodeID))
                                    }
                                    .font(.callout)
                                }
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .frame(maxHeight: 180)
                        .padding(.top, 8)
                    }
                }
            }
            if let errorMessage {
                Text(errorMessage).foregroundStyle(.red).textSelection(.enabled)
                Button(L10n.text("预览影响")) {
                    isDeleting = true
                    Task {
                        defer { isDeleting = false }
                        do {
                            group = try await store.authorizationGroupDetail(group.id)
                            preview = try await store.previewAuthorizationGroupDeletion(group)
                            self.errorMessage = nil
                        } catch { self.errorMessage = error.localizedDescription }
                    }
                }
                .disabled(isDeleting || !store.isConnected)
            }
            HStack {
                Button(result == nil ? L10n.text("取消") : L10n.text("关闭"), role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Spacer()
                if result == nil {
                    Button(isDeleting ? L10n.text("正在删除…") : L10n.text("删除授权组"), role: .destructive) {
                        delete()
                    }
                    .disabled(isDeleting || !store.isConnected || errorMessage != nil)
                    .keyboardShortcut(.defaultAction)
                }
            }
        }
        .padding(24)
        .frame(width: 620, height: 520)
        .interactiveDismissDisabled(isDeleting)
    }

    private func delete() {
        isDeleting = true
        errorMessage = nil
        Task {
            defer { isDeleting = false }
            do {
                result = try await store.deleteAuthorizationGroup(group, previewToken: preview.previewToken)
                onDeleted()
            } catch {
                errorMessage = L10n.text("预览之后授权数据可能已变化。草稿仍保留，请重新读取预览再试。\n{0}", String(describing: (error.localizedDescription)))
            }
        }
    }

    private func userName(_ id: String) -> String {
        store.users.first(where: { $0.id == id })?.name ?? id
    }

    private func nodeName(_ id: String) -> String {
        store.nodes.first(where: { $0.id == id })?.name ?? id
    }
}
