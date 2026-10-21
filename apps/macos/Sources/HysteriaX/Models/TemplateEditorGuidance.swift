import Foundation

/// Presentation metadata audited against the configuration documentation shipped with sing-box 1.14.2.
/// Controls describe defaults without materializing them, and hide ignored fields without deleting them.
enum TemplateEditorGuidance {
    static func isSingbox(_ schema: TemplateSchema) -> Bool { schema.root["$defs"]["DNSServer"].objectValue != nil }
    static func context(_ path: [String], schema: TemplateSchema, value: JSONValue) -> (name: String, kind: String) {
        let raw = schema.definition(at: path, value: value)
        let name = raw["$ref"].stringValue?.components(separatedBy: "/").last ?? ""
        if let type = value.at(path)?["type"].stringValue { return (name, type) }
        // Named shared objects have no protocol discriminator. Avoid expanding
        // their whole schema repeatedly for each label, section and default.
        if directories[name] == nil, name != "CertificateProviderReference" { return (name, "") }
        let resolved = schema.resolve(raw, value: value.at(path) ?? .null)
        let type = resolved["properties"]["type"]["const"].stringValue
            ?? resolved["properties"]["type"]["enum"].arrayValue?.compactMap(\.stringValue).first ?? ""
        return (name, type)
    }
    static func documentURL(_ path: [String], schema: TemplateSchema, value: JSONValue) -> URL? {
        guard isSingbox(schema) else { return nil }
        for count in stride(from: path.count, through: 1, by: -1) {
            let currentPath = Array(path.prefix(count))
            let current = context(currentPath, schema: schema, value: value)
            guard var page = documentationPage(current, path: currentPath) else { continue }
            let relative = path.dropFirst(count).filter { Int($0) == nil }.joined(separator: ".")
            let prefix = documentationPrefixes[current.name] ?? ""
            var field = prefix + relative
            if relative.isEmpty { field = String(prefix.dropLast()) }
            if !relative.isEmpty, !relative.contains("."), prefix.isEmpty {
                let object = schema.resolve(schema.definition(at: currentPath, value: value), value: value.at(currentPath) ?? .null)
                let properties = object["properties"].objectValue ?? [:]
                if properties[relative] != nil {
                    if ["Rule", "NestedRule", "DNSRule", "NestedDNSRule"].contains(current.name) {
                        let action = current.name.contains("DNS") ? "DNSRuleAction" : "RuleAction"
                        if schema.resolve(schema.root["$defs"][action], value: value.at(currentPath) ?? .null)["properties"][relative] != .null {
                            page = current.name.contains("DNS") ? "dns/rule_action" : "route/rule_action"
                        }
                    } else if current.name == "DomainResolver" {
                        page = "dns/rule_action"
                    } else if !documentedFields(page).contains(field) {
                        // A protocol's own field wins over a shared name (notably TUN netns).
                        let sharedPages = properties["listen"] != nil
                            ? ["shared/udp-nat", "shared/listen", "shared/http2", "shared/quic"]
                            : ["shared/dial", "shared/udp-nat", "shared/http2", "shared/quic"]
                        if let shared = sharedPages.first(where: { documentedFields($0).contains(field) }) { page = shared }
                    }
                }
            }
            // TLS has duplicate inbound/outbound headings; keep its page link rather than
            // guessing a generated suffix that may select the opposite direction.
            let anchor = TemplateDocumentationOrder.anchors[page]?[field]
                ?? field.replacingOccurrences(of: ".", with: "").lowercased()
            let fragment = page != "shared/tls" && documentedFields(page).contains(field) && !anchor.isEmpty
                ? "#" + anchor : ""
            return URL(string: "https://sing-box.sagernet.org/zh/configuration/" + page + "/" + fragment)
        }
        return URL(string: "https://sing-box.sagernet.org/zh/configuration/")
    }
    private static let documentationFields = TemplateDocumentationOrder.pages.mapValues {
        Set($0.split(separator: " ").map(String.init))
    }
    private static func documentedFields(_ page: String) -> Set<String> { documentationFields[page] ?? [] }
    private static func documentationPage(_ current: (name: String, kind: String), path: [String]) -> String? {
        if path.count == 1, let page = rootPages[path[0]] { return page }
        if path.last == "udp_over_tcp", current.name.isEmpty { return "shared/udp-over-tcp" }
        if current.name == "CertificateProviderReference", ["acme", "cloudflare-origin-ca", "tailscale"].contains(current.kind) {
            return "shared/certificate-provider/" + current.kind
        }
        if let directory = directories[current.name] {
            let kind = current.kind.isEmpty ? "default" : current.kind
            return documentedKinds[current.name]?.contains(kind) == true
                ? directory + "/" + (kind == "h3" ? "http3" : kind) : directory
        }
        return pages[current.name]
    }
    static func isInlineObject(_ path: [String], schema: TemplateSchema) -> Bool {
        path.last == "default_domain_resolver" || (isSingbox(schema)
            && ["tls", "multiplex", "udp_over_tcp", "brutal", "utls", "ech", "reality", "http_proxy", "ssh_server", "port_mapping", "stats"].contains(path.last ?? ""))
    }
    static func automaticTLS(_ path: [String], schema: TemplateSchema, value: JSONValue) -> Bool {
        guard path.last == "tls" else { return false }
        let parent = context(Array(path.dropLast()), schema: schema, value: value)
        return ["HTTPClient", "HTTPClientReference"].contains(parent.name)
            || (parent.name == "DNSServer" && ["tls", "quic", "https", "h3"].contains(parent.kind))
    }
    static func visibleKeys(_ object: JSONValue, path: [String], schema: TemplateSchema, value: JSONValue) -> [String] {
        let properties = object["properties"].objectValue ?? [:]
        guard isSingbox(schema) else { return properties.keys.sorted() }
        let current = value.at(path) ?? .null
        if path == ["log"], current["disabled"].boolValue == true { return ["disabled"] }
        if path == ["ntp"], current["enabled"].boolValue != true { return ["enabled"] }
        let automaticTLS = automaticTLS(path, schema: schema, value: value)
        let gated = ["tls", "utls", "ech", "reality", "multiplex", "udp_over_tcp", "brutal", "http_proxy", "cache_file", "ssh_server", "port_mapping", "dashboard", "stats"].contains(path.last ?? "")
        if gated, !automaticTLS, properties["enabled"] != nil {
            let enabled = current["enabled"].boolValue
                ?? defaultValue(path + ["enabled"], schema: schema, value: value)?.boolValue ?? false
            if !enabled { return ["enabled"] }
        }
        let context = context(path, schema: schema, value: value)
        let dialer = properties["listen"] == nil ? Set(TemplateFieldLayout.dialerFields(in: Array(properties.keys)) + (properties["protect_path"] == nil ? [] : ["protect_path"])) : []
        let systemTLS = context.name == "OutboundTLSOptions" && ["apple", "windows"].contains(current["engine"].stringValue ?? "")
        let allowedTLS = systemTLS ? TemplateHTTPClientLayout.tlsAllowedKeys(object, value: .object(["tls": current])) : nil
        return properties.keys.filter { key in
            if automaticTLS, key == "enabled" { return false }
            if dialer.contains(key), key != "detour", !(current["detour"].stringValue ?? "").isEmpty { return false }
            if ["tcp_keep_alive", "tcp_keep_alive_interval"].contains(key), current["disable_tcp_keep_alive"].boolValue == true { return false }
            if key == "fallback_network_type", !dialer.isEmpty, current["network_strategy"].stringValue != "fallback" { return false }
            if key == "default_fallback_network_type", current["default_network_strategy"].stringValue != "fallback" { return false }
            if key == "default_interface", current["auto_detect_interface"].boolValue == true { return false }
            if key == "fragment_fallback_delay", current["fragment"].boolValue != true { return false }
            if key == "external_ui_download_url", (current["external_ui"].stringValue ?? "").isEmpty { return false }
            if let allowedTLS, key != "enabled", !allowedTLS.contains(key) { return false }
            return TemplateProtocolOptions.visible(key, name: context.name, kind: context.kind, value: current)
        }.sorted()
    }
    /// Surface retained values when a feature or mode makes their controls unavailable.
    /// Reading a form must never silently erase these values or hide their existence.
    static func hiddenConfiguredKeys(_ object: JSONValue, path: [String], schema: TemplateSchema, value: JSONValue) -> [String] {
        guard isSingbox(schema) else { return [] }
        let visible = Set(visibleKeys(object, path: path, schema: schema, value: value))
        let current = value.at(path) ?? .null
        return (object["properties"].objectValue ?? [:]).keys.filter {
            !visible.contains($0) && current.objectValue?[$0] != nil && TemplateHTTPClientLayout.hasValue(current[$0])
                && !(automaticTLS(path, schema: schema, value: value) && $0 == "enabled")
        }.sorted()
    }
    static func ordered(_ keys: [String], path: [String], schema: TemplateSchema, value: JSONValue) -> [String] {
        guard isSingbox(schema) else { return TemplateFieldLayout.orderedObjectFields(keys) }
        let current = context(path, schema: schema, value: value)
        let page = documentURL(path, schema: schema, value: value)?.path.replacingOccurrences(of: "/zh/configuration/", with: "") ?? ""
        let documented = (TemplateDocumentationOrder.pages[page] ?? "").split(separator: " ").map(String.init)
        let prefix = documentationPrefixes[current.name] ?? ""
        let fields = documented.filter { $0.hasPrefix(prefix) }.map { String($0.dropFirst(prefix.count)) }.filter { !$0.contains(".") }
        let fallback = (orders[current.name + ":" + current.kind] ?? orders[current.name] ?? "").split(separator: " ").map(String.init)
        let order = ["type", "tag", "enabled", "disabled"] + fields + fallback
        // User-defined dial fields retain their explicit, shared order across protocols.
        if Set(keys).isSubset(of: Set(TemplateFieldLayout.dialerFieldOrder + ["protect_path"])) {
            return TemplateFieldLayout.orderedObjectFields(keys)
        }
        let required = Set(keys.filter { schema.isRequired(path + [$0], value: value) })
        return keys.sorted {
            let identity = ["type", "tag", "enabled", "disabled"]
            if identity.contains($0) != identity.contains($1) { return identity.contains($0) }
            if required.contains($0) != required.contains($1) { return required.contains($0) }
            let lhs = order.firstIndex(of: $0) ?? order.count, rhs = order.firstIndex(of: $1) ?? order.count
            return lhs == rhs ? $0 < $1 : lhs < rhs
        }
    }
    static func section(_ key: String, path: [String], schema: TemplateSchema, value: JSONValue) -> String {
        guard isSingbox(schema) else { return TemplateFieldLayout.objectSection(key, path: path) }
        let current = context(path, schema: schema, value: value)
        if ["Rule", "DNSRule", "NestedRule", "NestedDNSRule", "HeadlessRule"].contains(current.name) {
            if ["type", "invert", "mode"].contains(key) { return "基本标识" }
            if key == "rules" { return "子规则" }
            let actionDefinition = schema.root["$defs"][current.name.contains("DNS") ? "DNSRuleAction" : "RuleAction"]
            let actionFields = schema.resolve(actionDefinition, value: value.at(path) ?? .null)["properties"].objectValue ?? [:]
            if current.name != "HeadlessRule", actionFields[key] != nil { return "规则动作" }
            if ["action", "outbound", "server", "sniffer", "timeout", "strategy", "disable_cache", "rewrite_ttl", "client_subnet", "rcode", "answer", "ns", "extra"].contains(key) { return "规则动作" }
            if key.hasPrefix("domain") || ["geosite", "rule_set"].contains(key) { return "域名与规则集匹配" }
            if key.contains("ip_") || key.contains("geoip") { return "IP 匹配" }
            if key.contains("port") { return "端口匹配" }
            if key.hasPrefix("process") || ["package_name", "user", "user_id"].contains(key) { return "进程与用户匹配" }
            return "匹配条件"
        }
        if let section = TemplateProtocolOptions.section(key, name: current.name, kind: current.kind) { return section }
        if current.name == "RuleSet" {
            if ["type", "tag", "format"].contains(key) { return "基本标识" }
            if key == "rules" { return "规则内容" }
            if ["path", "url"].contains(key) { return "规则集来源" }
            return "下载设置"
        }
        if ["InboundTLSOptions", "OutboundTLSOptions"].contains(current.name) {
            if ["enabled", "engine", "server_name", "disable_sni", "insecure", "min_version", "max_version"].contains(key) { return "基本标识" }
            if key == "ech" { return "ECH" }
            if key == "reality" { return "Reality" }
            if key == "utls" { return "uTLS" }
            if key == "acme" { return "ACME" }
            if key.contains("certificate") || key == "key" || key == "key_path" { return "证书与身份验证" }
            return "TLS 参数"
        }
        if current.name == "Endpoint", current.kind == "wireguard" {
            if key == "peers" { return "WireGuard 对端" }
            if ["system", "name", "mtu", "address", "listen_port"].contains(key) { return "接口设置" }
        }
        if ["Inbound", "Service"].contains(current.name), ["listen", "listen_port"].contains(key) { return "监听设置" }
        return TemplateFieldLayout.objectSection(key, path: path)
    }
    static let sectionOrder = [
        "基本标识", "接口设置", "监听设置", "远端地址", "认证信息",
        "TUN 接口", "TUN DNS", "路由接管", "自动重定向", "流量范围", "系统代理",
        "登录与控制服务器", "节点身份", "子网与出口节点", "中继与监听", "系统接口", "状态与文件", "SSH 服务",
        "加密与密钥", "数据通道", "保活与重协商", "路由与推送", "客户端身份", "主机检查", "会话与重连",
        "证书签发", "签发账户", "域名验证", "证书与身份验证",
        "规则集来源", "下载设置", "规则内容", "子规则", "域名与规则集匹配", "IP 匹配", "端口匹配", "进程与用户匹配", "匹配条件", "规则动作",
        "协议选项", "UDP NAT", "TLS 设置", "TLS 参数", "TLS 与传输", "ECH", "Reality", "uTLS", "ACME",
        "传输层", "多路复用", "UDP over TCP", "TCP Brutal", "HTTP 客户端", "请求标头", "HTTP/2 参数", "连接与流控制", "QUIC 参数", "WireGuard 对端", "网络连接"
    ]
    static func defaultValue(_ path: [String], schema: TemplateSchema, value: JSONValue) -> JSONValue? {
        guard isSingbox(schema), let key = path.last else { return nil }
        let parentPath = Array(path.dropLast())
        let field = schema.definition(at: path, value: value)
        guard field != .null else { return nil }
        let parent = context(parentPath, schema: schema, value: value)
        let object = value.at(parentPath) ?? .null
        if key == "enabled", ["InboundTLSOptions", "OutboundTLSOptions"].contains(parent.name) {
            return .bool(automaticTLS(parentPath, schema: schema, value: value))
        }
        if key == "enabled", ["InboundECHOptions", "OutboundECHOptions", "InboundRealityOptions", "OutboundRealityOptions", "OutboundUTLSOptions"].contains(parent.name) {
            return .bool(false)
        }
        if path == ["certificate", "store"] { return .string("system") }
        if path.dropLast().last == "udp_over_tcp", key == "version" { return .integer(2) }
        if parent.name == "DNS", key == "timeout" { return .string("10s") }
        if ["DomainResolver", "DNSRule", "NestedDNSRule", "DNSRuleAction"].contains(parent.name), key == "timeout" {
            return .string(value["dns"]["timeout"].stringValue ?? "10s")
        }
        if ["Rule", "NestedRule", "RuleAction"].contains(parent.name), object["action"].stringValue == "sniff", key == "timeout" { return .string("300ms") }
        if ["DNSRule", "NestedDNSRule", "DNSRuleAction"].contains(parent.name), object["action"].stringValue == "predefined", key == "rcode" { return .integer(0) }
        if ["Rule", "NestedRule", "RuleAction", "DNSRule", "NestedDNSRule", "DNSRuleAction"].contains(parent.name), object["action"].stringValue == "reject", key == "method" { return .string("default") }
        if parent.name == "RuleSet", parent.kind == "remote", key == "update_interval" { return .string("1d") }
        if parent.name == "CacheFileOptions", key == "path" { return .string("cache.db") }
        if let result = TemplateProtocolOptions.defaultValue(key, name: parent.name, kind: parent.kind, object: object) { return result }
        if key == "udp_timeout", ["Inbound", "Endpoint", "Service"].contains(parent.name) {
            return field["$ref"].stringValue == "#/$defs/UDPTimeout" ? .integer(300) : .string("5m")
        }
        if parent.name == "OpenVPNRemoteOptions", key == "network" {
            return .string(value.at(Array(parentPath.dropLast(2)))?["network"].stringValue ?? "udp")
        }
        if parent.name == "OpenVPNPushOptions", key == "redirect_gateway_flags" { return .array([.string("def1")]) }
        if parentPath.last == "dashboard" || parentPath.last == "stun" || parentPath.last == "obfs" {
            let owner = context(Array(parentPath.dropLast()), schema: schema, value: value)
            if owner.name == "Service", owner.kind == "api", parentPath.last == "dashboard" {
                return ["path": .string("dashboard"), "download_url": .string("https://github.com/SagerNet/sing-box-dashboard/archive/refs/heads/gh-pages.zip"), "update_interval": .string("1d")][key]
            }
            if owner.name == "Service", owner.kind == "derp", parentPath.last == "stun" {
                return ["listen": .string("::"), "listen_port": .integer(3478)][key]
            }
            if ["Inbound", "Outbound"].contains(owner.name), owner.kind == "hysteria2", object["type"].stringValue == "gecko", parentPath.last == "obfs" {
                return ["min_packet_size": .integer(512), "max_packet_size": .integer(1200)][key]
            }
        }
        if parent.name == "DNSServer" {
            if key == "server_port" {
                switch parent.kind {
                case "tcp", "udp": return .integer(53)
                case "tls", "quic": return .integer(853)
                case "https", "h3": return .integer(443)
                default: break
                }
            }
            if key == "path", ["https", "h3"].contains(parent.kind) { return .string("/dns-query") }
        }
        if parent.name == "NTPOptions" {
            if key == "server_port" { return .integer(123) }
            if key == "interval" { return .string("30m") }
        }
        if parent.name == "Outbound", parent.kind == "urltest" {
            return ["url": .string("https://www.gstatic.com/generate_204"), "interval": .string("3m"), "tolerance": .integer(50), "idle_timeout": .string("30m")][key]
        }
        if parent.name == "Outbound", parent.kind == "ssh", key == "server_port" { return .integer(22) }
        if parent.name == "Endpoint", parent.kind == "wireguard", key == "mtu" { return .integer(1408) }
        if ["InboundMultiplexOptions", "OutboundMultiplexOptions"].contains(parent.name), key == "protocol" { return .string("h2mux") }
        guard ["tcp_keep_alive", "tcp_keep_alive_interval", "fallback_delay"].contains(key) else { return nil }
        let definition = schema.resolve(schema.definition(at: Array(path.dropLast()), value: value), value: value.at(Array(path.dropLast())) ?? .null)
        if !(parent.name == "Endpoint" && parent.kind == "openconnect"),
           !TemplateFieldLayout.dialerFields(in: Array((definition["properties"].objectValue ?? [:]).keys)).isEmpty {
            return ["tcp_keep_alive": .string("5m"), "tcp_keep_alive_interval": .string("75s"), "fallback_delay": .string("300ms")][key]
        }
        return nil
    }
    static func prompt(_ definition: JSONValue, path: [String]) -> String? {
        if definition["$ref"].stringValue == "#/$defs/Duration" { return "10s" }
        switch path.last {
        case "inet4_bind_address": return "0.0.0.0"
        case "inet6_bind_address": return "::"
        case "url", "server_url": return "https://example.com"
        case "server": return "example.com"
        case "certificate_path", "key_path", "private_key_path": return "/path/to/file"
        default: return nil
        }
    }
    static func enumPickerOptions(_ field: JSONValue, path: [String], schema: TemplateSchema, value: JSONValue) -> TemplateEnumPickerOptions {
        let singbox = isSingbox(schema)
        let fallback = defaultValue(path, schema: schema, value: value) ?? field.objectValue?["default"]
        // Only these audited enums use the empty string as an alias for their
        // documented nonempty default. Other explicit empty values stay visible.
        return TemplateEnumPickerOptions(values: field["enum"].arrayValue ?? field["x-enum-choices"].arrayValue ?? [],
            current: value.at(path), defaultValue: fallback,
            supportsUnset: singbox && Int(path.last ?? "") == nil,
            emptyUsesDefault: singbox && ["method", "udp_mapping", "udp_filtering", "wildcard_sni"].contains(path.last ?? ""))
    }
    static func enumPickerTitle(_ choice: JSONValue?, path: [String], schema: TemplateSchema) -> String {
        let networkStrategy = isSingbox(schema) && ["network_strategy", "default_network_strategy"].contains(path.last ?? "")
        guard let choice else {
            return networkStrategy ? L10n.text("未设置（继承默认）") : L10n.text("未设置（由内核决定）")
        }
        if choice == .string("") { return L10n.text("空字符串") }
        if networkStrategy && choice == .string("default") { return L10n.text("按顺序连接（default）") }
        return TemplateSchema.pickerTitle(choice)
    }
    static func help(_ path: [String], schema: TemplateSchema, value: JSONValue) -> String? {
        guard isSingbox(schema), let key = path.last else { return nil }
        let current = context(Array(path.dropLast()), schema: schema, value: value)
        if key == "enabled", ["InboundTLSOptions", "OutboundTLSOptions"].contains(current.name) {
            if automaticTLS(Array(path.dropLast()), schema: schema, value: value) {
                return L10n.text("此位置的 TLS 由协议自动启用，enabled 不控制启用状态。")
            }
            return L10n.text("未设置 enabled 时不启用 TLS。关闭只隐藏参数，保留已有配置。")
        }
        if let explanation = TemplateDefaultCatalog.explanation(key, name: current.name, kind: current.kind) {
            return L10n.text(explanation)
        }
        if key == "detour" {
            let parentPath = Array(path.dropLast())
            let parent = schema.resolve(schema.definition(at: parentPath, value: value), value: value.at(parentPath) ?? .null)
            if parent["properties"]["listen"].objectValue != nil {
                return L10n.text("将连接交给指定入站；其他监听参数仍然有效。")
            }
        }
        if let text = hints[key] {
            let parentPath = Array(path.dropLast())
            let parent = context(parentPath, schema: schema, value: value)
            let scopedHint: Bool
            switch key {
            case "max_streams", "max_connections", "min_streams": scopedHint = parent.name == "OutboundMultiplexOptions"
            case "format": scopedHint = parent.name == "RuleSet"
            case "detour", "domain_resolver", "connect_timeout", "disable_tcp_keep_alive", "fallback_network_type", "network_strategy":
                let definition = schema.resolve(schema.definition(at: parentPath, value: value), value: value.at(parentPath) ?? .null)
                let properties = definition["properties"].objectValue ?? [:]
                scopedHint = properties[key] != nil && (key == "disable_tcp_keep_alive" || (properties["listen"] == nil
                    && !["Rule", "NestedRule", "DNSRule", "NestedDNSRule", "HeadlessRule"].contains(parent.name)))
            default: scopedHint = true
            }
            if scopedHint { return L10n.text(text) }
        }
        if let defaultValue = defaultValue(path, schema: schema, value: value) {
            return L10n.text("默认：{0}", defaultValue.stringValue ?? defaultValue.pretty)
        }
        return nil
    }
    static func summary(_ value: JSONValue, path: [String]) -> String? {
        if path.last == "rules" {
            if value["type"].stringValue == "logical" {
                return [value["mode"].stringValue ?? "and", L10n.text("共 {0} 项", String(value["rules"].arrayValue?.count ?? 0)), value["action"].stringValue, value["outbound"].stringValue ?? value["server"].stringValue].compactMap { $0 }.joined(separator: " · ")
            }
            let keys = ["domain", "domain_suffix", "domain_keyword", "rule_set", "ip_cidr", "source_ip_cidr", "port", "source_port", "process_name", "protocol", "network"]
            let conditions = keys.compactMap { key -> String? in
                guard let field = value.objectValue?[key] else { return nil }
                let values = field.arrayValue ?? [field]
                let text = values.prefix(2).map { $0.stringValue ?? $0.pretty }.joined(separator: ", ")
                return key + ": " + text + (values.count > 2 ? " …" : "")
            }
            let target = value["outbound"].stringValue ?? value["server"].stringValue
            let action = value["action"].stringValue ?? "route"
            return (Array(conditions.prefix(2)) + [action + (target.map { " → " + $0 } ?? "")]).joined(separator: " · ")
        }
        if path == ["certificate_providers"] {
            let domain = value["domain"].arrayValue?.prefix(2).compactMap(\.stringValue).joined(separator: ", ") ?? value["domain"].stringValue
            return [value["type"].stringValue, domain, value["endpoint"].stringValue].compactMap { $0 }.joined(separator: " · ")
        }
        if path == ["network_namespaces"] { return [value["type"].stringValue ?? "default", value["path"].stringValue].compactMap { $0 }.joined(separator: " · ") }
        if path == ["endpoints"], value["type"].stringValue == "wireguard" {
            let addresses = value["address"].arrayValue?.prefix(2).compactMap(\.stringValue).joined(separator: ", ") ?? value["address"].stringValue
            return ["wireguard", addresses, "peers: " + String(value["peers"].arrayValue?.count ?? 0)].compactMap { $0 }.joined(separator: " · ")
        }
        if path == ["outbounds"], ["selector", "urltest"].contains(value["type"].stringValue ?? "") {
            return [value["type"].stringValue, "outbounds: " + String(value["outbounds"].arrayValue?.count ?? 0), value["default"].stringValue].compactMap { $0 }.joined(separator: " · ")
        }
        return nil
    }
    private static let hints = [
        "detour": "选择上游出站后，其他拨号参数会被忽略。",
        "domain_resolver": "用于解析连接目标的域名；也可由路由的默认域名解析器提供。",
        "connect_timeout": "连接超时时间，例如 300ms 或 10s。",
        "disable_tcp_keep_alive": "启用后不使用 TCP 保活时间和保活间隔。",
        "fallback_network_type": "仅在 fallback 网络策略下使用。",
        "network_strategy": "网络选择策略与显式绑定的网络接口或地址互斥。",
        "max_streams": "与 max_connections、min_streams 互斥。",
        "max_connections": "与 max_streams 互斥。",
        "min_streams": "与 max_streams 互斥。",
        "format": "规则集路径或 URL 以 .json、.srs 结尾时，可自动推断格式。",
        "certificate_provider": "引用共享证书提供者，或配置独立的证书提供者。"
    ]
    private static let directories = ["DNSServer": "dns/server", "Inbound": "inbound", "Outbound": "outbound", "Endpoint": "endpoint", "Service": "service", "CertificateProvider": "shared/certificate-provider", "NetworkNamespace": "network-namespace"]
    private static let documentedKinds: [String: Set<String>] = [
        "DNSServer": ["dhcp", "fakeip", "h3", "hosts", "https", "local", "mdns", "openconnect", "openvpn", "quic", "resolved", "tailscale", "tcp", "tls", "udp"],
        "Inbound": ["anytls", "cloudflared", "direct", "http", "hysteria", "hysteria2", "mixed", "naive", "redirect", "shadowsocks", "shadowtls", "snell", "socks", "tproxy", "trojan", "tuic", "tun", "vless", "vmess"],
        "Outbound": ["anytls", "block", "bridge", "direct", "http", "hysteria", "hysteria2", "naive", "selector", "shadowsocks", "shadowtls", "snell", "socks", "ssh", "tor", "trojan", "tuic", "urltest", "vless", "vmess"],
        "Endpoint": ["openconnect", "openvpn-client", "openvpn-server", "tailscale", "wireguard"],
        "Service": ["api", "ccm", "derp", "hysteria-realm", "ocm", "resolved", "ssm-api", "usbip-client", "usbip-server"],
        "CertificateProvider": ["acme", "cloudflare-origin-ca", "tailscale"], "NetworkNamespace": ["default", "unshare"]
    ]
    private static let rootPages = ["$schema": "schema", "log": "log", "dns": "dns", "ntp": "ntp", "certificate": "certificate", "certificate_providers": "shared/certificate-provider", "http_clients": "shared/http-client", "network_namespaces": "network-namespace", "endpoints": "endpoint", "inbounds": "inbound", "outbounds": "outbound", "route": "route", "services": "service", "experimental": "experimental"]
    private static let pages = [
        "DNS": "dns", "DNSRule": "dns/rule", "NestedDNSRule": "dns/rule", "DNSRuleAction": "dns/rule_action", "DomainResolver": "shared/dial",
        "RouteOptions": "route", "Rule": "route/rule", "NestedRule": "route/rule", "RuleAction": "route/rule_action", "RuleSet": "rule-set", "HeadlessRule": "rule-set/headless-rule",
        "LogOptions": "log", "NTPOptions": "ntp", "HTTPClient": "shared/http-client", "HTTPClientReference": "shared/http-client",
        "InboundTLSOptions": "shared/tls", "OutboundTLSOptions": "shared/tls", "InboundRealityOptions": "shared/tls", "OutboundRealityOptions": "shared/tls", "InboundRealityHandshakeOptions": "shared/tls", "InboundECHOptions": "shared/tls", "OutboundECHOptions": "shared/tls", "OutboundUTLSOptions": "shared/tls",
        "DialerOptions": "shared/dial", "HTTPProxyOptions": "inbound/tun", "TunPlatformOptions": "inbound/tun",
        "OpenConnectMobileOptions": "endpoint/openconnect", "OpenConnectCSDOptions": "endpoint/openconnect", "OpenConnectHIPOptions": "endpoint/openconnect", "OpenConnectTNCCOptions": "endpoint/openconnect", "OpenConnectTNCCCertificateOptions": "endpoint/openconnect", "OpenConnectFortinetHostCheckOptions": "endpoint/openconnect", "OpenConnectFormEntryOptions": "endpoint/openconnect",
        "OpenVPNControlWrapOptions": "endpoint/openvpn-client", "OpenVPNPushOptions": "endpoint/openvpn-server", "OpenVPNPushDNSServerOptions": "endpoint/openvpn-server", "DERPMeshOptions": "service/derp", "Hysteria2Realm": "outbound/hysteria2", "Hysteria2InboundRealm": "inbound/hysteria2", "Hysteria2RealmPortMapping": "outbound/hysteria2", "USBIPDeviceMatch": "service/usbip-client", "CCMUser": "service/ccm", "OCMUser": "service/ocm",
        "InboundMultiplexOptions": "shared/multiplex", "OutboundMultiplexOptions": "shared/multiplex", "BrutalOptions": "shared/tcp-brutal", "V2RayTransport": "shared/v2ray-transport",
        "WireGuardPeer": "endpoint/wireguard", "OpenVPNRemoteOptions": "endpoint/openvpn-client", "OpenVPNPullFilterOptions": "endpoint/openvpn-client", "OpenVPNOutboundTLSOptions": "endpoint/openvpn-client", "OpenVPNInboundTLSOptions": "endpoint/openvpn-server", "OpenVPNInboundControlWrapOptions": "endpoint/openvpn-server", "OpenConnectTLSOptions": "endpoint/openconnect", "OpenConnectTokenOptions": "endpoint/openconnect",
        "CacheFileOptions": "experimental/cache-file", "ClashAPIOptions": "experimental/clash-api", "V2RayAPIOptions": "experimental/v2ray-api", "V2RayStatsServiceOptions": "experimental/v2ray-api", "ExperimentalOptions": "experimental",
        "ACMEExternalAccountOptions": "shared/certificate-provider/acme", "ACMEProviderDNS01Challenge": "shared/dns01_challenge", "CertificateProviderReference": "shared/certificate-provider",
        "TUICUser": "inbound/tuic", "VLESSUser": "inbound/vless", "VMessUser": "inbound/vmess", "AnyTLSUser": "inbound/anytls", "ShadowsocksUser": "inbound/shadowsocks", "ShadowTLSUser": "inbound/shadowtls", "TrojanUser": "inbound/trojan", "SnellUser": "inbound/snell", "HysteriaUser": "inbound/hysteria", "Hysteria2User": "inbound/hysteria2", "HysteriaRealmUser": "service/hysteria-realm"
    ]
    private static let documentationPrefixes = [
        "WireGuardPeer": "peers.", "OpenVPNRemoteOptions": "servers.", "OpenVPNPullFilterOptions": "pull_filters.",
        "OpenVPNOutboundTLSOptions": "tls.", "OpenVPNInboundTLSOptions": "tls.", "OpenVPNControlWrapOptions": "tls.control_wrap.", "OpenVPNInboundControlWrapOptions": "tls.control_wrap.", "OpenVPNPushOptions": "push.", "OpenVPNPushDNSServerOptions": "push.dns_servers.",
        "OpenConnectTLSOptions": "tls.", "OpenConnectTokenOptions": "token.", "OpenConnectMobileOptions": "mobile.", "OpenConnectCSDOptions": "csd.", "OpenConnectHIPOptions": "hip.", "OpenConnectTNCCOptions": "tncc.", "OpenConnectTNCCCertificateOptions": "tncc.certificates.", "OpenConnectFortinetHostCheckOptions": "fortinet_host_check.", "OpenConnectFormEntryOptions": "form_entries.",
        "Hysteria2Realm": "realm.", "Hysteria2InboundRealm": "realm.", "Hysteria2RealmPortMapping": "realm.port_mapping.", "HTTPProxyOptions": "platform.http_proxy.", "TunPlatformOptions": "platform.", "V2RayStatsServiceOptions": "stats.", "ACMEExternalAccountOptions": "external_account."
    ]
    private static let orders = [
        "DNSServer": "type tag server server_port path headers tls",
        "Inbound": "type tag listen listen_port users tls transport multiplex",
        "Outbound": "type tag server server_port users username password uuid method tls transport multiplex",
        "Outbound:selector": "type tag outbounds default interrupt_exist_connections",
        "Outbound:urltest": "type tag outbounds url interval tolerance idle_timeout interrupt_exist_connections",
        "Endpoint:wireguard": "type tag system name mtu address private_key listen_port peers udp_timeout workers",
        "WireGuardPeer": "address port public_key pre_shared_key allowed_ips persistent_keepalive_interval",
        "RuleSet": "type tag format rules path url initial_path http_client update_interval download_detour",
        "InboundTLSOptions": "enabled engine server_name alpn min_version max_version certificate certificate_path key key_path certificate_provider client_authentication client_certificate client_certificate_path kernel_tx kernel_rx handshake_timeout ech reality acme",
        "OutboundTLSOptions": "enabled engine disable_sni server_name insecure alpn min_version max_version cipher_suites curve_preferences certificate certificate_path certificate_public_key_sha256 client_certificate client_certificate_path client_key client_key_path kernel_tx kernel_rx handshake_timeout utls ech reality fragment fragment_fallback_delay record_fragment",
        "InboundRealityOptions": "enabled handshake private_key short_id max_time_difference",
        "OutboundRealityOptions": "enabled public_key short_id",
        "OutboundUTLSOptions": "enabled fingerprint",
        "InboundMultiplexOptions": "enabled padding brutal",
        "OutboundMultiplexOptions": "enabled protocol max_connections min_streams max_streams padding brutal",
        "ClashAPIOptions": "external_controller external_ui external_ui_download_url external_ui_download_detour secret default_mode access_control_allow_origin access_control_allow_private_network",
        "CacheFileOptions": "enabled path cache_id store_fakeip store_rdrc rdrc_timeout",
        "NTPOptions": "enabled server server_port interval",
        "LogOptions": "disabled level output timestamp"
    ]
}
