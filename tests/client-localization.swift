import Foundation
import Observation
import Synchronization

@main
struct LocalizationTests {
    static func main() {
        for language in ["zh", "zh-CN", "zh-Hans-CN", "zh-Hant-TW", "zh-HK"] {
            precondition(L10n.resolveLanguage(selection: "system", preferredLanguages: [language]) == "zh-Hans")
        }
        for languages in [["en-US"], ["en-GB"], ["fr-FR", "zh-CN"], []] {
            precondition(L10n.resolveLanguage(selection: "system", preferredLanguages: languages) == "en")
        }
        precondition(L10n.resolveLanguage(selection: "en", preferredLanguages: ["zh-CN"]) == "en")
        precondition(L10n.resolveLanguage(selection: "zh-Hans", preferredLanguages: ["en-US"]) == "zh-Hans")
        precondition(L10n.resolveLanguage(selection: "invalid", preferredLanguages: ["zh-CN"]) == "zh-Hans")

        let suite = "HysteriaX.LocalizationTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let preference = AppLanguage(defaults: defaults)
        precondition(preference.selection == "system")
        preference.selection = "en"
        precondition(AppLanguage(defaults: defaults).selection == "en")
        preference.selection = "invalid"
        precondition(preference.selection == "system")
        let changed = Mutex(false)
        withObservationTracking {
            _ = preference.selection
        } onChange: {
            changed.withLock { $0 = true }
        }
        preference.selection = "zh-Hans"
        precondition(changed.withLock { $0 }, "Language changes must invalidate observing views")

        precondition(L10n.interpolate("{1} / {0} / {1}", arguments: ["A", "B"]) == "B / A / B")
        precondition(L10n.interpolate("{0} then {1}", arguments: ["literal {1}", "B"]) == "literal {1} then B")
        precondition(L10n.interpolate("{9} {text} {", arguments: ["A"]) == "{9} {text} {")
        precondition(L10n.interpolate("plain", arguments: []) == "plain")

        let previous = AppLanguage.shared.selection
        defer { AppLanguage.shared.selection = previous }
        AppLanguage.shared.selection = "en"
        precondition(L10n.text("概览") == "Overview", "English resources must be bundled")
        precondition(L10n.text("语言") == "Language")
        precondition(L10n.text("剩余 {0} 天", "12") == "12 days remaining")
        precondition(L10n.text("unknown.remote.code") == "unknown.remote.code")
        precondition(JobDisplayText.status("queued") == "Queued")
        precondition(JobDisplayText.errorMessage("SSH connection timed out") == "SSH connection timed out")
        let englishDate = L10n.date(Date(timeIntervalSince1970: 1_700_000_000))
        AppLanguage.shared.selection = "zh-Hans"
        precondition(L10n.text("概览") == "概览")
        precondition(L10n.text("剩余 {0} 天", "12") == "剩余 12 天")
        precondition(JobDisplayText.status("queued") == "排队中")
        precondition(JobDisplayText.errorMessage("SSH connection timed out") == "SSH 连接超时", "Error tables must follow runtime language changes")
        precondition(englishDate != L10n.date(Date(timeIntervalSince1970: 1_700_000_000)))
        AppLanguage.shared.selection = "en"
        precondition(JobDisplayText.errorMessage("SSH connection timed out") == "SSH connection timed out")
        print("Localization tests passed: system detection, persistence, observation, interpolation, resources and runtime switching.")
    }
}
