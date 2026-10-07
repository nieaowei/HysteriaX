import Foundation

enum APIClientError: LocalizedError {
    case invalidBaseURL
    case invalidResponse
    case incompatibleAPI(String)
    case methodMismatch(expected: String, actual: String)
    case server(String)

    var errorDescription: String? {
        switch self {
        case .invalidBaseURL: L10n.text("请输入有效的 HTTPS 服务地址。")
        case .invalidResponse: L10n.text("管理服务返回了无法识别的响应。")
        case .incompatibleAPI(let version): L10n.text("此管理服务 API 版本（{0}）与客户端不兼容。", String(describing: (version)))
        case .methodMismatch(let expected, let actual): L10n.text("OpenAPI 接口要求使用 {0}，客户端传入了 {1}。", String(describing: (expected)), String(describing: (actual)))
        case .server(let message): message
        }
    }
}

actor APIClient {
    private let baseURL: URL
    private let token: String
    private let session: URLSession

    init(baseURL: URL, token: String, session: URLSession? = nil) {
        self.baseURL = baseURL
        self.token = token
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

    func get<Value: Decodable>(_ operation: APIOperation<NoRequest, Value>) async throws -> Value {
        try requireMethod(operation, expected: "GET")
        return try await send(operation, method: operation.method, body: Optional<Data>.none)
    }

    func post<Input: Encodable & Sendable, Output: Decodable>(
        _ operation: APIOperation<Input, Output>,
        body: Input
    ) async throws -> Output {
        try requireMethod(operation, expected: "POST")
        let data = try JSONEncoder().encode(body)
        return try await send(operation, method: operation.method, body: data)
    }

    func patch<Input: Encodable & Sendable, Output: Decodable>(
        _ operation: APIOperation<Input, Output>,
        body: Input
    ) async throws -> Output {
        try requireMethod(operation, expected: "PATCH")
        let data = try JSONEncoder().encode(body)
        return try await send(operation, method: operation.method, body: data)
    }

    func put<Input: Encodable & Sendable, Output: Decodable>(
        _ operation: APIOperation<Input, Output>,
        body: Input
    ) async throws -> Output {
        try requireMethod(operation, expected: "PUT")
        let data = try JSONEncoder().encode(body)
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
        )
    }

    func events(
        _ operation: APIOperation<NoRequest, NoResponse>,
        lastEventID: String?
    ) async throws -> AsyncThrowingStream<String, Error> {
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
        guard (200..<300).contains(response.statusCode) else {
            throw APIClientError.server(L10n.text("任务事件流返回 HTTP {0}。", String(describing: (response.statusCode))))
        }

        return AsyncThrowingStream { continuation in
            let reader = Task {
                var pendingID: String?
                do {
                    for try await line in bytes.lines {
                        if line.hasPrefix("id:") {
                            pendingID = line.dropFirst(3).trimmingCharacters(in: .whitespaces)
                        } else if line.isEmpty, let eventID = pendingID {
                            continuation.yield(eventID)
                            pendingID = nil
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
        let data = try await perform(operation, method: operation.method, body: nil)
        guard !data.isEmpty else { return nil }
        return try decode(Output.self, from: data)
    }

    func deleteNoContent(_ operation: APIOperation<NoRequest, NoResponse>) async throws {
        try requireMethod(operation, expected: "DELETE")
        _ = try await perform(operation, method: operation.method, body: nil)
    }

    func delete<Input: Encodable & Sendable, Output: Decodable>(
        _ operation: APIOperation<Input, Output>,
        body: Input
    ) async throws -> Output {
        try requireMethod(operation, expected: "DELETE")
        let data = try JSONEncoder().encode(body)
        let responseData = try await perform(operation, method: operation.method, body: data)
        return try decode(Output.self, from: responseData)
    }

    private func send<Input: Sendable, Value: Decodable>(
        _ operation: APIOperation<Input, Value>,
        method: String,
        body: Data?
    ) async throws -> Value {
        let data = try await perform(operation, method: method, body: body)
        return try decode(Value.self, from: data)
    }

    private func perform<Input: Sendable, Output: Sendable>(
        _ operation: APIOperation<Input, Output>,
        method: String,
        body: Data?,
        accept: String = "application/json",
        rejectHTML: Bool = false
    ) async throws -> Data {
        let url = try makeURL(for: operation)
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.setValue(accept, forHTTPHeaderField: "Accept")
        if let body {
            request.httpBody = body
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }

        let (data, response) = try await session.data(for: request)
        guard let response = response as? HTTPURLResponse else { throw APIClientError.invalidResponse }
        guard (200..<300).contains(response.statusCode) else {
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
        return data
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
            return try JSONDecoder().decode(type, from: data)
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
