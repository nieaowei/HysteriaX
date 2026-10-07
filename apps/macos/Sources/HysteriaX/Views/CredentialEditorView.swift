import AppKit
import SwiftUI
import UniformTypeIdentifiers

struct CredentialEditorView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ManagementStore
    var replacing: CredentialDetail? = nil
    var initialKind = "ssh_private_key"
    var initialOwner: String? = nil
    var allowedKinds: [String]? = nil
    var allowsUserOwnership = true
    var onCreated: ((CredentialReceipt) -> Void)? = nil
    @State private var name = ""
    @State private var kind = "ssh_private_key"
    @State private var owner = ""
    @State private var secret = ""
    @State private var passphrase = ""
    @State private var certificate = ""
    @State private var privateKey = ""
    @State private var dns = ACMEDNSDraft()
    @State private var hasReminder = false
    @State private var reminder = Date().addingTimeInterval(30 * 86400)
    @State private var importField = ""
    @State private var importing = false
    @State private var saving = false
    @State private var error: String?

    private var kinds: [String] { allowedKinds ?? ["ssh_private_key", "ssh_password", "tls_identity", "ca_certificate", "ech_key", "dns", "api_token"] }
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(replacing == nil ? L10n.text("创建凭据") : L10n.text("发布新版本")).font(.title2.bold())
            Form {
                if replacing == nil {
                    TextField(L10n.text("名称"), text: $name).accessibilityIdentifier("credential.editor.name")
                    Picker(L10n.text("类型"), selection: $kind) { ForEach(kinds, id: \.self) { Text(CredentialDisplay.kind($0)).tag($0) } }.accessibilityIdentifier("credential.editor.kind")
                    if kind == "tls_identity" && allowsUserOwnership {
                        Picker(L10n.text("用途"), selection: $owner) {
                            Text(L10n.text("节点证书（可共享）")).tag("")
                            ForEach(store.users) { Text("\($0.name) · mTLS").tag($0.id) }
                        }
                    }
                    Toggle(L10n.text("设置到期提醒"), isOn: $hasReminder)
                    if hasReminder { DatePicker(L10n.text("提醒时间"), selection: $reminder) }
                } else {
                    LabeledContent(L10n.text("凭据"), value: replacing!.name)
                    Text(L10n.text("发布后自动更新全部当前引用；历史配置固定原版本。")).foregroundStyle(.secondary)
                }
                switch kind {
                case "ssh_private_key":
                    importedText(L10n.text("SSH 私钥"), text: $secret, field: "secret")
                    SecureField(L10n.text("私钥解密口令（可留空）"), text: $passphrase)
                case "ssh_password": SecureField(L10n.text("SSH 登录密码"), text: $secret)
                case "api_token": SecureField("API Token", text: $secret).accessibilityIdentifier("credential.editor.token")
                case "tls_identity":
                    importedText(L10n.text("证书链 PEM"), text: $certificate, field: "certificate")
                    importedText(L10n.text("私钥 PEM"), text: $privateKey, field: "private_key")
                case "ca_certificate", "ech_key":
                    importedText(kind == "ca_certificate" ? L10n.text("CA 证书 PEM") : L10n.text("密钥内容"), text: $secret, field: "secret")
                case "dns": ACMEDNSFields(draft: $dns)
                default: EmptyView()
                }
            }.formStyle(.grouped)
            Text(L10n.text("SSH 授权、账户改密或第三方 Token 创建需先在远端完成。秘密保存后不再显示。")).font(.caption).foregroundStyle(.secondary)
            if let error { Text(error).foregroundStyle(.red).font(.callout) }
            HStack {
                Button(L10n.text("取消")) { dismiss() }.keyboardShortcut(.cancelAction)
                Spacer()
                if saving { ProgressView().controlSize(.small) }
                Button(replacing == nil ? L10n.text("创建") : L10n.text("发布并更新全部引用")) { save() }
                    .keyboardShortcut(.defaultAction).disabled(saving || !store.isConnected)
                    .accessibilityIdentifier("credential.editor.save")
            }
        }.padding(24).frame(width: 620, height: 660)
        .disabled(saving).interactiveDismissDisabled(saving)
        .onAppear {
            kind = replacing?.kind ?? initialKind
            owner = replacing?.ownerUserId ?? initialOwner ?? ""
            if let provider = replacing?.metadata["provider"]?.stringValue { dns.provider = provider }
        }
        .fileImporter(isPresented: $importing, allowedContentTypes: [.data]) { result in
            do {
                let url = try result.get()
                let access = url.startAccessingSecurityScopedResource()
                defer { if access { url.stopAccessingSecurityScopedResource() } }
                let data = try Data(contentsOf: url)
                guard data.count <= 1_048_576, let text = String(data: data, encoding: .utf8) else { throw APIClientError.server(L10n.text("请选择不超过 1 MiB 的 UTF-8 文件。")) }
                switch importField { case "certificate": certificate = text; case "private_key": privateKey = text; default: secret = text }
                if name.isEmpty { name = url.lastPathComponent }
            } catch { self.error = error.localizedDescription }
        }
    }

    private func importedText(_ label: String, text: Binding<String>, field: String) -> some View {
        VStack(alignment: .leading) {
            HStack { Text(label); Spacer(); Button(L10n.text("导入文件…")) { importField = field; importing = true } }
            CredentialPlainTextEditor(text: text, identifier: "credential.editor.content.\(field)", isEditable: !saving)
                .frame(minHeight: 70, maxHeight: 110)
        }
    }

    private func save() {
        var payload: [String: JSONValue]
        switch kind {
        case "ssh_private_key":
            payload = ["secret": .string(secret)]
            if !passphrase.isEmpty { payload["passphrase"] = .string(passphrase) }
        case "ssh_password": payload = ["secret": .string(secret)]
        case "api_token": payload = ["token": .string(secret)]
        case "tls_identity": payload = ["certificate": .string(certificate), "private_key": .string(privateKey)]
        case "dns":
            if let error = dns.validationError { self.error = error; return }
            payload = ["provider": .string(dns.provider), "config": .object(dns.configuration)]
        default: payload = ["content": .string(secret)]
        }
        saving = true
        Task {
            defer { saving = false }
            do {
                if let replacing { try await store.publishCredential(replacing, payload: payload) }
                else {
                    let receipt = try await store.createCredential(name: name, kind: kind,
                        ownerUserID: kind == "tls_identity" && !owner.isEmpty ? owner : nil,
                        reminderAt: hasReminder ? ISO8601DateFormatter().string(from: reminder) : nil, payload: payload)
                    onCreated?(receipt)
                }
                secret = ""; passphrase = ""; privateKey = ""; certificate = ""; dns = ACMEDNSDraft()
                dismiss()
            } catch { self.error = error.localizedDescription }
        }
    }
}

