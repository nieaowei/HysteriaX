import Foundation

enum APIClientError: LocalizedError {
    case invalidBaseURL
    case invalidResponse
    case eventCursorExpired
    case incompatibleAPI(String)
    case methodMismatch(expected: String, actual: String)
    case server(String)

    var errorDescription: String? {
        switch self {
        case .invalidBaseURL: L10n.text("请输入有效的 HTTPS 服务地址。")
        case .invalidResponse: L10n.text("管理服务返回了无法识别的响应。")
        case .eventCursorExpired: L10n.text("事件游标已失效，需要重新同步。")
        case .incompatibleAPI(let version): L10n.text("此管理服务 API 版本（{0}）与客户端不兼容。", String(describing: (version)))
        case .methodMismatch(let expected, let actual): L10n.text("OpenAPI 接口要求使用 {0}，客户端传入了 {1}。", String(describing: (expected)), String(describing: (actual)))
        case .server(let message): message
        }
    }
}

protocol APIPage: Decodable, Sendable {
    associatedtype Item: Sendable
    var items: [Item] { get }
    var total: Int { get }
    var page: Int { get }
    var pageSize: Int { get }
}

extension NodesPage: APIPage {}
extension UsersPage: APIPage {}
extension CredentialsPage: APIPage {}
extension DNSRecordsPage: APIPage {}

