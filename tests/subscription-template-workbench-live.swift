import Foundation

private struct WorkbenchFixture: Decodable {
    let base: String
    let token: String
    let templateId: String
    let userId: String
    let subscriptionToken: String
}

/// Exercise the production editor against the same disposable service used by the UI suite.
@main struct SubscriptionTemplateWorkbenchLiveTests {
    @MainActor static func main() async throws {
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        let fixture = try decoder.decode(WorkbenchFixture.self, from: Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1])))
        let api = APIClient(baseURL: URL(string: fixture.base)!, token: fixture.token)
        let draft = try await api.get(APIEndpoints.getSubscriptionTemplateDraft(id: fixture.templateId))
        let model = SubscriptionTemplateStore()
        try await model.open(draft.template, api: api)
        model.set(["mixed-port"], .integer(8100))
        try await model.save(api)
        precondition(!model.dirty && model.selected?.publishedVersion == nil)

        try await model.switchMode(api)
        let validSource = model.source
        precondition(validSource.contains("mixed-port: 8100"))
        model.source = "mixed-port: ["
        do { try await model.switchMode(api); preconditionFailure("Invalid source must prevent leaving source mode") }
        catch APIClientError.server { }
        precondition(model.sourceMode && model.source == "mixed-port: [")
        model.source = validSource; try await model.switchMode(api)
        precondition(model.document["schema_version"] == .integer(2) && !model.needsPlatformConversion)
        model.set(["mixed-port"], .integer(0))
        try await model.save(api)
        model.previewUserID = fixture.userId; model.previewPlatform = "windows"
        try await model.render(api, user: fixture.userId, platform: "windows", version: "", reveal: false)
        precondition(!model.previewStale && model.preview["source"]?.stringValue?.contains("mixed-port: 0") == true)
        try await model.preparePublish(api)
        precondition(!(model.publishDiff.arrayValue ?? []).isEmpty)
        try await model.publish(api)
        precondition(model.selected?.publishedVersion == 1)
        let published = try await download(fixture)
        precondition(published.contains("mixed-port: 0"))

        model.set(["mixed-port"], .integer(9001))
        precondition(model.previewStale)
        try await model.save(api)
        let unchanged = try await download(fixture)
        precondition(unchanged.contains("mixed-port: 0"), "Saving a draft must preserve the published subscription")
        try await model.preparePublish(api)
        precondition(model.publishDiff.arrayValue?.contains { $0["path"].stringValue == "/common/mixed-port" } == true)
        try await model.publish(api)
        precondition(model.selected?.publishedVersion == 2 && model.history.count == 2)
        let updated = try await download(fixture)
        precondition(updated.contains("mixed-port: 9001"))
        try await model.compare(api, version: 1)
        precondition(!(SubscriptionTemplateStore.difference(model.comparison!, model.document).arrayValue ?? []).isEmpty)
        try await model.rollback(api, version: 1)
        precondition(model.selected?.publishedVersion == 3 && !model.dirty)
        let rolledBack = try await download(fixture)
        precondition(rolledBack.contains("mixed-port: 0"))

        let source = """
        mixed-port: 8200
        proxies:
          - {name: legacy, type: ss, server: example.invalid, password: fixture}
        proxy-groups:
          - {name: Select, type: select, proxies: [legacy]}
        rules:
          - MATCH,Select
        future-field: {values: [false, null, 0], counter: 18446744073709551615}
        """
        try await model.analyzeImport(api, source: source)
        let review = SubscriptionTemplateStore()
        review.connectedAPI = api; review.selected = model.selected; review.catalog = model.catalog
        review.document = model.pendingImport!["document"]!; review.baseline = review.document
        review.edit { $0.set(["injections", "0", "contains"], .string("edge")) }
        try await review.validate(api)
        precondition(!review.diagnostics.contains { $0["severity"].stringValue == "error" })
        model.pendingImport?["document"] = review.document
        model.pendingImport?["diagnostics"] = .array(review.diagnostics)
        model.acceptImport()
        precondition(model.dirty)
        try await model.save(api)
        precondition(model.document["injections"].arrayValue?.first?["contains"] == .string("edge"))
        precondition(model.common["future-field"]["values"] == .array([.bool(false), .null, .integer(0)]))
        precondition(model.common["future-field"]["counter"] == .unsignedInteger(UInt64.max))
        let stillPublished = try await download(fixture)
        precondition(stillPublished.contains("mixed-port: 0") && model.selected?.publishedVersion == 3)
        let package = try await api.get(APIEndpoints.exportSubscriptionTemplate(id: fixture.templateId))
        precondition(package["document"] == model.document && package["package_version"]?.integerValue == 1)
        print("Real-service workbench source, preview, save/publish isolation, history/rollback, and import/export passed.")
    }

    private static func download(_ fixture: WorkbenchFixture) async throws -> String {
        var url = URLComponents(string: fixture.base + "/sub/" + fixture.subscriptionToken)!
        url.queryItems = [URLQueryItem(name: "template", value: fixture.templateId), URLQueryItem(name: "format", value: "mihomo"), URLQueryItem(name: "platform", value: "windows")]
        var request = URLRequest(url: url.url!, cachePolicy: .reloadIgnoringLocalCacheData)
        request.timeoutInterval = 10
        let (data, response) = try await URLSession.shared.data(for: request)
        precondition((response as? HTTPURLResponse)?.statusCode == 200)
        return String(decoding: data, as: UTF8.self)
    }
}
