import Foundation

/// Check deletion against each merged platform, including reference-bearing overlays outside the current editor.
struct TemplateReferenceIndex {
    let format: String
    let schema: TemplateSchema
    struct Identity: Hashable {
        let kind: String
        let name: String
    }
    struct Reference {
        let identity: Identity
        let path: [String]
    }
    var collections: [String: [String]] {
        if format == "mihomo" { return ["outbound": ["proxy-groups"], "rule_set": ["rule-providers"], "sub_rule": ["sub-rules"]] }
        return ["outbound": ["outbounds"], "inbound": ["inbounds"], "dns_server": ["dns", "servers"], "rule_set": ["route", "rule_set"],
                "http_client": ["http_clients"], "certificate_provider": ["certificate_providers"], "network_namespace": ["network_namespaces"]]
    }
    var collectionPaths: [[String]] { Array(collections.values) + (format == "singbox" ? [["endpoints"]] : []) }
    func identities(at path: [String], in config: JSONValue) -> [Identity] {
        let endpoint = format == "singbox" && path.count == 2 && path.first == "endpoints"
        guard let kind = endpoint ? "outbound" : collections.first(where: { path.count == $0.value.count + 1 && path.starts(with: $0.value) })?.key,
              let value = config.at(path) else { return [] }
        if format == "mihomo", ["rule_set", "sub_rule"].contains(kind) { return [Identity(kind: kind, name: path.last!)] }
        let tag = value[format == "mihomo" && kind == "outbound" ? "name" : "tag"]
        return (tag.arrayValue?.compactMap(\.stringValue) ?? [tag.stringValue].compactMap { $0 }).filter { !$0.isEmpty }.map { Identity(kind: kind, name: $0) }
    }
    func references(in config: JSONValue) -> [Reference] {
        var result: [Reference] = []
        func walk(_ value: JSONValue, definition: JSONValue, path: [String], depth: Int) {
            guard depth < 70 else { return }
            let resolved = schema.resolve(definition, value: value)
            // resolve() provides a fallback editor for unknown variants. That
            // fallback must not give unknown protocol/action fields new meaning.
            for key in ["type", "action", "provider", "protocol", "version"] {
                if let actual = value.objectValue?[key], let expected = resolved["properties"][key].objectValue?["const"], actual != expected {
                    if key == "action", actual == .string(""), expected == .string("route") { continue }
                    return
                }
            }
            if let kind = TemplateSchema.referenceKind(resolved, path: path) {
                if let name = value.stringValue, !name.isEmpty { result.append(Reference(identity: Identity(kind: kind, name: name), path: path)) }
                if let values = value.arrayValue {
                    for (index, item) in values.enumerated() {
                        if let name = item.stringValue, !name.isEmpty { result.append(Reference(identity: Identity(kind: kind, name: name), path: path + [String(index)])) }
                    }
                    return
                }
            }
            if format == "mihomo", let native = value.stringValue,
               (path.count == 2 && path.first == "rules" || path.count == 3 && path.first == "sub-rules") {
                let rule = MihomoTemplateRule(native)
                for kind in ["outbound", "sub_rule", "rule_set"] {
                    result += rule.referenceNames(kind).map { Reference(identity: Identity(kind: kind, name: $0), path: path) }
                }
            }
            for (key, child) in value.objectValue ?? [:] {
                walk(child, definition: resolved["properties"].objectValue?[key] ?? resolved["additionalProperties"], path: path + [key], depth: depth + 1)
            }
            for (index, child) in (value.arrayValue ?? []).enumerated() { walk(child, definition: resolved["items"], path: path + [String(index)], depth: depth + 1) }
        }
        walk(config, definition: schema.root, path: [], depth: 0)
        return result
    }
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
        if format == "singbox", kind == "outbound", (config["endpoints"].arrayValue ?? []).contains(where: { $0["tag"] == .string(name) }) { return true }
        if format == "mihomo" {
            if kind == "outbound", ["DIRECT", "REJECT", "REJECT-DROP", "PASS", "COMPATIBLE"].contains(name) { return true }
            if kind == "rule_set" { return config["rule-providers"].objectValue?[name] != nil }
            if kind == "sub_rule" { return config["sub-rules"].objectValue?[name] != nil }
        }
        guard let location = collections[kind] else { return false }
        return (config.at(location)?.arrayValue ?? []).contains { item in
            item[format == "mihomo" ? "name" : "tag"].stringValue == name || item["tag"].arrayValue?.contains(.string(name)) == true
        }
    }
    /// Write only changed fields; inherited arrays still become whole-array
    /// overrides, while unrelated platform inheritance remains untouched.
    static func applying(_ updated: JSONValue, to document: JSONValue, layer: String) -> JSONValue {
        var result = document
        let before = effective(document, platform: layer)
        func write(_ path: [String], _ value: JSONValue?) {
            if layer == "common" { result.set(["common"] + path, value); return }
            result.set(["platforms", layer, "set"] + path, value)
            let pointer = SubscriptionTemplateStore.pointer(path)
            var removals = result["platforms"][layer]["remove"].arrayValue?.compactMap(\.stringValue) ?? []
            removals.removeAll { $0 == pointer || $0.hasPrefix(pointer + "/") || pointer.hasPrefix($0 + "/") }
            if value == nil { removals.append(pointer) }
            result.set(["platforms", layer, "remove"], .array(removals.map(JSONValue.string)))
        }
        func diff(_ old: JSONValue?, _ new: JSONValue?, _ path: [String]) {
            guard old != new else { return }
            if let a = old?.objectValue, let b = new?.objectValue {
                for key in Set(a.keys).union(b.keys).sorted() { diff(a[key], b[key], path + [key]) }
            } else { write(path, new) }
        }
        diff(before, updated, [])
        return result
    }
    func renamed(_ config: JSONValue, identity: Identity, new: String) -> JSONValue {
        var result = config
        for reference in references(in: config) where reference.identity == identity {
            if format == "mihomo", let native = result.at(reference.path)?.stringValue,
               reference.path.first == "rules" || reference.path.first == "sub-rules" {
                result.set(reference.path, .string(MihomoTemplateRule(native).renamingReference(identity.kind, old: identity.name, new: new).source))
            } else { result.set(reference.path, .string(new)) }
        }
        if let collection = collections[identity.kind] {
            if format == "mihomo", ["rule_set", "sub_rule"].contains(identity.kind), let entry = result.at(collection + [identity.name]) {
                result.set(collection + [identity.name], nil); result.set(collection + [new], entry)
            } else {
                let locations = [collection] + (format == "singbox" && identity.kind == "outbound" ? [["endpoints"]] : [])
                for location in locations {
                for (index, _) in (config.at(location)?.arrayValue ?? []).enumerated() {
                    let path = location + [String(index)]
                    guard identities(at: path, in: config).contains(identity) else { continue }
                    let key = format == "mihomo" && identity.kind == "outbound" ? "name" : "tag"
                    let tag = config.at(path + [key]) ?? .null
                    result.set(path + [key], tag.arrayValue.map { .array($0.map { $0 == .string(identity.name) ? .string(new) : $0 }) } ?? .string(new))
                }
                }
            }
        }
        return result
    }
    static func originalPath(_ path: [String], current config: JSONValue, original: JSONValue) -> [String] {
        var current: [String] = [], previous: [String] = []
        for part in path {
            var mapped = part
            if let index = Int(part), let items = config.at(current)?.arrayValue, items.indices.contains(index), let oldItems = original.at(previous)?.arrayValue {
                let item = items[index]
                let namedCollections = [["proxy-groups"], ["outbounds"], ["inbounds"], ["endpoints"], ["dns", "servers"], ["route", "rule_set"], ["http_clients"], ["certificate_providers"], ["network_namespaces"]]
                let identity = namedCollections.contains(current) ? item.objectValue?["name"] ?? item.objectValue?["tag"] : nil
                let oldIndex = identity.flatMap { name in oldItems.firstIndex { ($0.objectValue?["name"] ?? $0.objectValue?["tag"]) == name } }
                    ?? (oldItems.indices.contains(index) && oldItems[index] == item ? index : oldItems.firstIndex(of: item))
                if let oldIndex { mapped = String(oldIndex) }
            }
            current.append(part); previous.append(mapped)
        }
        return previous
    }
    struct CascadeResult {
        let config: JSONValue
        let removed: Set<Identity>
        let paths: [[String]]
    }
    func cascading(_ original: JSONValue, removing seeds: Set<Identity>, injectedGroups: Set<String>) -> CascadeResult {
        var config = original
        var removed = seeds
        var paths: [[String]] = []
        var affectedGroups: Set<String> = []
        while true {
            let dead = removed.filter { !exists($0.name, kind: $0.kind, in: config) }
            var units: [[String]] = []
            for reference in references(in: config) where dead.contains(reference.identity) {
                let path = reference.path
                if path.starts(with: ["route", "rules"]) || path.starts(with: ["dns", "rules"]) {
                    units.append(Array(path.prefix(3)))
                } else if path.first == "rules" { units.append(Array(path.prefix(2))) }
                else if path.first == "sub-rules" { units.append(Array(path.prefix(3))) }
                else if let collection = collectionPaths.first(where: { path.starts(with: $0) && path.count > $0.count + 1 }) {
                    let owner = Array(path.prefix(collection.count + 1))
                    let field = path.dropFirst(owner.count).first
                    if ["outbounds", "proxies"].contains(field), reference.identity.kind == "outbound" {
                        if let name = config.at(owner)?[format == "mihomo" ? "name" : "tag"].stringValue { affectedGroups.insert(name) }
                        units.append(path)
                    } else if field == "default", reference.identity.kind == "outbound" { units.append(path) }
                    else { units.append(owner) }
                } else if path.count >= 3 && ["domain_resolver", "default_domain_resolver"].contains(path[1]) {
                    units.append(Array(path.prefix(2)))
                } else { units.append(path) }
            }
            // Removing the last static member must not leave an invalid group.
            let groupsPath = collections["outbound"]!
            for (index, group) in (config.at(groupsPath)?.arrayValue ?? []).enumerated() {
                let key = format == "mihomo" ? "proxies" : "outbounds"
                guard group[key].arrayValue?.isEmpty == true,
                      removed.contains(where: { $0.kind == "outbound" }),
                      !injectedGroups.contains(group[format == "mihomo" ? "name" : "tag"].stringValue ?? ""),
                      affectedGroups.contains(group[format == "mihomo" ? "name" : "tag"].stringValue ?? "") else { continue }
                units.append(groupsPath + [String(index)])
            }
            for (key, rules) in config["sub-rules"].objectValue ?? [:] where rules.arrayValue?.isEmpty == true && paths.contains(where: { $0.starts(with: ["sub-rules", key]) }) {
                units.append(["sub-rules", key])
            }
            let unique = Set(units.map(SubscriptionTemplateStore.pointer)).map(SubscriptionTemplateStore.path)
            let minimal = unique.filter { path in !unique.contains(where: { $0.count < path.count && path.starts(with: $0) }) }
            guard !minimal.isEmpty else { break }
            // Sibling indices are removed backwards so the remaining rows keep identity.
            let sorted = minimal.sorted { a, b in
                for (left, right) in zip(a, b) where left != right {
                    if let l = Int(left), let r = Int(right) { return l > r }
                    return left > right
                }
                return a.count > b.count
            }
            for path in sorted where config.at(path) != nil {
                removed.formUnion(identities(at: path, in: config))
                let address = Self.originalPath(path, current: config, original: original)
                config.set(path, nil); paths.append(address)
            }
        }
        return CascadeResult(config: config, removed: removed, paths: paths)
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
                for reference in references(in: config) where reference.identity == Identity(kind: kind, name: name) {
                    result.insert(address(reference.path))
                }
                if kind == "outbound" { for (index, injection) in (document["injections"].arrayValue ?? []).enumerated() where injection["group"].stringValue == name { result.insert(platform + ":/injections/\(index)/group") } }
            }
        }
        return result.sorted()
    }
}
