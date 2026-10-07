import Foundation
import Observation
import Synchronization

/// Shared by views and background services. Only display text uses this preference.
@Observable
final class AppLanguage: @unchecked Sendable {
    static let shared = AppLanguage()
    static let preferenceKey = "appLanguage"
    @ObservationIgnored private let storage: Mutex<String>
    @ObservationIgnored private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        storage = Mutex(Self.validSelection(defaults.string(forKey: Self.preferenceKey) ?? "system"))
    }

    var selection: String {
        get {
            access(keyPath: \.selection)
            return storage.withLock { $0 }
        }
        set {
            let value = Self.validSelection(newValue)
            withMutation(keyPath: \.selection) {
                storage.withLock { $0 = value }
                defaults.set(value, forKey: Self.preferenceKey)
            }
        }
    }

    func refreshSystemLanguage() {
        if selection == "system" {
            withMutation(keyPath: \.selection) {}
        }
    }

    private static func validSelection(_ value: String) -> String {
        ["system", "en", "zh-Hans"].contains(value) ? value : "system"
    }
}

enum L10n {
    static func resolveLanguage(selection: String, preferredLanguages: [String]) -> String {
        if selection == "en" || selection == "zh-Hans" { return selection }
        let primary = preferredLanguages.first?.lowercased() ?? "en"
        return primary == "zh" || primary.hasPrefix("zh-") ? "zh-Hans" : "en"
    }

    static var language: String {
        resolveLanguage(selection: AppLanguage.shared.selection, preferredLanguages: Locale.preferredLanguages)
    }

    static var locale: Locale { Locale(identifier: language) }

    private static var resourceBundle: Bundle {
        #if SWIFT_PACKAGE
        Bundle.module
        #else
        Bundle.main
        #endif
    }

    static func date(_ value: Date, date: Date.FormatStyle.DateStyle = .abbreviated,
                     time: Date.FormatStyle.TimeStyle = .shortened) -> String {
        value.formatted(Date.FormatStyle(date: date, time: time).locale(locale))
    }

    // Load each table once; language selection itself stays observable and dynamic.
    private static let tables: [String: [String: String]] = {
        var result: [String: [String: String]] = [:]
        for language in ["en", "zh-Hans"] {
            if let url = resourceBundle.url(forResource: "Localizable", withExtension: "strings", subdirectory: "\(language).lproj"),
               let data = try? Data(contentsOf: url),
               let table = try? PropertyListSerialization.propertyList(from: data, format: nil) as? [String: String] {
                result[language] = table
            }
        }
        return result
    }()

    static func text(_ key: String, _ arguments: String...) -> String {
        let template = tables[language]?[key] ?? tables["en"]?[key] ?? key
        return interpolate(template, arguments: arguments)
    }

    /// Substitute in one pass so user values containing `{0}` are never interpreted again.
    static func interpolate(_ template: String, arguments: [String]) -> String {
        guard !arguments.isEmpty else { return template }
        var output = ""
        var remaining = template[...]
        while let open = remaining.firstIndex(of: "{") {
            output += remaining[..<open]
            let afterOpen = remaining.index(after: open)
            if let close = remaining[afterOpen...].firstIndex(of: "}"),
               let index = Int(remaining[afterOpen..<close]), arguments.indices.contains(index) {
                output += arguments[index]
                remaining = remaining[remaining.index(after: close)...]
            } else {
                output += "{"
                remaining = remaining[afterOpen...]
            }
        }
        return output + remaining
    }
}
