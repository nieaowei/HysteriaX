import SwiftUI

struct SettingsView: View {
    @Environment(\.openWindow) private var openWindow
    @Bindable var store: ManagementStore
    @Bindable private var language = AppLanguage.shared
#if HYSTERIAX_UI_TESTING
    @State private var serviceAddress = ProcessInfo.processInfo.environment[
        "HYSTERIAX_UI_TEST_SERVICE_ADDRESS"
    ] ?? UserDefaults.standard.string(forKey: "serviceAddress") ?? ""
#else
    @State private var serviceAddress = UserDefaults.standard.string(forKey: "serviceAddress") ?? ""
#endif
    @AppStorage(NodeAlertNotifications.preferenceKey) private var packageNotifications = false
    @State private var notificationMessage: String?
    @State private var token = KeychainStore.readToken() ?? ""
    @State private var message: String?
    @State private var isConnecting = false

    var body: some View {
        Form {
            Section(L10n.text("语言")) {
                Picker(L10n.text("显示语言"), selection: $language.selection) {
                    Text(L10n.text("跟随系统")).tag("system")
                    Text("English").tag("en")
                    Text("中文").tag("zh-Hans")
                }
                .accessibilityIdentifier("settings.language")
            }
            Section(L10n.text("管理服务")) {
                TextField(L10n.text("HTTPS 地址"), text: $serviceAddress, prompt: Text("https://manage.example.com"))
                    .accessibilityLabel(L10n.text("HTTPS 地址"))
                    .accessibilityIdentifier("settings.serviceAddress")
                    .textContentType(.URL)
                SecureField(L10n.text("管理员 Bearer Token"), text: $token)
                    .accessibilityLabel(L10n.text("管理员 Bearer Token"))
                    .accessibilityIdentifier("settings.adminToken")
                    .textContentType(.password)
                HStack {
                    Label(
                        store.isConnected ? (isConnectedToEnteredService ? L10n.text("已连接") : L10n.text("当前地址未连接")) : L10n.text("未连接"),
                        systemImage: store.isConnected && isConnectedToEnteredService ? "checkmark.circle.fill" : "circle"
                    )
                    .foregroundStyle(store.isConnected && isConnectedToEnteredService ? .green : .secondary)
                    .accessibilityIdentifier("settings.connectionState")
                    Spacer()
                    if isConnecting { ProgressView().controlSize(.small) }
                    Button(L10n.text("验证并保存")) { connect() }
                        .disabled(isConnecting)
                }
            }
            if let message {
                Text(message)
                    .foregroundStyle(.secondary)
                    .font(.callout)
                    .accessibilityLabel(message)
                    .accessibilityIdentifier("settings.connectionMessage")
            }
            Section(L10n.text("节点与凭据通知")) {
                Toggle(L10n.text("启用 macOS 系统通知"), isOn: $packageNotifications)
                    .onChange(of: packageNotifications) { _, enabled in
                        if enabled {
                            Task {
                                notificationMessage = await NodeAlertNotifications.shared.requestPermission()
                                await store.refresh()
                            }
                        }
                    }
                Text(notificationMessage ?? L10n.text("应用内始终显示提醒；系统通知需要应用运行、联网并获得授权。"))
                    .font(.callout).foregroundStyle(.secondary)
            }
            Section(L10n.text("安全")) {
                #if HYSTERIAX_UI_TESTING
                Text(L10n.text("测试环境中的管理员令牌只保存在当前进程内存中。"))
                    .foregroundStyle(.secondary)
                    .accessibilityLabel(L10n.text("测试环境中的管理员令牌只保存在当前进程内存中。"))
                    .accessibilityIdentifier("settings.keychainMode")
                #else
                Text(L10n.text("管理员令牌保存在 macOS Keychain 中。服务端负责保存节点、用户和订阅数据。"))
                    .foregroundStyle(.secondary)
                #endif
            }
            Section(L10n.text("凭据管理")) {
                Button(L10n.text("在凭据中心管理 Token、证书和私钥")) {
                    store.requestedSection = "credentials"
                    openWindow(id: "main")
                }.disabled(!store.isConnected)
            }
        }
        .formStyle(.grouped)
        .padding(20)
    }

    private func connect() {
        isConnecting = true
        message = nil
        Task {
            defer { isConnecting = false }
            do {
                try await store.connect(serviceAddress: serviceAddress, token: token)
                message = L10n.text("连接成功，服务端状态已刷新。")
            } catch { message = error.localizedDescription }
        }
    }

    private var isConnectedToEnteredService: Bool {
        guard let url = URL(string: serviceAddress.trimmingCharacters(in: .whitespacesAndNewlines)),
              url.scheme == "https", url.host != nil else { return false }
        return url.absoluteString.trimmingCharacters(in: CharacterSet(charactersIn: "/")) == store.serviceAddress
    }
}
