import Foundation
import UserNotifications

@MainActor
protocol NodeNotificationTransport {
    func requestPermission() async throws -> Bool
    func isAuthorized() async -> Bool
    func send(id: String, title: String, body: String) async throws
}

@MainActor
private struct SystemNodeNotificationTransport: NodeNotificationTransport {
    func requestPermission() async throws -> Bool {
        try await UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound])
    }
    func isAuthorized() async -> Bool {
        let settings = await UNUserNotificationCenter.current().notificationSettings()
        return settings.authorizationStatus == .authorized || settings.authorizationStatus == .provisional
    }
    func send(id: String, title: String, body: String) async throws {
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        content.sound = .default
        try await UNUserNotificationCenter.current().add(UNNotificationRequest(identifier: id, content: content, trigger: nil))
    }
}

@MainActor
final class NodeAlertNotifications: NSObject, UNUserNotificationCenterDelegate {
    static let shared = NodeAlertNotifications()
    static let preferenceKey = "nodePackageNotificationsEnabled"
    private var delivering = false
    private let transport: any NodeNotificationTransport
    private let defaults: UserDefaults

    override convenience init() {
        self.init(transport: SystemNodeNotificationTransport(), defaults: .standard)
        UNUserNotificationCenter.current().delegate = self
    }
    init(transport: any NodeNotificationTransport, defaults: UserDefaults) {
        self.transport = transport
        self.defaults = defaults
        super.init()
    }

    func requestPermission() async -> String {
        do {
            let granted = try await transport.requestPermission()
            return granted ? "已启用系统通知；应用运行并联网时发送。" : "系统通知未获授权；应用内提醒仍可用。请在系统设置中允许通知。"
        } catch { return "无法启用系统通知：\(error.localizedDescription)" }
    }

    func deliver(nodes: [NodeSummary], service: String) async {
        guard defaults.bool(forKey: Self.preferenceKey), !delivering else { return }
        delivering = true
        defer { delivering = false }
        guard await transport.isAuthorized() else { return }
        let key = "nodePackageNotified.\(service)"
        var seen = Set(defaults.stringArray(forKey: key) ?? [])
        for node in nodes {
            for alert in node.packageUsage?.alerts ?? [] where !seen.contains(alert.id) {
                do {
                    try await transport.send(
                        id: "node-package-\(alert.id)",
                        title: "\(node.name)：\(PackageDisplay.warning(alert.kind))",
                        body: "\(PackageDisplay.expiry(node.package)) · \(PackageDisplay.usage(node.package, node.packageUsage))"
                    )
                    seen.insert(alert.id)
                    defaults.set(Array(seen), forKey: key)
                } catch {
                    // Retry on the next successful refresh; do not mark undelivered events.
                }
            }
        }
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        [.banner, .sound]
    }
}
