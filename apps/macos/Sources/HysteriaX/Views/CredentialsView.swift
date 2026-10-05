import SwiftUI

struct CredentialsView: View {
    @Bindable var store: ManagementStore
    var onJump: (String, String) -> Void
    @State private var selection: String?
    @State private var search = ""
    @State private var type = ""
    @State private var detail: CredentialDetail?
    @State private var creating = false
    @State private var replacing: CredentialDetail?
    @State private var editing: CredentialDetail?
    @State private var error: String?
    @State private var showAdminCreation = false
    @State private var adminLabel = ""
    @State private var createdToken: String?
    @State private var deleting = false

    private var entries: [CredentialSummary] {
        store.credentials.filter { (type.isEmpty || $0.kind == type) && (search.isEmpty || $0.name.localizedCaseInsensitiveContains(search)) }
    }
    private var selected: CredentialSummary? { store.credentials.first { $0.id == selection } }
    var body: some View {
        GeometryReader { geometry in
            HStack(spacing: 0) {
            VStack(spacing: 0) {
                HStack {
                    TextField("搜索凭据", text: $search)
                    Picker("类型", selection: $type) {
                        Text("全部类型").tag("")
                        ForEach(Array(Set(store.credentials.map(\.kind))).sorted(), id: \.self) { Text(CredentialDisplay.kind($0)).tag($0) }
                    }.frame(width: 180)
                }.padding(12)
                Table(entries, selection: $selection) {
                    TableColumn("名称", value: \.name)
                    TableColumn("类型", value: \.typeTitle)
                    TableColumn("状态", value: \.statusTitle)
                    TableColumn("引用") { entry in Text(entry.isManaged ? String(entry.referenceCount ?? 0) : "—") }
                    TableColumn("到期") { entry in Text(entry.expiresAt.map { DateDisplayText.local($0) } ?? "未知") }
                }.accessibilityIdentifier("credentials.table")
            }.frame(minWidth: 0, maxWidth: .infinity, maxHeight: .infinity)
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    if let selected {
                        Text(selected.name).font(.title2.bold()).lineLimit(3).truncationMode(.tail)
                        LabeledContent("类型", value: selected.typeTitle)
                        LabeledContent("状态", value: selected.statusTitle)
                        if selected.isManaged {
                            managedDetail
                        } else {
                            businessDetail(selected)
                        }
                    } else {
                        ContentUnavailableView("选择凭据", systemImage: "key.horizontal", description: Text("查看引用、版本和更新结果。"))
                    }
                    if let error { Text(error).foregroundStyle(.red) }
                }.padding(20).frame(minWidth: 0, maxWidth: .infinity, alignment: .leading)
            }.frame(width: min(420, max(280, geometry.size.width * 0.4)), height: geometry.size.height)
                .clipped()
            }
            .frame(width: geometry.size.width, height: geometry.size.height, alignment: .topLeading)
        }
        .toolbar {
            Button { creating = true } label: { Label("创建凭据", systemImage: "plus") }
                .disabled(!store.isConnected).keyboardShortcut("n", modifiers: .command)
                .accessibilityIdentifier("credential.create")
            Button("创建并切换管理员 Token") { showAdminCreation = true }.disabled(!store.isConnected)
        }
        .sheet(isPresented: $creating) { CredentialEditorView(store: store) }
        .sheet(item: $replacing) { CredentialEditorView(store: store, replacing: $0) }
        .sheet(item: $editing) { CredentialMetadataEditor(store: store, detail: $0) }
        .task(id: "\(selection ?? ""): \(store.lastUpdated?.timeIntervalSince1970 ?? 0):\(store.isConnected)") { await load() }
        .alert("创建并切换管理员 Token", isPresented: $showAdminCreation) {
            TextField("用途名称", text: $adminLabel)
            Button("取消", role: .cancel) {}
            Button("创建") { Task { do { createdToken = try await store.createAndSwitchAdminToken(label: adminLabel).token } catch { self.error = error.localizedDescription } } }
        }
        .sheet(isPresented: Binding(get: { createdToken != nil }, set: { if !$0 { createdToken = nil } })) {
            VStack(alignment: .leading, spacing: 20) {
                Text("管理员 Token（仅显示一次）").font(.title2.bold())
                Text(createdToken ?? "").font(.system(.body, design: .monospaced)).textSelection(.enabled)
                Button("已保存，关闭") { createdToken = nil }.keyboardShortcut(.defaultAction)
            }.padding(24).frame(width: 600)
        }
        .confirmationDialog("删除未被引用的凭据？", isPresented: $deleting) {
            Button("删除", role: .destructive) { if let detail { Task { do { try await store.deleteCredential(detail); selection = nil } catch { self.error = error.localizedDescription } } } }
        }
    }

    @ViewBuilder private var managedDetail: some View {
        if let detail {
            LabeledContent("最新版本", value: "v\(detail.latestVersion)")
            if let fingerprint = detail.metadata["fingerprint"]?.stringValue { Text(fingerprint).font(.system(.caption, design: .monospaced)).textSelection(.enabled) }
            if let certificate = detail.metadata["certificate"]?.stringValue {
                DisclosureGroup("公开证书") { Text(certificate).font(.system(.caption, design: .monospaced)).textSelection(.enabled) }
            }
            HStack {
                Button("发布新版本") { replacing = detail }.disabled(detail.archived)
                Button("编辑信息") { editing = detail }
                Button("删除", role: .destructive) { deleting = true }.disabled(!detail.references.isEmpty)
            }.disabled(!store.isConnected)
            GroupBox("引用与生效版本") {
                VStack(alignment: .leading, spacing: 8) {
                    if detail.references.isEmpty { Text("暂无引用").foregroundStyle(.secondary) }
                    ForEach(Array(detail.references.enumerated()), id: \.offset) { _, reference in
                        HStack {
                            if reference.entityType != "batch" { Button(reference.name ?? reference.entityID) { onJump(reference.entityType, reference.entityID) }.buttonStyle(.link) }
                            else { Text("更新批次") }
                            Spacer()
                            Text("\(CredentialDisplay.source(reference.source)) · v\(reference.version ?? 0)").font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }.frame(maxWidth: .infinity, alignment: .leading)
            }
            GroupBox("更新结果") {
                VStack(alignment: .leading, spacing: 12) {
                    if detail.batches.isEmpty { Text("暂无更新批次").foregroundStyle(.secondary) }
                    ForEach(detail.batches) { batch in
                        Text("v\(batch.version) · \(DateDisplayText.local(batch.createdAt))").font(.headline)
                        ForEach(Array(batch.items.enumerated()), id: \.offset) { _, item in
                            VStack(alignment: .leading) {
                                HStack { Button(item.name ?? item.nodeID) { onJump("node", item.nodeID) }.buttonStyle(.link); Spacer(); Text(JobDisplayText.status(item.status)) }
                                Button("查看任务") { onJump("job", item.jobId) }.buttonStyle(.link)
                                if let message = item.errorMessage { Text(message).font(.caption).foregroundStyle(.red) }
                            }
                        }
                        if batch.items.contains(where: { ["failed", "rolled_back", "cancelled"].contains($0.status) }) {
                            Button("重试失败项") { Task { do { try await store.retryCredentialBatch(batch.id); await load() } catch { self.error = error.localizedDescription } } }
                                .disabled(!store.isConnected || detail.archived || batch.version != detail.latestVersion)
                        }
                    }
                }.frame(maxWidth: .infinity, alignment: .leading)
            }
            DisclosureGroup("版本历史（\(detail.versions.count)）") {
                ForEach(detail.versions, id: \.version) { Text("v\($0.version) · \(DateDisplayText.local($0.createdAt))") }
            }
        } else { Text(store.isConnected ? "正在读取详情…" : "连接服务后可读取引用与版本详情。").foregroundStyle(.secondary) }
    }

    @ViewBuilder private func businessDetail(_ entry: CredentialSummary) -> some View {
        if let user = entry.ownerUserId { Button("管理用户凭据与订阅") { onJump("user", user) } }
        if entry.kind == "admin_token", let id = entry.metadata["token_id"]?.stringValue,
           let token = store.adminTokens.first(where: { $0.id == id }) {
            Text(id == store.currentAdminTokenID ? "本 Mac 当前使用的 Token" : "管理员访问令牌").foregroundStyle(.secondary)
            if let lastUsed = token.lastUsedAt { LabeledContent("最近使用", value: DateDisplayText.local(lastUsed)) }
            Button("撤销 Token", role: .destructive) { Task { do { try await store.revokeAdminToken(token) } catch { self.error = error.localizedDescription } } }
                .disabled(!store.isConnected || token.revokedAt != nil || id == store.currentAdminTokenID)
        }
    }

    private func load() async {
        guard let selected, selected.isManaged, store.isConnected else { detail = nil; return }
        do {
            let loaded = try await store.credentialDetail(selected.id)
            guard selection == loaded.id else { return }
            detail = loaded; error = nil
        } catch { self.error = error.localizedDescription }
    }
}

private struct CredentialMetadataEditor: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ManagementStore
    let detail: CredentialDetail
    @State private var name = ""
    @State private var archived = false
    @State private var hasReminder = false
    @State private var reminder = Date()
    @State private var error: String?
    @State private var saving = false
    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("凭据信息").font(.title2.bold())
            TextField("名称", text: $name)
            Toggle("归档（保留已有引用，禁止新增引用）", isOn: $archived)
            Toggle("设置到期提醒", isOn: $hasReminder)
            if hasReminder { DatePicker("提醒时间", selection: $reminder) }
            if let error { Text(error).foregroundStyle(.red) }
            HStack {
                Button("取消") { dismiss() }; Spacer()
                Button("保存") { Task {
                    saving = true; defer { saving = false }
                    do { try await store.updateCredential(detail, name: name, archived: archived, reminderAt: hasReminder ? ISO8601DateFormatter().string(from: reminder) : nil); dismiss() }
                    catch { self.error = error.localizedDescription }
                } }.disabled(saving || !store.isConnected)
            }
        }.padding(24).frame(width: 500)
        .onAppear { name = detail.name; archived = detail.archived; hasReminder = detail.reminderAt != nil; reminder = detail.reminderAt.flatMap(DateDisplayText.parse) ?? Date() }
    }
}
