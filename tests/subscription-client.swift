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
        let pair = try decoder.decode(RotatedCredentials.self, from: Data(#"{"user_id":"user","revision":2,"credential":"new-password","token":"new-token","url":"https://example.test/sub/new-token","revocation_job_ids":["kick"]}"#.utf8))
        precondition(pair.credential != pair.token && pair.url.hasSuffix(pair.token))
        let features = ["client_summary", "ssh_dns_sync", "overview_monitoring", "node_record_removal", "job_retry_links", "node_packages", "dns_management"]
        for joint in [false, true] {
            let data = try JSONSerialization.data(withJSONObject: ["api_version":"2.0.0", "management_protocol":2, "service_version":"test", "hysteria_version":"test", "mihomo_version":"test", "directory_version":"b3:test", "features":features + (joint ? ["joint_user_credentials"] : [])])
            let version = try decoder.decode(APIVersion.self, from: data)
            do {
                try version.requireCurrentManagementAPI()
                precondition(joint, "a server without joint rotation must be rejected")
            } catch {
                precondition(!joint, "a server with joint rotation must be accepted")
            }
        }

        let current = try decoder.decode(ActiveSubscription.self, from: Data(#"{"id":"id","token":"token","url":"https://example.test/sub/token","created_at":"2026-10-18T00:00:00Z"}"#.utf8))
        precondition(current.url == "https://example.test/sub/token")
        for format in SubscriptionFileFormat.allCases {
            let native = format == .mihomo || format == .singbox
            let url = try format.subscriptionURL(baseURL: current.url, template: native ? "chosen" : nil, platform: native ? "windows" : nil)
            let query = Dictionary(uniqueKeysWithValues: URLComponents(string: url)!.queryItems!.map { ($0.name, $0.value!) })
            precondition(query["format"] == format.rawValue)
            precondition(query["template"] == (native ? "chosen" : nil))
            precondition(query["platform"] == (native ? "windows" : nil))
        }
        do {
            _ = try SubscriptionFileFormat.mihomo.subscriptionURL(baseURL: current.url)
            preconditionFailure("native URLs must require an explicit target platform")
        } catch APIClientError.server { }
        let operation = APIEndpoints.downloadAutomaticSubscription(token: "token", template: "chosen", format: "singbox", platform: "ios")
        precondition(operation.path == "sub/token")
        precondition(operation.queryParameters == ["template":"chosen", "format":"singbox", "platform":"ios"])
        precondition(APIEndpoints.downloadAutomaticSubscription(token: "token").queryParameters.isEmpty)
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
        print("Swift canonical subscription URLs, template/platform selection, and HTML export guard passed.")
    }
}
