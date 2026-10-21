import Foundation

/// HTTP client presentation rules from sing-box 1.14.2 shared/http-client, http2 and quic docs.
enum TemplateHTTPClientLayout {
    static let http2 = ["idle_timeout", "keep_alive_period", "stream_receive_window", "connection_receive_window", "max_concurrent_streams"]
    static let quic = ["initial_packet_size", "disable_path_mtu_discovery"]
    static let tlsBasic = ["server_name", "insecure", "min_version", "max_version", "certificate", "certificate_path", "certificate_sha256", "certificate_public_key_sha256"]
    struct TLSSection: Identifiable {
        let title: String
        let keys: [String]
        var id: String { title }
    }
    static func tlsAllowedKeys(_ schema: JSONValue, value: JSONValue) -> Set<String> {
        let keys = Set((schema["properties"].objectValue ?? [:]).keys).subtracting(["enabled"])
        if isApple(value) { return keys.intersection(tlsBasic) }
        if ["apple", "windows"].contains(value["tls"]["engine"].stringValue ?? "") {
            return keys.intersection(tlsBasic + ["engine", "alpn", "handshake_timeout"])
        }
        return keys
    }
    static func tlsSections(_ schema: JSONValue, value: JSONValue) -> [TLSSection] {
        let definitions: [(String, [String])] = [
            ("TLS 基础设置", ["engine", "server_name", "disable_sni", "insecure", "handshake_timeout"]),
            ("TLS 协议协商", ["alpn", "min_version", "max_version", "cipher_suites", "curve_preferences"]),
            ("TLS 证书校验", ["certificate", "certificate_path", "certificate_sha256", "certificate_public_key_sha256"]),
            ("TLS 客户端身份", ["client_certificate", "client_certificate_path", "client_key", "client_key_path"]),
            ("TLS 分片与伪装", ["fragment", "fragment_fallback_delay", "record_fragment", "spoof", "spoof_method"]),
            ("TLS 内核设置", ["kernel_tx", "kernel_rx"]),
            ("ECH", ["ech"]), ("uTLS", ["utls"]), ("Reality", ["reality"])
        ]
        let allowed = tlsAllowedKeys(schema, value: value)
        var sections = definitions.compactMap { title, keys -> TLSSection? in
            let keys = keys.filter { allowed.contains($0) }
            return keys.isEmpty ? nil : TLSSection(title: title, keys: keys)
        }
        let covered = Set(sections.flatMap(\.keys))
        let remaining = allowed.subtracting(covered).sorted()
        if !remaining.isEmpty { sections.append(TLSSection(title: "TLS 其他设置", keys: remaining)) }
        return sections
    }
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
    static func incompatiblePaths(_ value: JSONValue, tlsSchema: JSONValue) -> [[String]] {
        var paths: [[String]] = []
        if isApple(value) {
            paths = (["disable_version_fallback"] + http2 + quic).filter { hasValue(value[$0]) }.map { [$0] }
            if let version = value["version"].integerValue, ![0, 2].contains(version) { paths.insert(["version"], at: 0) }
        }
        let allowed = tlsAllowedKeys(tlsSchema, value: value)
        for key in (tlsSchema["properties"].objectValue ?? [:]).keys.sorted()
            where key != "enabled" && !allowed.contains(key) && hasValue(value["tls"][key]) {
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
