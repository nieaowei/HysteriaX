import Foundation

/// Check deletion against each merged platform, including reference-bearing overlays outside the current editor.
struct TemplateReferenceIndex {
    let format: String
    let schema: TemplateSchema
    static func effective(_ document: JSONValue, platform: String) -> JSONValue {
        if platform == "common" { return document["common"] }
        let overlay = document["platforms"][platform]
        var result = JSONValue.merging(document["common"], overlay["set"].objectValue == nil ? .object([:]) : overlay["set"])
        for path in overlay["remove"].arrayValue?.compactMap(\.stringValue) ?? [] {
            let keys = path.dropFirst().components(separatedBy: "/").map { $0.replacingOccurrences(of: "~1", with: "/").replacingOccurrences(of: "~0", with: "~") }
            result.set(keys, nil)
        }
        return result
    }
    func exists(_ name: String, kind: String, in config: JSONValue) -> Bool {
        if format == "mihomo" {
            if kind == "outbound", ["DIRECT", "REJECT", "REJECT-DROP", "PASS", "COMPATIBLE"].contains(name) { return true }
            if kind == "rule_set" { return config["rule-providers"].objectValue?[name] != nil }
            if kind == "sub_rule" { return config["sub-rules"].objectValue?[name] != nil }
        }
        let locations: [String: [String]] = ["outbound": [format == "mihomo" ? "proxy-groups" : "outbounds"], "inbound": ["inbounds"], "dns_server": ["dns", "servers"], "rule_set": ["route", "rule_set"], "http_client": ["http_clients"], "certificate_provider": ["certificate_providers"], "network_namespace": ["network_namespaces"]]
        guard let location = locations[kind] else { return false }
        return (config.at(location)?.arrayValue ?? []).contains { item in
            item[format == "mihomo" ? "name" : "tag"].stringValue == name || item["tag"].arrayValue?.contains(.string(name)) == true
        }
    }
    func blockingPaths(after document: JSONValue, before original: JSONValue, names: [String], kind: String) -> [String] {
        var result: Set<String> = []
        for platform in TemplatePlatform.all {
            let config = Self.effective(document, platform: platform)
            let oldConfig = Self.effective(original, platform: platform)
            func address(_ path: [String]) -> String {
                var current: [String] = []; var previous: [String] = []
                for part in path {
                    var mapped = part
                    if let index = Int(part), let items = config.at(current)?.arrayValue, items.indices.contains(index), let oldItems = oldConfig.at(previous)?.arrayValue {
                        let identity = items[index].objectValue?["name"] ?? items[index].objectValue?["tag"]
                        if let identity, let oldIndex = oldItems.firstIndex(where: { ($0.objectValue?["name"] ?? $0.objectValue?["tag"]) == identity }) { mapped = String(oldIndex) }
                    }
                    current.append(part); previous.append(mapped)
                }
                return platform + ":/" + previous.map { $0.replacingOccurrences(of: "~", with: "~0").replacingOccurrences(of: "/", with: "~1") }.joined(separator: "/")
            }
            for name in names where !exists(name, kind: kind, in: config) {
                func walk(_ value: JSONValue, definition: JSONValue, path: [String], depth: Int) {
                    guard depth < 70 else { return }
                    let resolved = schema.resolve(definition, value: value)
                    if resolved["x-tag-reference"].stringValue == kind {
                        let references = value.arrayValue?.compactMap(\.stringValue) ?? [value.stringValue].compactMap { $0 }
                        if references.contains(name) { result.insert(address(path)) }
                    }
                    if format == "mihomo", let native = value.stringValue,
                       (path.first == "rules" || path.first == "sub-rules") {
                        let rule = MihomoTemplateRule(native)
                        let direct = (kind == "outbound" && rule.kind != "SUB-RULE" && rule.target == name)
                            || (kind == "sub_rule" && rule.kind == "SUB-RULE" && rule.target == name)
                            || (kind == "rule_set" && rule.kind == "RULE-SET" && rule.payload == name)
                        let pattern = "(?:^|\\()RULE-SET\\s*,\\s*" + NSRegularExpression.escapedPattern(for: name) + "(?:,|\\)|$)"
                        if direct || (kind == "rule_set" && native.range(of: pattern, options: .regularExpression) != nil) { result.insert(address(path)) }
                    }
                    for (key, child) in value.objectValue ?? [:] {
                        let childDefinition = resolved["properties"].objectValue?[key] ?? resolved["additionalProperties"]
                        walk(child, definition: childDefinition, path: path + [key], depth: depth + 1)
                    }
                    for (index, child) in (value.arrayValue ?? []).enumerated() { walk(child, definition: resolved["items"], path: path + [String(index)], depth: depth + 1) }
                }
                walk(config, definition: schema.root, path: [], depth: 0)
                if kind == "outbound" { for (index, injection) in (document["injections"].arrayValue ?? []).enumerated() where injection["group"].stringValue == name { result.insert(platform + ":/injections/\(index)/group") } }
            }
        }
        return result.sorted()
    }
}
