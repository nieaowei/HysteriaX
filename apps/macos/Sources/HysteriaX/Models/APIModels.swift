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
            return "探测地址必须是有效的 HTTP URL，不能包含认证信息、查询参数或片段。"
        }
        return nil
    }
}

enum JSONValue: Codable, Sendable {
    case string(String)
    case integer(Int)
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
        else if let value = try? container.decode(Double.self) { self = .decimal(value) }
        else if let value = try? container.decode(String.self) { self = .string(value) }
        else if let value = try? [JSONValue](from: decoder) { self = .array(value) }
        else { self = .object(try [String: JSONValue](from: decoder)) }
    }

    func encode(to encoder: Encoder) throws {
        switch self {
        case .string(let value): try value.encode(to: encoder)
        case .integer(let value): try value.encode(to: encoder)
        case .bool(let value): try value.encode(to: encoder)
        case .decimal(let value): try value.encode(to: encoder)
        case .array(let value): try value.encode(to: encoder)
        case .object(let value): try value.encode(to: encoder)
        case .null: var container = encoder.singleValueContainer(); try container.encodeNil()
        }
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
