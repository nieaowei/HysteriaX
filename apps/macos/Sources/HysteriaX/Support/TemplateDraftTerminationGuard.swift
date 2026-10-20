import AppKit

/// SwiftUI keeps editor state; this hook only protects drafts when the process exits.
@MainActor
final class TemplateDraftTerminationGuard: NSObject, NSApplicationDelegate {
    weak var store: ManagementStore?
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        let editors = store?.templateWindows.sessions.values.filter(\.dirty) ?? []
        guard !editors.isEmpty else { return .terminateNow }
        let alert = NSAlert()
        alert.messageText = L10n.text("存在未保存的订阅模板修改")
        alert.informativeText = L10n.text("保存所有草稿后退出，或放弃本地修改。无效源码和并发冲突会保留编辑内容并取消退出。")
        alert.addButton(withTitle: L10n.text("保存并退出"))
        alert.addButton(withTitle: L10n.text("取消"))
        alert.addButton(withTitle: L10n.text("放弃修改并退出"))
        switch alert.runModal() {
        case .alertFirstButtonReturn:
            Task {
                do {
                    guard editors.allSatisfy({ !$0.busy }) else { throw APIClientError.server("请等待当前模板操作完成后再退出。") }
                    editors.forEach { $0.busy = true }
                    defer { editors.forEach { $0.busy = false } }
                    for editor in editors {
                        guard let api = editor.connectedAPI else { throw APIClientError.invalidResponse }
                        try await editor.save(api)
                    }
                    guard self.store?.templateWindows.sessions.values.contains(where: { $0.dirty }) != true else {
                        throw APIClientError.server("仍有未保存的模板修改，已取消退出。")
                    }
                    sender.reply(toApplicationShouldTerminate: true)
                } catch {
                    let failure = NSAlert(); failure.messageText = error.localizedDescription
                    failure.runModal(); sender.reply(toApplicationShouldTerminate: false)
                }
            }
            return .terminateLater
        case .alertThirdButtonReturn: return .terminateNow
        default: return .terminateCancel
        }
    }
}
