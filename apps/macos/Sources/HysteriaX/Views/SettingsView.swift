import AppKit
import SwiftUI

struct SettingsView: View {
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
    @State private var newAdminTokenLabel = ""
    @State private var createdAdminToken: AdminTokenReceipt?
    @State private var isCreatingAdminToken = false
    @State private var tokenToRevoke: AdminTokenSummary?
    @State private var isConfirmingTokenRevocation = false
    @State private var revokingTokenID: String?

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
                        .disabled(isConnecting || isCreatingAdminToken)
                }
            }
            if let message {
                Text(message)
                    .foregroundStyle(.secondary)
                    .font(.callout)
                    .accessibilityLabel(message)
                    .accessibilityIdentifier("settings.connectionMessage")
            }
            Section("节点套餐通知") {
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
            Section("管理员令牌轮换") {
                Text("创建并切换新令牌后，本 Mac 会标记当前令牌。随后可以撤销旧令牌；当前令牌的撤销操作会被禁用。")
                    .foregroundStyle(.secondary)
                if store.isConnected && isConnectedToEnteredService {
                    HStack {
                        TextField("令牌用途，例如 Nekil 的 Mac", text: $newAdminTokenLabel)
                            .accessibilityLabel("令牌用途")
                            .accessibilityIdentifier("settings.newAdminTokenLabel")
                        if isCreatingAdminToken { ProgressView().controlSize(.small) }
                        Button("创建并切换") { createAdminToken() }
                            .disabled(
                                isCreatingAdminToken
                                    || isConnecting
                                    || newAdminTokenLabel.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                                    || newAdminTokenLabel.trimmingCharacters(in: .whitespacesAndNewlines).unicodeScalars.count > 100
                            )
                    }
                } else if !store.isConnected {
                    Text("连接管理服务后可创建、切换和撤销管理员令牌。")
                        .foregroundStyle(.secondary)
                } else {
                    Text("请先验证并保存当前填写的服务地址，再管理令牌。")
                        .foregroundStyle(.secondary)
                }
                if let createdAdminToken {
                    VStack(alignment: .leading, spacing: 8) {
                        Label("新令牌仅显示一次", systemImage: "key.fill")
                            .font(.headline)
                        TextField("Bearer Token", text: .constant(createdAdminToken.token))
                            .textFieldStyle(.roundedBorder)
                            .font(.system(.body, design: .monospaced))
                            .accessibilityLabel("新管理员令牌")
                            .accessibilityIdentifier("settings.createdAdminToken")
                            .textSelection(.enabled)
                        HStack {
                            Text(createdAdminToken.label).foregroundStyle(.secondary)
                            Spacer()
                            Button("复制令牌") { copy(createdAdminToken.token) }
                        }
                    }
                    .padding(.vertical, 4)
                }
                if store.isConnected && isConnectedToEnteredService {
                    if store.currentAdminTokenID == nil {
                        Text("当前连接令牌尚未由此 Mac 创建。为避免误撤销当前令牌，请先创建并切换新令牌，再撤销旧令牌。")
                            .font(.callout)
                            .foregroundStyle(.secondary)
                    }
                    if store.adminTokens.isEmpty {
                        Text("没有可显示的管理员令牌。")
                            .foregroundStyle(.secondary)
                    } else {
                        ForEach(store.adminTokens) { adminToken in
                            HStack(spacing: 10) {
                                VStack(alignment: .leading, spacing: 3) {
                                    Text(adminToken.label).fontWeight(.medium)
                                    Text("创建于 \(DateDisplayText.local(adminToken.createdAt)) · 最近使用 \(DateDisplayText.local(adminToken.lastUsedAt))")
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                }
                                Spacer()
                                if adminToken.id == store.currentAdminTokenID {
                                    Label("本 Mac 当前令牌", systemImage: "checkmark.circle.fill")
                                        .font(.caption)
                                        .foregroundStyle(.green)
                                } else if adminToken.revokedAt != nil {
                                    Text("已撤销").font(.caption).foregroundStyle(.secondary)
                                } else {
                                    Button("撤销", role: .destructive) {
                                        tokenToRevoke = adminToken
                                        isConfirmingTokenRevocation = true
                                    }
                                    .disabled(
                                        store.currentAdminTokenID == nil
                                            || revokingTokenID != nil
                                            || isCreatingAdminToken
                                            || isConnecting
                                    )
                                }
                            }
                            .padding(.vertical, 3)
                        }
                    }
                }
            }
        }
        .formStyle(.grouped)
        .padding(20)
        .confirmationDialog(
            "撤销管理员令牌？",
            isPresented: $isConfirmingTokenRevocation,
            titleVisibility: .visible
        ) {
            Button("撤销 \(tokenToRevoke?.label ?? "令牌")", role: .destructive) {
                if let tokenToRevoke { revokeAdminToken(tokenToRevoke) }
            }
            Button("取消", role: .cancel) { tokenToRevoke = nil }
        } message: {
            Text("此操作会拒绝该令牌后续的所有管理请求。")
        }
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

    private func createAdminToken() {
        isCreatingAdminToken = true
        message = nil
        createdAdminToken = nil
        Task {
            defer { isCreatingAdminToken = false }
            do {
                let receipt = try await store.createAndSwitchAdminToken(label: newAdminTokenLabel)
                createdAdminToken = receipt
                newAdminTokenLabel = ""
                if store.currentAdminTokenID == receipt.id {
                    token = receipt.token
                    message = store.isConnected
                        ? "新令牌已切换并保存到 Keychain；旧令牌仍有效，可在下方撤销。"
                        : store.errorMessage
                } else {
                    message = store.errorMessage ?? "新令牌已创建；请立即复制并安全保存。"
                }
            } catch { message = error.localizedDescription }
        }
    }

    private func revokeAdminToken(_ adminToken: AdminTokenSummary) {
        revokingTokenID = adminToken.id
        message = nil
        Task {
            defer {
                revokingTokenID = nil
                tokenToRevoke = nil
            }
            do {
                try await store.revokeAdminToken(adminToken)
                message = "已撤销令牌“\(adminToken.label)”。"
            } catch { message = error.localizedDescription }
        }
    }

    private func copy(_ value: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(value, forType: .string)
    }

    private var isConnectedToEnteredService: Bool {
        guard let url = URL(string: serviceAddress.trimmingCharacters(in: .whitespacesAndNewlines)),
              url.scheme == "https", url.host != nil else { return false }
        return url.absoluteString.trimmingCharacters(in: CharacterSet(charactersIn: "/")) == store.serviceAddress
    }
}
