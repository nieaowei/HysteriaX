import SwiftUI

@main
struct HysteriaXApp: App {
    @State private var store = ManagementStore()

    var body: some Scene {
        WindowGroup("HysteriaX") {
            ContentView(store: store)
                .frame(minWidth: 920, minHeight: 600)
        }
        .commands {
            CommandGroup(after: .appInfo) {
                SettingsLink()
            }
        }

        Settings {
            SettingsView(store: store)
                .frame(width: 540, height: 380)
        }
    }
}
