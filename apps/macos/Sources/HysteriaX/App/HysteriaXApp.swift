import SwiftUI
import AppKit

@main
struct HysteriaXApp: App {
    @NSApplicationDelegateAdaptor(TemplateDraftTerminationGuard.self) private var terminationGuard
    @State private var store = ManagementStore()

    var body: some Scene {
        WindowGroup("HysteriaX", id: "main") {
            ContentView(store: store)
                .onAppear { terminationGuard.store = store }
                .frame(minWidth: 920, maxWidth: .infinity, minHeight: 600, maxHeight: .infinity)
                .environment(\.locale, L10n.locale)
                .onReceive(NotificationCenter.default.publisher(for: NSLocale.currentLocaleDidChangeNotification)) { _ in
                    AppLanguage.shared.refreshSystemLanguage()
                }
        }
        .commands {
            TemplateEditorCommands()
            CommandGroup(after: .appInfo) {
                SettingsLink()
            }
            CommandGroup(after: .sidebar) {
                Section(L10n.text("导航")) {
                    Button(L10n.text("概览")) { store.requestedSection = "overview" }
                        .keyboardShortcut("1", modifiers: .command)
                    Button(L10n.text("节点")) { store.requestedSection = "nodes" }
                        .keyboardShortcut("2", modifiers: .command)
                    Button("DNS") { store.requestedSection = "dns" }
                        .keyboardShortcut("3", modifiers: .command)
                    Button(L10n.text("凭据")) { store.requestedSection = "credentials" }
                        .keyboardShortcut("4", modifiers: .command)
                    Button(L10n.text("用户")) { store.requestedSection = "users" }
                        .keyboardShortcut("5", modifiers: .command)
                    Button(L10n.text("订阅模板")) { store.requestedSection = "templates" }
                        .keyboardShortcut("6", modifiers: .command)
                    Button(L10n.text("任务")) { store.requestedSection = "jobs" }
                        .keyboardShortcut("7", modifiers: .command)
                    Button(L10n.text("审计")) { store.requestedSection = "audit" }
                        .keyboardShortcut("8", modifiers: .command)
                }
            }
        }

        Settings {
            SettingsView(store: store)
                .frame(width: 540, height: 480)
                .environment(\.locale, L10n.locale)
        }
    }
}
