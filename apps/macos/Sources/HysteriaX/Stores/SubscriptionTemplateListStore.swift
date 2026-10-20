import Foundation
import Observation

/// The directory owns metadata only. Open documents live in TemplateEditorWindows.
@MainActor @Observable
final class SubscriptionTemplateListStore {
    var templates: [SubscriptionTemplate] = []
    var busy = false
    var error: String?
    @ObservationIgnored var sessionAvailable: ((APIClient) -> Bool)?
    private var generation = UUID()
    func loadList(_ api: APIClient) async throws {
        let request = UUID(); generation = request
        let items = try await api.collectPages { page in APIEndpoints.listSubscriptionTemplates(page: page, pageSize: 200) }
        guard request == generation, sessionAvailable?(api) != false else { throw CancellationError() }
        templates = items
    }
    func create(_ api: APIClient, name: String, format: String) async throws -> SubscriptionTemplate {
        guard sessionAvailable?(api) != false else { throw CancellationError() }
        let item = try await api.post(APIEndpoints.createSubscriptionTemplate, body: CreateSubscriptionTemplateRequest(expectedRevision: 0, name: name, format: format))
        guard sessionAvailable?(api) != false else { throw CancellationError() }
        try await loadList(api)
        return item
    }
    func run(_ action: @escaping @MainActor () async throws -> Void) {
        guard !busy else { return }; busy = true; error = nil
        Task {
            defer { busy = false }
            do { try await action() }
            catch is CancellationError { }
            catch { self.error = error.localizedDescription }
        }
    }
}
