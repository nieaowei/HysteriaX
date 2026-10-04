import Foundation

// Field names match Hysteria app/v2.12.3 app/cmd/server.go.
struct ACMEDNSField: Identifiable {
    let key: String
    let title: String
    var required = true
    var secret = true
    var help: String? = nil
    var id: String { key }
}

enum ACMEDNSProvider: String, CaseIterable, Identifiable {
    case cloudflare, duckdns, gandi, godaddy, namecheap, njalla, porkbun, vultr

    var id: String { rawValue }
    var title: String {
        switch self {
        case .cloudflare: "Cloudflare"
        case .duckdns: "DuckDNS"
        case .gandi: "Gandi"
        case .godaddy: "GoDaddy"
        case .namecheap: "Namecheap"
        case .njalla: "Njalla"
        case .porkbun: "Porkbun"
        case .vultr: "Vultr"
        }
    }

    var fields: [ACMEDNSField] {
        switch self {
        case .duckdns:
            return [
                ACMEDNSField(key: "duckdns_api_token", title: "API Token"),
                ACMEDNSField(key: "duckdns_override_domain", title: "覆盖域名", required: false, secret: false,
                             help: "通过 CNAME 指向 DuckDNS 时填写目标域名；直接使用 DuckDNS 域名可留空。"),
            ]
        case .namecheap:
            return [
                ACMEDNSField(key: "namecheap_api_key", title: "API Key"),
                ACMEDNSField(key: "namecheap_api_user", title: "API User", secret: false),
                ACMEDNSField(key: "namecheap_client_ip", title: "Client IP", required: false, secret: false,
                             help: "发起请求的节点公网 IPv4，须加入 Namecheap API 白名单。留空由节点自动探测。"),
                ACMEDNSField(key: "namecheap_api_endpoint", title: "API Endpoint", required: false, secret: false,
                             help: "默认使用正式环境；仅需沙箱或自定义端点时填写完整 HTTP(S) URL。"),
            ]
        case .porkbun:
            return [
                ACMEDNSField(key: "porkbun_api_key", title: "API Key"),
                ACMEDNSField(key: "porkbun_api_secret_key", title: "API Secret Key"),
            ]
        case .godaddy:
            return [ACMEDNSField(key: "godaddy_api_token", title: "API Token",
                                 help: "按 API Key:API Secret 格式填写。")]
        default:
            return [ACMEDNSField(key: "\(rawValue)_api_token", title: "API Token")]
        }
    }
}

struct ACMEDNSDraft {
    var provider = ""
    // Provider-specific drafts prevent credentials leaking across provider switches.
    private var configs: [String: [String: String]] = [:]

    init() {}

    init(provider: String, config: [String: JSONValue]) {
        self.provider = provider.lowercased()
        configs[self.provider] = config.compactMapValues(\.stringValue)
    }

    var definition: ACMEDNSProvider? { ACMEDNSProvider(rawValue: provider) }
    var values: [String: String] { configs[provider] ?? [:] }
    var unknownKeys: [String] {
        let known = Set(definition?.fields.map(\.key) ?? [])
        return values.keys.filter { !known.contains($0) }.sorted()
    }

    mutating func setValue(_ value: String, for key: String) {
        configs[provider, default: [:]][key] = value
    }

    mutating func removeValue(for key: String) {
        configs[provider]?.removeValue(forKey: key)
    }

    var configuration: [String: JSONValue] {
        var config = values
        // Omit empty known fields, but preserve unknown legacy parameters verbatim.
        for field in definition?.fields ?? [] where config[field.key]?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty == true {
            config.removeValue(forKey: field.key)
        }
        return config.mapValues(JSONValue.string)
    }

    var validationError: String? {
        guard let definition else { return "请选择支持的 DNS 服务商。" }
        for field in definition.fields where field.required {
            if values[field.key, default: ""].trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                return "请填写 \(definition.title) 的 \(field.title)。"
            }
        }
        if provider == "namecheap" {
            let ip = values["namecheap_client_ip", default: ""]
            if !ip.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, !Self.isIPv4Address(ip) {
                return "Namecheap Client IP 必须是有效的 IPv4 地址。"
            }
            let endpoint = values["namecheap_api_endpoint", default: ""]
            if !endpoint.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                guard let url = URLComponents(string: endpoint),
                      ["http", "https"].contains(url.scheme?.lowercased() ?? ""),
                      let host = url.host, !host.isEmpty,
                      !endpoint.contains(where: { $0.isWhitespace }) else {
                    return "Namecheap API Endpoint 必须是完整的 HTTP(S) URL。"
                }
            }
        }
        return nil
    }

    private static func isIPv4Address(_ value: String) -> Bool {
        let parts = value.split(separator: ".", omittingEmptySubsequences: false)
        return parts.count == 4 && parts.allSatisfy { part in
            !part.isEmpty && part.utf8.allSatisfy { (48...57).contains($0) }
                && (part.count == 1 || part.first != "0")
                && UInt8(part) != nil
        }
    }

    static func redactingDNSValues(in config: [String: JSONValue]) -> [String: JSONValue] {
        var result = config
        if var acme = result["acme"]?.objectValue,
           var dns = acme["dns"]?.objectValue,
           let values = dns["config"]?.objectValue {
            dns["config"] = .object(values.mapValues { _ in .string("<redacted>") })
            acme["dns"] = .object(dns)
            result["acme"] = .object(acme)
        }
        return result
    }
}
