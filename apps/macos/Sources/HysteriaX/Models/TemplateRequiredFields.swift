import Foundation

/// Presentation metadata audited against SagerNet/sing-box v1.14.2 docs/configuration.
/// Supplements incomplete JSON Schema required lists; does not change validation or saved values.
enum TemplateRequiredFields {
    static func contains(_ field: String, definition: JSONValue, schema: JSONValue, value: JSONValue, path: [String], root: JSONValue) -> Bool {
        guard schema["properties"].objectValue?[field] != nil else { return false }
        guard root["$defs"]["DNSServer"].objectValue != nil else {
            return mihomoContains(field, value: value, path: path)
        }
        let name = definition["$ref"].stringValue?.components(separatedBy: "/").last ?? ""
        let discriminator = schema["properties"]["type"]
        let kind = value["type"].stringValue.flatMap { $0.isEmpty ? nil : $0 }
            ?? discriminator["const"].stringValue
            ?? discriminator["enum"].arrayValue?.compactMap(\.stringValue).first(where: { !$0.isEmpty }) ?? "default"
        var fields = singbox[name, default: []].union(singbox[name + ":" + kind, default: []])
        if name == "Inbound", schema["properties"].objectValue?["listen"] != nil { fields.insert("listen") }
        if ["Rule", "NestedRule", "RuleAction"].contains(name), (value["action"].stringValue ?? "route") == "route" { fields.insert("outbound") }
        if ["DNSRule", "NestedDNSRule", "DNSRuleAction"].contains(name), ["", "route", "evaluate"].contains(value["action"].stringValue ?? "route") { fields.insert("server") }
        guard fields.contains(field) else { return false }
        if name == "RuleSet", field == "format" {
            let source = value[kind == "local" ? "path" : "url"].stringValue ?? ""
            let filename = kind == "local" ? source : (URLComponents(string: source)?.path ?? source)
            return !["json", "srs"].contains((filename as NSString).pathExtension.lowercased())
        }
        if ["InboundRealityOptions", "OutboundRealityOptions", "BrutalOptions", "HTTPProxyOptions"].contains(name), value["enabled"].boolValue != true { return false }
        if name == "Inbound", kind == "shadowtls", field == "handshake", value["wildcard_sni"].stringValue == "all" { return false }
        if name == "Outbound", kind == "hysteria2", ["server", "server_port"].contains(field), value["realm"].objectValue != nil { return false }
        if name == "Outbound", kind == "hysteria2", field == "server_port",
           !(value["server_ports"].arrayValue ?? []).isEmpty || !(value["server_ports"].stringValue ?? "").isEmpty { return false }
        if ["Inbound", "Outbound"].contains(name), kind == "hysteria" {
            if ["up", "down"].contains(field), (value[field + "_mbps"].integerValue ?? 0) > 0 { return false }
            if ["up_mbps", "down_mbps"].contains(field), !(value[String(field.dropLast(5))].stringValue ?? "").isEmpty { return false }
        }
        if name == "Service", kind == "usbip-server", field == "devices", !["", "default"].contains(value["provider"].stringValue ?? "") { return false }
        return true
    }

    /// Audited against MetaCubeX/mihomo v1.19.31 adapter/outboundgroup/parser.go,
    /// rules/provider/parse.go and listener/inbound/base.go.
    private static func mihomoContains(_ field: String, value: JSONValue, path: [String]) -> Bool {
        guard path.count == 2 else { return false }
        switch path.first {
        case "proxy-groups":
            if ["name", "type"].contains(field) { return true }
            let automatic = ["include-all", "include-all-proxies", "include-all-providers"].contains { value[$0].boolValue == true }
            if automatic { return false }
            if field == "proxies" { return (value["use"].arrayValue ?? []).isEmpty }
            if field == "use" { return (value["proxies"].arrayValue ?? []).isEmpty }
        case "rule-providers":
            return ["type", "behavior"].contains(field)
                || (field == "url" && value["type"].stringValue == "http")
                || (field == "path" && value["type"].stringValue == "file")
        case "listeners":
            return ["name", "type"].contains(field)
                || (value["type"].stringValue == "tunnel" && ["network", "target"].contains(field))
        default: break
        }
        return false
    }

