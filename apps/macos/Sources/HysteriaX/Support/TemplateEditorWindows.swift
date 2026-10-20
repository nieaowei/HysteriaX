import AppKit
import SwiftUI
import Observation

extension Notification.Name {
    static let templateListChanged = Notification.Name("templateListChanged")
}

@MainActor @Observable
final class TemplateEditorWindows {
    private(set) var sessions: [TemplateEditorID: SubscriptionTemplateStore] = [:]
    @ObservationIgnored private var controllers: [TemplateEditorID: TemplateEditorWindowController] = [:]
    func connectionChanged() { for model in sessions.values { model.invalidatePreview() } }
    func reconcile(store: ManagementStore) async {
        guard let api = try? store.requireConnectedAPI() else { return }
        for (id, model) in sessions where id.service == TemplateEditorID(service: store.serviceAddress, template: id.template).service {
            do { try await model.reconnect(api) } catch { model.record(error) }
        }
    }
    func status(service: String, template: String) -> String {
        guard let model = sessions[TemplateEditorID(service: service, template: template)] else { return L10n.text("未打开") }
        if model.saving { return L10n.text("保存中") }
        if model.error != nil { return L10n.text("失败") }
        return model.dirty ? L10n.text("未保存") : L10n.text("无本地修改")
    }
    func open(_ template: SubscriptionTemplate, store: ManagementStore, action: String? = nil) {
        let id = TemplateEditorID(service: store.serviceAddress, template: template.id)
        if let controller = controllers[id] { controller.showWindow(nil); controller.window?.deminiaturize(nil); controller.window?.makeKeyAndOrderFront(nil); NSApp.activate(); if let action { sessions[id]?.requestedAction = action }; return }
        let model = SubscriptionTemplateStore()
        model.selected = template; model.serviceName = id.service
        model.sessionAvailable = { [weak store] api in
            guard let store else { return false }
            return TemplateEditorID(service: store.serviceAddress, template: template.id) == id && store.isConnected && (try? store.requireConnectedAPI()) === api
        }
        sessions[id] = model
        let controller = TemplateEditorWindowController(id: id, model: model, store: store) { [weak self] in
            model.closed = true; model.invalidatePreview()
            self?.sessions[id] = nil; self?.controllers[id] = nil
        }
        controllers[id] = controller
        controller.showWindow(nil); controller.window?.makeKeyAndOrderFront(nil); NSApp.activate()
        model.run { try await model.open(template, api: store.requireConnectedAPI()); model.requestedAction = action }
    }
}

/// Owns only native window lifetime and responder routing; document state stays in SwiftUI's model.
@MainActor
final class TemplateEditorWindowController: NSWindowController, NSWindowDelegate {
    let model: SubscriptionTemplateStore
    let id: TemplateEditorID
    weak var store: ManagementStore?
    private let onClose: () -> Void
    init(id: TemplateEditorID, model: SubscriptionTemplateStore, store: ManagementStore, onClose: @escaping () -> Void) {
        self.id = id; self.model = model; self.store = store; self.onClose = onClose
        var initialWidth: CGFloat = 1200
        #if HYSTERIAX_UI_TESTING
        let args = ProcessInfo.processInfo.arguments
        if let index = args.firstIndex(of: "-layoutWidth"), args.indices.contains(index + 1), let width = Double(args[index + 1]) { initialWidth = width }
        else { initialWidth = 1350 }
        #endif
        let available = NSScreen.main?.visibleFrame.size ?? NSSize(width: 1200, height: 800)
        // Let NavigationSplitView own the sidebar material and toolbar safe area
        // across the full window, including the titlebar.
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: min(initialWidth, available.width), height: min(800, available.height)), styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView], backing: .buffered, defer: false)
        super.init(window: window)
        window.identifier = NSUserInterfaceItemIdentifier("template.\(id.template)")
        window.titleVisibility = .visible
        window.titlebarAppearsTransparent = true
        window.toolbarStyle = .unified
        window.contentMinSize = NSSize(width: 920, height: 640)
        window.contentViewController = NSHostingController(rootView: SubscriptionTemplateWorkbench(store: store, model: model).environment(\.locale, L10n.locale))
        window.setContentSize(NSSize(width: min(initialWidth, available.width), height: min(800, available.height - 52)))
        window.delegate = self; window.isReleasedWhenClosed = false; window.center()
    }
    required init?(coder: NSCoder) { nil }
    func windowShouldClose(_ sender: NSWindow) -> Bool {
        guard model.dirty else { return true }
        guard !model.busy else { return false }
        let alert = NSAlert(); alert.messageText = L10n.text("存在未保存修改")
        alert.addButton(withTitle: L10n.text("保存并关闭")); alert.addButton(withTitle: L10n.text("取消")); alert.addButton(withTitle: L10n.text("放弃修改"))
        switch alert.runModal() {
        case .alertFirstButtonReturn:
            model.run { [self] in
                guard let store else { throw CancellationError() }
                try await model.save(store.requireConnectedAPI())
                guard !model.dirty else { throw APIClientError.server("草稿仍有未保存修改，已取消关闭。") }
                window?.close()
            }
            return false
        case .alertThirdButtonReturn: return true
        default: return false
        }
    }
    func windowWillClose(_ notification: Notification) { onClose() }
    @objc func saveDocument(_ sender: Any?) {
        guard let store else { return }; model.run { try await self.model.save(store.requireConnectedAPI()) }
    }
    @objc func undo(_ sender: Any?) { if !model.sourceMode && !model.busy { model.undo() } }
    @objc func redo(_ sender: Any?) { if !model.sourceMode && !model.busy { model.redo() } }
}

struct TemplateEditorCommands: Commands {
    var body: some Commands {
        CommandGroup(replacing: .saveItem) {
            Button(L10n.text("保存草稿")) {
                NSApp.sendAction(#selector(TemplateEditorWindowController.saveDocument(_:)), to: nil, from: nil)
            }.keyboardShortcut("s", modifiers: .command)
        }
    }
}
