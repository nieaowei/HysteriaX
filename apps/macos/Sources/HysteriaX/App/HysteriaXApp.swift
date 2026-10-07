import SwiftUI

@main
struct HysteriaXApp: App {
    @State private var store = ManagementStore()

    var body: some Scene {
        WindowGroup("HysteriaX", id: "main") {
            ContentView(store: store)
                .frame(minWidth: 920, minHeight: 600)
                .environment(\.locale, L10n.locale)
                .onReceive(NotificationCenter.default.publisher(for: NSLocale.currentLocaleDidChangeNotification)) { _ in
                    AppLanguage.shared.refreshSystemLanguage()
                }
        }
        .commands {
            CommandGroup(after: .appInfo) {
                SettingsLink()
            }
        }

        Settings {
            SettingsView(store: store)
                .frame(width: 540, height: 480)
                .environment(\.locale, L10n.locale)
        }
    }
}
