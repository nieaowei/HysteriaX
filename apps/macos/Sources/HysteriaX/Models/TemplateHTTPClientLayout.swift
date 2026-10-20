import Foundation

/// HTTP client presentation rules from sing-box 1.14.2 shared/http-client, http2 and quic docs.
enum TemplateHTTPClientLayout {
    static let http2 = ["idle_timeout", "keep_alive_period", "stream_receive_window", "connection_receive_window", "max_concurrent_streams"]
    static let quic = ["initial_packet_size", "disable_path_mtu_discovery"]
    static let tlsBasic = ["server_name", "insecure", "min_version", "max_version", "certificate", "certificate_path", "certificate_sha256", "certificate_public_key_sha256"]
    static func matches(_ schema: JSONValue) -> Bool {
        let keys = Set(schema["properties"].objectValue?.keys.map { $0 } ?? [])
        return schema["type"].stringValue == "object" && keys.isSuperset(of: ["engine", "version", "disable_version_fallback", "headers"])
    }
    static func isApple(_ value: JSONValue) -> Bool { value["engine"].stringValue == "apple" }
    static func version(_ value: JSONValue) -> Int {
        let version = value["version"].integerValue ?? 0
        return version == 0 ? 2 : version
    }
    static func transportKeys(_ value: JSONValue) -> [String] {
        guard !isApple(value) else { return [] }
        switch version(value) {
        case 2: return http2
        case 3: return http2 + quic
        default: return []
        }
    }
    static func versionTitle(_ value: Int) -> String {
        switch value {
        case 0: L10n.text("默认（HTTP/2）")
        case 1: "HTTP/1.1"
        case 2: "HTTP/2"
        case 3: "HTTP/3"
        default: String(value)
        }
    }
    static func referenceTitle(_ schema: JSONValue) -> String? {
        if schema["x-tag-reference"].stringValue == "http_client" { return L10n.text("共享客户端") }
        if matches(schema) { return L10n.text("自定义配置") }
        return nil
    }
    static func hasValue(_ value: JSONValue) -> Bool {
        switch value {
        case .null: false
        case .bool(let flag): flag
        case .string(let text): !text.isEmpty
        case .integer(let number): number != 0
        case .unsignedInteger(let number): number != 0
        case .decimal(let number): number != 0
        case .array(let values): !values.isEmpty
        case .object(let fields): !fields.isEmpty
        }
    }
    static func memoryValue(_ text: String) -> JSONValue {
        if let number = Int(text) { return .integer(number) }
        if let number = UInt64(text) { return .unsignedInteger(number) }
        return .string(text)
    }
    static func incompatiblePaths(_ value: JSONValue, tlsSchema: JSONValue) -> [[String]] {
        guard isApple(value) else { return [] }
        var paths = (["disable_version_fallback"] + http2 + quic).filter { hasValue(value[$0]) }.map { [$0] }
        if let version = value["version"].integerValue, ![0, 2].contains(version) { paths.insert(["version"], at: 0) }
        for key in (tlsSchema["properties"].objectValue ?? [:]).keys.sorted()
            where key != "enabled" && !tlsBasic.contains(key) && hasValue(value["tls"][key]) {
            paths.append(["tls", key])
        }
        return paths
    }
    static func clearingIncompatible(_ value: JSONValue, tlsSchema: JSONValue) -> JSONValue {
        var next = value
        for path in incompatiblePaths(value, tlsSchema: tlsSchema) { next.set(path, nil) }
        return next
    }
}
