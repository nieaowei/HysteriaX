import Foundation

enum PatchValue<Value: Encodable & Sendable>: Encodable, Sendable {
    case null
    case value(Value)

    func encode(to encoder: Encoder) throws {
        switch self {
        case .null:
            var container = encoder.singleValueContainer()
            try container.encodeNil()
        case .value(let value):
            try value.encode(to: encoder)
        }
    }
}

enum ProxyProbeURLValidation {
    static func error(_ value: String) -> String? {
        let value = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty else { return nil }
        guard value.unicodeScalars.count <= 2048,
              let components = URLComponents(string: value),
              components.scheme == "http",
              let host = components.host, !host.isEmpty,
              components.user == nil,
              components.password == nil,
              components.query == nil,
              components.fragment == nil else {
            return L10n.text("探测地址必须是有效的 HTTP URL，不能包含认证信息、查询参数或片段。")
        }
        return nil
    }
}

enum JSONValue: Codable, Sendable, Equatable {
    case string(String)
    case integer(Int)
    case unsignedInteger(UInt64)
    case bool(Bool)
    case decimal(Double)
    case array([JSONValue])
    case object([String: JSONValue])
    case null

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() { self = .null }
        else if let value = try? container.decode(Bool.self) { self = .bool(value) }
        else if let value = try? container.decode(Int.self) { self = .integer(value) }
        else if let value = try? container.decode(UInt64.self) { self = .unsignedInteger(value) }
        else if let value = try? container.decode(Double.self) { self = .decimal(value) }
        else if let value = try? container.decode(String.self) { self = .string(value) }
        else if let value = try? [JSONValue](from: decoder) { self = .array(value) }
        else { self = .object(try [String: JSONValue](from: decoder)) }
    }

    func encode(to encoder: Encoder) throws {
        switch self {
        case .string(let value): try value.encode(to: encoder)
        case .integer(let value): try value.encode(to: encoder)
        case .unsignedInteger(let value): try value.encode(to: encoder)
        case .bool(let value): try value.encode(to: encoder)
        case .decimal(let value): try value.encode(to: encoder)
        case .array(let value): try value.encode(to: encoder)
        case .object(let value): try value.encode(to: encoder)
        case .null: var container = encoder.singleValueContainer(); try container.encodeNil()
        }
    }

    var arrayValue: [JSONValue]? {
        guard case .array(let value) = self else { return nil }
        return value
    }

    var objectValue: [String: JSONValue]? {
        guard case .object(let value) = self else { return nil }
        return value
    }

    var stringValue: String? {
        guard case .string(let value) = self else { return nil }
        return value
    }

    var boolValue: Bool? {
        guard case .bool(let value) = self else { return nil }
        return value
    }

    var integerValue: Int? {
        guard case .integer(let value) = self else { return nil }
        return value
    }
}

/// Explicit formats ensure that exports never negotiate an HTML selection page.
enum SubscriptionFileFormat: String, CaseIterable, Sendable {
    case mihomo, singbox, base64, uri

    var title: String {
        switch self {
        case .mihomo: "Mihomo YAML"
        case .singbox: "sing-box JSON"
        case .base64: L10n.text("Base64 节点订阅")
        case .uri: L10n.text("纯文本节点链接")
        }
    }

    var fileExtension: String {
        switch self {
        case .mihomo: "yaml"
        case .singbox: "json"
        case .base64, .uri: "txt"
        }
    }

    var accept: String {
        switch self {
        case .mihomo: "application/yaml, text/yaml"
        case .singbox: "application/json"
        case .base64, .uri: "text/plain"
        }
    }

    func subscriptionURL(baseURL: String, template: String? = nil, platform: String? = nil) throws -> String {
        guard var components = URLComponents(string: baseURL) else { throw APIClientError.invalidBaseURL }
        if self == .mihomo || self == .singbox {
            guard let platform, TemplatePlatform.all.contains(platform) else { throw APIClientError.server(L10n.text("请明确选择目标平台。")) }
        } else if template != nil { throw APIClientError.invalidResponse }
        var items = (components.queryItems ?? []).filter { !["format", "platform", "template"].contains($0.name) }
        items.append(URLQueryItem(name: "format", value: rawValue))
        if let template { items.append(URLQueryItem(name: "template", value: template)) }
        if let platform, self == .mihomo || self == .singbox { items.append(URLQueryItem(name: "platform", value: platform)) }
        components.queryItems = items
        guard let url = components.url else { throw APIClientError.invalidBaseURL }; return url.absoluteString
    }
}

enum TemplatePlatform {
    static let all = ["linux", "macos", "windows", "android", "ios"]
    static func title(_ value: String) -> String {
        switch value { case "linux": "Linux/OpenWrt"; case "macos": "macOS"; case "windows": "Windows"; case "android": "Android"; case "ios": "iOS"; default: L10n.text("通用配置") }
    }
}