/// Preserve PEM and key bytes instead of applying macOS prose substitutions.
private struct CredentialPlainTextEditor: NSViewRepresentable {
    @Binding var text: String
    let identifier: String
    let isEditable: Bool

    func makeCoordinator() -> Coordinator { Coordinator(text: $text) }

    func makeNSView(context: Context) -> NSScrollView {
        let editor = NSTextView()
        editor.isRichText = false
        editor.isAutomaticQuoteSubstitutionEnabled = false
        editor.isAutomaticDashSubstitutionEnabled = false
        editor.isAutomaticTextReplacementEnabled = false
        editor.isAutomaticSpellingCorrectionEnabled = false
        editor.isContinuousSpellCheckingEnabled = false
        editor.isGrammarCheckingEnabled = false
        editor.allowsUndo = true
        editor.font = .monospacedSystemFont(ofSize: NSFont.smallSystemFontSize, weight: .regular)
        editor.isVerticallyResizable = true
        editor.isHorizontallyResizable = false
        editor.autoresizingMask = [.width]
        editor.textContainer?.widthTracksTextView = true
        editor.textContainer?.containerSize = NSSize(width: 0, height: CGFloat.greatestFiniteMagnitude)
        editor.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        editor.setAccessibilityIdentifier(identifier)
        editor.delegate = context.coordinator
        editor.string = text
        editor.isEditable = isEditable
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.documentView = editor
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        context.coordinator.text = $text
        guard let editor = scroll.documentView as? NSTextView else { return }
        if editor.string != text { editor.string = text }
        editor.isEditable = isEditable
    }

    final class Coordinator: NSObject, NSTextViewDelegate {
        var text: Binding<String>
        init(text: Binding<String>) { self.text = text }
        func textDidChange(_ notification: Notification) {
            guard let editor = notification.object as? NSTextView else { return }
            text.wrappedValue = editor.string
        }
    }
}
