import SwiftUI
import UniformTypeIdentifiers

struct CAIdentityEditorView: View {
    enum Operation { case create, publish, policy }
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ManagementStore
    var detail: CredentialDetail? = nil
    var operation = Operation.create
    var onSaved: ((String) -> Void)? = nil
    @State private var name = ""
    @State private var source = "generated"
    @State private var certificate = ""
    @State private var privateKey = ""
    @State private var validity = 180
    @State private var renewBefore = 720
    @State private var replacingBlockedTarget = false
    @State private var idempotencyKey = UUID().uuidString
    @State private var saving = false
    @State private var error: String?
    @State private var importing = false
    @State private var importField = "certificate"

    private var blockedRotation: CARotationState? {
        detail?.batches.compactMap(\.caRotation).first { $0.phase != "complete" }
    }
    private var importsMaterial: Bool { operation != .policy && source == "imported" }
    private var policyValid: Bool { (1...365).contains(validity) && renewBefore >= 1 && renewBefore < validity * 24 }
    private var title: String {
        switch operation {
        case .create: L10n.text("创建 CA 身份")
        case .publish: L10n.text("发布 CA 新版本")
        case .policy: L10n.text("客户端证书策略")
        }
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(title).font(.title2.bold())
            Form {
                if operation == .create {
                    TextField(L10n.text("名称"), text: $name).accessibilityIdentifier("ca.editor.name")
                    Picker(L10n.text("CA 来源"), selection: $source) {
                        Text(L10n.text("系统生成")).tag("generated")
                        Text(L10n.text("导入证书链与私钥")).tag("imported")
                    }.accessibilityIdentifier("ca.editor.source")
                } else if let detail {
                    LabeledContent(L10n.text("CA 身份"), value: detail.name)
                    LabeledContent(L10n.text("CA 来源"), value: source == "generated" ? L10n.text("系统生成") : L10n.text("导入证书链与私钥"))
                }
                if importsMaterial {
                    importedText(L10n.text("CA 证书链 PEM"), text: $certificate, field: "certificate")
                    importedText(L10n.text("私钥 PEM"), text: $privateKey, field: "private_key")
                    Text(L10n.text("首张证书是签发 CA，后续为完整上级链；节点仅信任选定签发 CA。私钥保存后不可查看。"))
                        .font(.caption).foregroundStyle(.secondary)
                } else if operation != .policy {
                    Text(L10n.text("生成 ECDSA P-256 CA，有效期 10 年，提前一年自动轮换。"))
                        .font(.callout).foregroundStyle(.secondary)
                }
                if operation != .publish {
                    Section(L10n.text("客户端证书策略")) {
                        TextField(L10n.text("有效期（天）"), value: $validity, format: .number)
                            .accessibilityIdentifier("ca.editor.validity")
                        TextField(L10n.text("提前续期（小时）"), value: $renewBefore, format: .number)
                            .accessibilityIdentifier("ca.editor.renewBefore")
                        Text(L10n.text("有效期 1–365 天；提前续期至少 1 小时且小于有效期。策略修改只影响后续签发，不创建 CA 版本或部署节点。"))
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
                if operation == .publish {
                    Text(L10n.text("先更新全部引用节点的信任，再发布共享用户证书。离线节点会阻止发布；旧信任保留至全部旧证书到期。"))
                        .font(.callout).foregroundStyle(.secondary)
                    if let blockedRotation {
                        Toggle(L10n.text("替换当前未完成的更新目标"), isOn: $replacingBlockedTarget)
                        Text(L10n.text("仅未发布目标，或已经到期的已发布目标可被替换。当前批次：{0}", blockedRotation.id))
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
            }.formStyle(.grouped)
            if let error { Text(error).font(.callout).foregroundStyle(.red).textSelection(.enabled) }
            HStack {
                Button(L10n.text("取消")) { dismiss() }.keyboardShortcut(.cancelAction)
                Spacer()
                if saving { ProgressView().controlSize(.small) }
                Button(operation == .publish ? L10n.text("发布并更新全部引用") : L10n.text("保存")) { save() }
                    .keyboardShortcut(.defaultAction)
                    .accessibilityIdentifier("ca.editor.save")
                    .disabled(!store.isConnected || !policyValid || (operation == .create && name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty) || (importsMaterial && (certificate.isEmpty || privateKey.isEmpty)) || (operation == .publish && blockedRotation != nil && !replacingBlockedTarget))
            }
        }
        .padding(24).frame(width: 640, height: importsMaterial ? 740 : 520)
        .disabled(saving).interactiveDismissDisabled(saving)
        .onAppear {
            if let detail, let ca = detail.ca {
                name = detail.name; source = ca.source; validity = ca.clientValidityDays; renewBefore = ca.renewBeforeHours
            }
        }
        .onChange(of: name) { resetKey() }.onChange(of: source) { resetKey() }
        .onChange(of: certificate) { resetKey() }.onChange(of: privateKey) { resetKey() }
        .onChange(of: validity) { resetKey() }.onChange(of: renewBefore) { resetKey() }
        .onChange(of: replacingBlockedTarget) { resetKey() }
        .fileImporter(isPresented: $importing, allowedContentTypes: [.data]) { result in
            do {
                let url = try result.get()
                let access = url.startAccessingSecurityScopedResource()
                defer { if access { url.stopAccessingSecurityScopedResource() } }
                let data = try Data(contentsOf: url)
                guard data.count <= 1_048_576, let text = String(data: data, encoding: .utf8) else { throw APIClientError.server(L10n.text("请选择不超过 1 MiB 的 UTF-8 文件。")) }
                if importField == "certificate" { certificate = text } else { privateKey = text }
            } catch { self.error = error.localizedDescription }
        }
        .onDisappear { certificate = ""; privateKey = "" }
    }
    private func resetKey() { idempotencyKey = UUID().uuidString }
    private func importedText(_ title: String, text: Binding<String>, field: String) -> some View {
        VStack(alignment: .leading) {
            HStack { Text(title); Spacer(); Button(L10n.text("导入文件…")) { importField = field; importing = true } }
            CredentialPlainTextEditor(text: text, identifier: "ca.editor.\(field)", isEditable: !saving)
                .frame(minHeight: 90, maxHeight: 120)
        }
    }
    private func save() {
        saving = true; error = nil
        let key = idempotencyKey
        Task {
            defer { saving = false }
            do {
                let id: String
                let policy = ManagedCAPolicy(clientValidityDays: validity, renewBeforeHours: renewBefore)
                switch operation {
                case .create:
                    let receipt = try await store.createCAIdentity(CAIdentityCreateRequest(name: name.trimmingCharacters(in: .whitespacesAndNewlines), source: source, certificate: importsMaterial ? certificate : nil, privateKey: importsMaterial ? privateKey : nil, policy: policy, idempotencyKey: key))
                    id = receipt.id
                case .publish:
                    guard let detail else { return }
                    let receipt = try await store.publishCAIdentity(detail.id, request: CAIdentityPublishRequest(expectedRevision: detail.revision, certificate: importsMaterial ? certificate : nil, privateKey: importsMaterial ? privateKey : nil, replaceRotationId: replacingBlockedTarget ? blockedRotation?.id : nil, idempotencyKey: key))
                    id = receipt.id
                case .policy:
                    guard let detail, let ca = detail.ca else { return }
                    try await store.updateCAIdentityPolicy(detail.id, request: CAPolicyUpdateRequest(expectedRevision: ca.policyRevision, policy: policy, idempotencyKey: key))
                    id = detail.id
                }
                certificate = ""; privateKey = ""; onSaved?(id); dismiss()
            } catch { self.error = error.localizedDescription }
        }
    }
}