actor APIClient {
    private let baseURL: URL
    private let token: String
    private let session: URLSession
    private let writeJournal: ManagementWriteJournal

    init(baseURL: URL, token: String, session: URLSession? = nil, writeJournal: ManagementWriteJournal = .shared) {
        self.baseURL = baseURL
        self.token = token
        self.writeJournal = writeJournal
        if let session {
            self.session = session
        } else {
            // Keep management requests fresh and isolate stale transport/cache state.
            let configuration = URLSessionConfiguration.ephemeral
            configuration.urlCache = nil
            configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
            self.session = URLSession(configuration: configuration)
        }
    }

    func get<Value: Decodable>(_ operation: APIOperation<NoRequest, Value>, bypassCache: Bool = false) async throws -> Value {
        try requireMethod(operation, expected: "GET")
        let data = try await perform(operation, method: operation.method, body: nil, bypassCache: bypassCache).data
        return try decode(Value.self, from: data)
    }

    func collectPages<Page: APIPage>(
        bypassCache: Bool = false,
        _ operation: @Sendable (Int) -> APIOperation<NoRequest, Page>
    ) async throws -> [Page.Item] {
        var items: [Page.Item] = []
        var requestedPage = 1
        while true {
            try Task.checkCancellation()
            let response = try await get(operation(requestedPage), bypassCache: bypassCache)
            guard response.page == requestedPage, (1...200).contains(response.pageSize),
                  response.total >= 0, response.items.count <= response.pageSize else {
                throw APIClientError.invalidResponse
            }
            items.append(contentsOf: response.items)
            if response.total == 0 || response.page > (response.total - 1) / response.pageSize {
                return items
            }
            guard !response.items.isEmpty else { throw APIClientError.invalidResponse }
            requestedPage += 1
        }
    }

    func post<Input: Encodable & Sendable, Output: Decodable>(
        _ operation: APIOperation<Input, Output>,
        body: Input
    ) async throws -> Output {
        try requireMethod(operation, expected: "POST")
        let data = try encode(body)
        return try await send(operation, method: operation.method, body: data)
    }

    func patch<Input: Encodable & Sendable, Output: Decodable>(
        _ operation: APIOperation<Input, Output>,
        body: Input
    ) async throws -> Output {
        try requireMethod(operation, expected: "PATCH")
        let data = try encode(body)
        return try await send(operation, method: operation.method, body: data)
    }

    func put<Input: Encodable & Sendable, Output: Decodable>(
        _ operation: APIOperation<Input, Output>,
        body: Input
    ) async throws -> Output {
        try requireMethod(operation, expected: "PUT")
        let data = try encode(body)
        return try await send(operation, method: operation.method, body: data)
    }

    func post<Output: Decodable>(_ operation: APIOperation<NoRequest, Output>) async throws -> Output {
        try requireMethod(operation, expected: "POST")
        return try await send(operation, method: operation.method, body: Optional<Data>.none)
    }

    func download(_ operation: APIOperation<NoRequest, NoResponse>, accept: String = "application/yaml, text/yaml") async throws -> Data {
        try requireMethod(operation, expected: "GET")
        return try await perform(
            operation,
            method: operation.method,
            body: nil,
            accept: accept,
            rejectHTML: true
        ).data
    }

    func events(
        _ operation: APIOperation<NoRequest, NoResponse>,
        lastEventID: String?
    ) async throws -> AsyncThrowingStream<ManagementEvent, Error> {
        try requireMethod(operation, expected: "GET")
        let url = try makeURL(for: operation)
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.setValue("text/event-stream", forHTTPHeaderField: "Accept")
        if let lastEventID {
            request.setValue(lastEventID, forHTTPHeaderField: "Last-Event-ID")
        }

        let (bytes, response) = try await session.bytes(for: request)
        guard let response = response as? HTTPURLResponse else { throw APIClientError.invalidResponse }
        if response.statusCode == 410 { throw APIClientError.eventCursorExpired }
        guard (200..<300).contains(response.statusCode) else {
            throw APIClientError.server(L10n.text("任务事件流返回 HTTP {0}。", String(describing: (response.statusCode))))
        }

        let v2 = operation.path.trimmingCharacters(in: CharacterSet(charactersIn: "/")).hasPrefix("api/v2/")
        return AsyncThrowingStream(bufferingPolicy: .bufferingOldest(256)) { continuation in
            let reader = Task {
                var parser = ManagementEventDecoder(v2: v2)
                do {
                    for try await byte in bytes {
                        if let event = try parser.append(byte) {
                            switch continuation.yield(event) {
                            case .enqueued: break
                            case .dropped: throw APIClientError.invalidResponse
                            case .terminated: return
                            @unknown default: throw APIClientError.invalidResponse
                            }
                        }
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in reader.cancel() }
        }
    }

    func delete<Output: Decodable>(
        _ operation: APIOperation<NoRequest, Output>
    ) async throws -> Output? {
        try requireMethod(operation, expected: "DELETE")
        let result = try await perform(operation, method: operation.method, body: nil)
        let value = result.data.isEmpty ? nil : try decode(Output.self, from: result.data)
        if let identity = result.identity { await writeJournal.acknowledge(identity) }
        return value
    }

    func deleteNoContent(_ operation: APIOperation<NoRequest, NoResponse>) async throws {
        try requireMethod(operation, expected: "DELETE")
        let result = try await perform(operation, method: operation.method, body: nil)
        if let identity = result.identity { await writeJournal.acknowledge(identity) }
    }

    func delete<Input: Encodable & Sendable, Output: Decodable>(
        _ operation: APIOperation<Input, Output>,
        body: Input
    ) async throws -> Output {
        try requireMethod(operation, expected: "DELETE")
        let data = try encode(body)
        return try await send(operation, method: operation.method, body: data)
    }

    private func send<Input: Sendable, Value: Decodable>(
        _ operation: APIOperation<Input, Value>,
        method: String,
        body: Data?
    ) async throws -> Value {
        let result = try await perform(operation, method: method, body: body)
        let value = try decode(Value.self, from: result.data)
        if let identity = result.identity { await writeJournal.acknowledge(identity) }
        return value
    }

    private struct Performed {
        let data: Data
        let identity: ManagementWriteIdentity?
    }

    private func encode<Input: Encodable>(_ body: Input) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return try encoder.encode(body)
    }

    private func perform<Input: Sendable, Output: Sendable>(
        _ operation: APIOperation<Input, Output>,
        method: String,
        body: Data?,
        accept: String = "application/json",
        rejectHTML: Bool = false,
        bypassCache: Bool = false
    ) async throws -> Performed {
        let url = try makeURL(for: operation)
        let identity: ManagementWriteIdentity?
        if method != "GET", operation.path.trimmingCharacters(in: CharacterSet(charactersIn: "/")).hasPrefix("api/v2/") {
            identity = try await writeJournal.reserve(method: method, url: url, token: token, body: body)
        } else { identity = nil }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.setValue(accept, forHTTPHeaderField: "Accept")
        if let identity { request.setValue(identity.key, forHTTPHeaderField: "Idempotency-Key") }
        if bypassCache { request.setValue("no-cache", forHTTPHeaderField: "Cache-Control") }
        if let body {
            request.httpBody = body
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }

        var received: (Data, URLResponse)?
        for attempt in 0..<3 {
            try Task.checkCancellation()
            do {
                let result = try await session.data(for: request)
                if identity != nil, attempt < 2, let http = result.1 as? HTTPURLResponse,
                   [408, 429, 502, 503, 504].contains(http.statusCode) {
                    try await Task.sleep(for: .milliseconds(100 * (attempt + 1)))
                    continue
                }
                received = result
                break
            } catch let error as URLError {
                guard identity != nil, attempt < 2,
                      [.timedOut, .networkConnectionLost, .cannotConnectToHost, .cannotFindHost, .dnsLookupFailed, .notConnectedToInternet].contains(error.code) else { throw error }
                try await Task.sleep(for: .milliseconds(100 * (attempt + 1)))
            }
        }
        guard let (data, rawResponse) = received, let response = rawResponse as? HTTPURLResponse else { throw APIClientError.invalidResponse }
        guard (200..<300).contains(response.statusCode) else {
            if let identity, (400..<500).contains(response.statusCode), ![408, 429].contains(response.statusCode) {
                await writeJournal.acknowledge(identity)
            }
            if let error = try? JSONDecoder().decode(APIErrorResponse.self, from: data) {
                throw APIClientError.server(error.error.message)
            }
            throw APIClientError.server(L10n.text("管理服务返回 HTTP {0}。", String(describing: (response.statusCode))))
        }
        if rejectHTML {
            let contentType = response.value(forHTTPHeaderField: "Content-Type")?.lowercased() ?? ""
            let prefix = String(decoding: data.prefix(256), as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
            guard !contentType.contains("text/html"), !prefix.hasPrefix("<!doctype html"), !prefix.hasPrefix("<html") else {
                throw APIClientError.server(L10n.text("订阅接口返回了网页，请选择明确的订阅格式。"))
            }
        }
        return Performed(data: data, identity: identity)
    }

    private func makeURL<Request: Sendable, Response: Sendable>(
        for operation: APIOperation<Request, Response>
    ) throws -> URL {
        guard var components = URLComponents(
            url: baseURL.appending(path: operation.path),
            resolvingAgainstBaseURL: false
        ) else {
            throw APIClientError.invalidBaseURL
        }
        let queryItems = operation.queryParameters
            .sorted { $0.key < $1.key }
            .map { URLQueryItem(name: $0.key, value: $0.value) }
        if !queryItems.isEmpty { components.queryItems = queryItems }
        guard let url = components.url else { throw APIClientError.invalidBaseURL }
        return url
    }

    private func decode<Value: Decodable>(_ type: Value.Type, from data: Data) throws -> Value {
        do {
            let payload = data.isEmpty && type == NoResponse.self ? Data("{}".utf8) : data
            return try JSONDecoder().decode(type, from: payload)
        } catch {
            throw APIClientError.server(L10n.text("解析管理服务响应失败：{0}", String(describing: (error.localizedDescription))))
        }
    }

    private func requireMethod<Request: Sendable, Response: Sendable>(
        _ operation: APIOperation<Request, Response>,
        expected: String
    ) throws {
        guard operation.method == expected else {
            throw APIClientError.methodMismatch(expected: expected, actual: operation.method)
        }
    }
}