    static let singbox: [String: Set<String>] = [
        "Endpoint:openvpn-server": ["address"],
        "Endpoint:openconnect": ["server"],
        "BrutalOptions": ["up_mbps", "down_mbps"],
        "CertificateProvider:acme": ["domain"],
        "CertificateProvider:cloudflare-origin-ca": ["domain"],
        "CertificateProvider:tailscale": ["endpoint"],
        "DNSRule:logical": ["mode", "rules"],
        "DNSServer:h3": ["server"],
        "DNSServer:https": ["server"],
        "DNSServer:openconnect": ["endpoint"],
        "DNSServer:openvpn": ["endpoint"],
        "DNSServer:quic": ["server"],
        "DNSServer:resolved": ["service"],
        "DNSServer:tailscale": ["endpoint"],
        "DNSServer:tcp": ["server"],
        "DNSServer:tls": ["server"],
        "DNSServer:udp": ["server"],
        "Endpoint:wireguard": ["address", "private_key", "peers"],
        "HTTPProxyOptions": ["server", "server_port"],
        "HeadlessRule:logical": ["mode", "rules"],
        "Hysteria2InboundRealm": ["server_url", "realm_id", "stun_servers"],
        "Hysteria2Realm": ["server_url", "realm_id", "stun_servers"],
        "HysteriaRealmUser": ["name", "token"],
        "Inbound:anytls": ["users"],
        "Inbound:cloudflared": ["token"],
        "Inbound:hysteria": ["up", "down", "up_mbps", "down_mbps", "tls"],
        "Inbound:hysteria2": ["tls"],
        "Inbound:naive": ["users"],
        "Inbound:shadowsocks": ["method", "password"],
        "Inbound:shadowtls": ["handshake"],
        "Inbound:snell": ["version", "psk"],
        "Inbound:trojan": ["users"],
        "Inbound:tuic": ["tls"],
        "Inbound:vless": ["users"],
        "Inbound:vmess": ["users"],
        "InboundRealityHandshakeOptions": ["server"],
        "InboundRealityOptions": ["handshake", "private_key", "short_id"],
        "NTPOptions": ["server"],
        "NestedDNSRule:logical": ["mode", "rules"],
        "NestedRule:logical": ["mode", "rules"],
        "NetworkNamespace:default": ["path"],
        "OpenConnectTokenOptions": ["mode"],
        "OpenVPNInboundControlWrapOptions": ["type"],
        "OpenVPNPullFilterOptions": ["action", "text"],
        "OpenVPNRemoteOptions": ["server", "server_port"],
        "Outbound:anytls": ["server", "server_port", "password", "tls"],
        "Outbound:http": ["server", "server_port"],
        "Outbound:hysteria": ["server", "server_port", "up", "down", "up_mbps", "down_mbps", "tls"],
        "Outbound:hysteria2": ["server", "server_port", "tls"],
        "Outbound:naive": ["server", "server_port", "tls"],
        "Outbound:selector": ["outbounds"],
        "Outbound:shadowsocks": ["server", "server_port", "method", "password"],
        "Outbound:shadowtls": ["server", "server_port", "tls"],
        "Outbound:snell": ["server", "server_port", "version", "psk"],
        "Outbound:socks": ["server", "server_port"],
        "Outbound:ssh": ["server"],
        "Outbound:trojan": ["server", "server_port", "password"],
        "Outbound:tuic": ["server", "server_port", "uuid", "tls"],
        "Outbound:urltest": ["outbounds"],
        "Outbound:vless": ["server", "server_port", "uuid"],
        "Outbound:vmess": ["server", "server_port", "uuid"],
        "OutboundRealityOptions": ["public_key", "short_id"],
        "Rule:logical": ["mode", "rules"],
        "RuleSet:inline": ["rules"],
        "RuleSet:local": ["path", "format"],
        "RuleSet:remote": ["url", "format"],
        "Service:derp": ["config_path"],
        "Service:hysteria-realm": ["users"],
        "Service:ssm-api": ["servers"],
        "Service:usbip-client": ["server"],
        "Service:usbip-server": ["devices"],
        "TUICUser": ["uuid"],
        "VLESSUser": ["uuid"],
        "WireGuardPeer": ["public_key", "allowed_ips"],
    ]
}
