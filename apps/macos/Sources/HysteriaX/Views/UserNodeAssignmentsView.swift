import SwiftUI
import UniformTypeIdentifiers

struct UserNodeAssignmentsView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ManagementStore
    let user: UserSummary

    @State private var selected: Set<String>
    @State private var applied: Set<String>
    @State private var revision: Int
    @State private var searchText = ""
    @State private var mtlsSelections: [String: String] = [:]
    @State private var isSaving = false
    @State private var errorMessage: String?
    @State private var credentials: [String: String] = [:]
    @State private var showingRemovalConfirmation = false

    init(store: ManagementStore, user: UserSummary) {
        self.store = store
        self.user = user
        let assigned = Set(user.assignments.map(\.nodeID))
        _selected = State(initialValue: assigned)
        _applied = State(initialValue: assigned)
        _revision = State(initialValue: user.revision)
    }

    private var additions: Set<String> { selected.subtracting(applied) }
    private var removals: Set<String> { applied.subtracting(selected) }
    private var hasChanges: Bool { selected != applied }
    private var visibleNodes: [NodeSummary] {
        store.nodes.filter { searchText.isEmpty || $0.name.localizedCaseInsensitiveContains(searchText) }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("分配节点").font(.title.bold())
            Text("为 \(user.name) 勾选可用节点，取消勾选即可移除。")
                .foregroundStyle(.secondary)
            TextField("搜索节点", text: $searchText)
                .textFieldStyle(.roundedBorder)
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 12) {
                    ForEach(visibleNodes) { node in
                        nodeRow(node)
                        Divider()
                    }
                    if visibleNodes.isEmpty {
                        Text(store.nodes.isEmpty ? "暂无节点，请先添加节点。" : "没有匹配的节点。")
                            .foregroundStyle(.secondary)
                            .frame(maxWidth: .infinity, minHeight: 80)
                    }
                }
            }
            .frame(minHeight: 180)
            Text("新增 \(additions.count) 个 · 移除 \(removals.count) 个 · 已选择 \(selected.count) 个")
                .font(.callout)
                .accessibilityIdentifier("user.assignments.changes")
            Text("移除后会撤销该用户在节点上的访问权限，并排队断开现有连接。")
                .font(.caption).foregroundStyle(.secondary)
            if !credentials.isEmpty {
                GroupBox("新节点连接密码（请及时保存）") {
                    ScrollView {
                        Text(credentials.keys.sorted().map { "\(nodeName($0)): \(credentials[$0] ?? "")" }.joined(separator: "\n"))
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .frame(maxHeight: 80)
                }
            }
            if let errorMessage {
                Text(errorMessage).font(.callout).foregroundStyle(.red)
            }
            HStack {
                Button(hasChanges ? "取消" : "完成") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Spacer()
                if isSaving { ProgressView().controlSize(.small) }
                Button(isSaving ? "正在保存…" : "保存更改") {
                    if removals.isEmpty { save() }
                    else { showingRemovalConfirmation = true }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!hasChanges || !store.isConnected)
                .accessibilityIdentifier("user.assignments.save")
            }
        }
        .padding(24)
        .frame(width: 580, height: 620)
        .disabled(isSaving)
        .interactiveDismissDisabled(isSaving)
        .confirmationDialog("移除 \(removals.count) 个节点？", isPresented: $showingRemovalConfirmation, titleVisibility: .visible) {
            Button("保存并移除节点", role: .destructive) { save() }
        } message: {
            Text("\(user.name) 将无法继续使用这些节点，现有连接会被排队断开。")
        }

    }

    private func nodeRow(_ node: NodeSummary) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Toggle(node.name, isOn: Binding(
                    get: { selected.contains(node.id) },
                    set: { checked in
                        if checked { selected.insert(node.id) }
                        else { selected.remove(node.id) }
                    }
                ))
                .toggleStyle(.checkbox)
                .accessibilityIdentifier("user.assignments.node.\(node.id)")
                Spacer()
                Text(additions.contains(node.id) ? "待分配" : removals.contains(node.id) ? "待移除" : applied.contains(node.id) ? "已分配" : "未分配")
                    .font(.caption)
                    .foregroundStyle(removals.contains(node.id) ? Color.red : Color.secondary)
            }
            if additions.contains(node.id) {
                DisclosureGroup("mTLS 客户端证书（普通节点可留空）") {
                    VStack(alignment: .leading, spacing: 8) {
                        CredentialPickerView(store: store, selection: Binding(get: { mtlsSelections[node.id] ?? "" }, set: { mtlsSelections[node.id] = $0 }), kinds: ["tls_identity"], ownerUserID: user.id, title: "mTLS 凭据")
                        Text("启用 mTLS 的节点需选择匹配的证书和私钥。")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    .padding(.top, 8)
                }
                .padding(.leading, 20)
            }
        }
    }

    private func nodeName(_ id: String) -> String {
        store.nodes.first { $0.id == id }?.name ?? String(id.prefix(8))
    }

    private func save() {
        // Apply additions first, so a failed assignment does not revoke existing access.
        let operations = additions.sorted().map { ($0, true) } + removals.sorted().map { ($0, false) }
        isSaving = true
        errorMessage = nil
        Task {
            defer { isSaving = false }
            var currentNodeID = ""
            do {
                for (id, assigned) in operations {
                    currentNodeID = id
                    let result = try await store.setNodeAssignment(
                        userID: user.id, nodeID: id, expectedRevision: revision, assigned: assigned,
                        mtlsCredentialID: mtlsSelections[id]
                    )
                    revision = result.revision
                    if assigned { applied.insert(id) }
                    else { applied.remove(id); credentials.removeValue(forKey: id) }
                    if let credential = result.credential { credentials[id] = credential }
                }
                await store.refresh()
                if credentials.isEmpty { dismiss() }
            } catch {
                await store.refresh()
                errorMessage = "\(nodeName(currentNodeID))：\(error.localizedDescription) 已完成的更改已保留；其余更改尚未提交。若数据已被其他操作修改，请重新打开弹窗。"
            }
        }
    }
}
