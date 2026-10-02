import Foundation

final class SubscriptionURLProtocol: URLProtocol, @unchecked Sendable {
    nonisolated(unsafe) static var contentType = "application/json"
    nonisolated(unsafe) static var body = Data("{\"outbounds\":[]}".utf8)

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        let response = HTTPURLResponse(url: request.url!, statusCode: 200, httpVersion: nil,
                                       headerFields: ["Content-Type": Self.contentType])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Self.body)
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}

@main struct SubscriptionClientTests {
    static func main() async throws {
        let decoder = JSONDecoder()
        let old = Data(#"{"id":"id","token":"token","url":"https://example.test/sub/token/clash.yaml","created_at":"2026-10-02T00:00:00Z"}"#.utf8)
        let active = try decoder.decode(ActiveSubscription.self, from: old)
        precondition(active.autoUrl == nil)
        precondition((active.autoUrl ?? active.url).hasSuffix("/clash.yaml"))
        let modern = Data(#"{"id":"id","token":"token","url":"https://example.test/sub/token/clash.yaml","auto_url":"https://example.test/sub/token","created_at":"2026-10-02T00:00:00Z"}"#.utf8)
        let current = try decoder.decode(ActiveSubscription.self, from: modern)
        precondition((current.autoUrl ?? current.url) == "https://example.test/sub/token")
        let receipt = try decoder.decode(SubscriptionReceipt.self, from: Data(#"{"user_id":"id","revision":2,"token":"token","url":"https://example.test/sub/token/clash.yaml","note":"rotated"}"#.utf8))
        precondition(receipt.autoUrl == nil)
        for format in SubscriptionFileFormat.allCases {
            let url = try format.subscriptionURL(autoURL: current.autoUrl, legacyURL: current.url)
            let components = URLComponents(string: url)!
            if format == .mihomo { precondition(url == current.url) }
            else { precondition(components.queryItems == [URLQueryItem(name: "format", value: format.rawValue)]) }
            precondition(["yaml", "json", "txt"].contains(format.fileExtension))
        }
        do {
            _ = try SubscriptionFileFormat.singbox.subscriptionURL(autoURL: nil, legacyURL: active.url)
            preconditionFailure("old servers must not expose unsupported format URLs")
        } catch APIClientError.server { }
        let legacyURL = try SubscriptionFileFormat.mihomo.subscriptionURL(autoURL: nil, legacyURL: active.url)
        precondition(legacyURL == active.url)
        let operation = APIEndpoints.downloadAutomaticSubscription(token: "token", format: "singbox")
        precondition(operation.path == "sub/token")
        precondition(operation.queryParameters == ["format": "singbox"])
        precondition(APIEndpoints.downloadAutomaticSubscription(token: "token").queryParameters.isEmpty)
        precondition(APIEndpoints.downloadSubscription(token: "token").path == "sub/token/clash.yaml")
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [SubscriptionURLProtocol.self]
        let client = APIClient(baseURL: URL(string: "https://example.test")!, token: "fixture", session: URLSession(configuration: configuration))
        let downloaded = try await client.download(operation, accept: "application/json")
        precondition(downloaded == SubscriptionURLProtocol.body)
        for mime in ["text/html; charset=utf-8", "application/json"] {
            SubscriptionURLProtocol.contentType = mime
            SubscriptionURLProtocol.body = Data("<!doctype html><html>Choose a format</html>".utf8)
            do {
                _ = try await client.download(operation, accept: "application/json")
                preconditionFailure("HTML must not be exported as configuration")
            } catch APIClientError.server { }
        }
        print("Swift subscription DTO compatibility, explicit URLs, extensions, and HTML export guard passed.")
    }
}
