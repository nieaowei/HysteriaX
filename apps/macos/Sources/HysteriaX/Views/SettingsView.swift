import SwiftUI

struct SettingsView: View {
    @Environment(\.openWindow) private var openWindow
    @Bindable var store: ManagementStore
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
            Section("管理服务") {
                TextField("HTTPS 地址", text: $serviceAddress, prompt: Text("https://manage.example.com"))
                    .accessibilityLabel("HTTPS 地址")
                    .accessibilityIdentifier("settings.serviceAddress")
                    .textContentType(.URL)
                SecureField("管理员 Bearer Token", text: $token)
                    .accessibilityLabel("管理员 Bearer Token")
                    .accessibilityIdentifier("settings.adminToken")
                    .textContentType(.password)
                HStack {
                    Label(
                        store.isConnected ? (isConnectedToEnteredService ? "已连接" : "当前地址未连接") : "未连接",
                        systemImage: store.isConnected && isConnectedToEnteredService ? "checkmark.circle.fill" : "circle"
                    )
                    .foregroundStyle(store.isConnected && isConnectedToEnteredService ? .green : .secondary)
                    .accessibilityIdentifier("settings.connectionState")
                    Spacer()
                    if isConnecting { ProgressView().controlSize(.small) }
                    Button("验证并保存") { connect() }
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
            Section("节点与凭据通知") {
                Toggle("启用 macOS 系统通知", isOn: $packageNotifications)
                    .onChange(of: packageNotifications) { _, enabled in
                        if enabled {
                            Task {
                                notificationMessage = await NodeAlertNotifications.shared.requestPermission()
                                await store.refresh()
                            }
                        }
                    }
                Text(notificationMessage ?? "应用内始终显示提醒；系统通知需要应用运行、联网并获得授权。")
                    .font(.callout).foregroundStyle(.secondary)
            }
            Section("安全") {
                #if HYSTERIAX_UI_TESTING
                Text("测试环境中的管理员令牌只保存在当前进程内存中。")
                    .foregroundStyle(.secondary)
                    .accessibilityLabel("测试环境中的管理员令牌只保存在当前进程内存中。")
                    .accessibilityIdentifier("settings.keychainMode")
                #else
                Text("管理员令牌保存在 macOS Keychain 中。服务端负责保存节点、用户和订阅数据。")
                    .foregroundStyle(.secondary)
                #endif
            }
            Section("凭据管理") {
                Button("在凭据中心管理 Token、证书和私钥") {
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
                message = "连接成功，服务端状态已刷新。"
            } catch { message = error.localizedDescription }
        }
    }

    private var isConnectedToEnteredService: Bool {
        guard let url = URL(string: serviceAddress.trimmingCharacters(in: .whitespacesAndNewlines)),
              url.scheme == "https", url.host != nil else { return false }
        return url.absoluteString.trimmingCharacters(in: CharacterSet(charactersIn: "/")) == store.serviceAddress
    }
}
