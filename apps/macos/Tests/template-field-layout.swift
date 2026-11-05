import Foundation

@main struct TemplateFieldLayoutCatalogTests {
    static func main() throws {
        let dialerOrder = [
            "detour", "bind_interface", "inet4_bind_address", "inet6_bind_address",
            "bind_address_no_port", "routing_mark", "reuse_addr", "netns", "connect_timeout",
            "tcp_fast_open", "tcp_multi_path", "disable_tcp_keep_alive", "tcp_keep_alive",
            "tcp_keep_alive_interval", "udp_fragment", "domain_resolver", "network_strategy",
            "network_type", "fallback_network_type", "fallback_delay"
        ]
        precondition(TemplateFieldLayout.orderedObjectFields(dialerOrder.reversed(), required: ["fallback_delay"]) == dialerOrder)
        precondition(TemplateFieldLayout.dialerFields(in: dialerOrder.reversed()) == dialerOrder)
        precondition(TemplateFieldLayout.dialerFields(in: ["type", "network_type", "network_strategy"]).isEmpty, "Match conditions alone must not become dialer settings")
        for key in dialerOrder {
            precondition(TemplateFieldLayout.objectSection(key, path: ["outbounds", "0"]) == "网络连接")
        }
        let base = URL(fileURLWithPath: CommandLine.arguments[1])
        var failures: [String] = []
        for format in ["singbox", "mihomo"] {
            let root = try JSONSerialization.jsonObject(with: Data(contentsOf: base.appendingPathComponent(format + ".schema.json"))) as! [String: Any]
            func resolve(_ schema: [String: Any]) -> [String: Any] {
                if let ref = schema["$ref"] as? String {
                    var target: Any = root
                    for key in ref.dropFirst(2).split(separator: "/") { target = (target as! [String: Any])[String(key)]! }
                    return resolve(target as! [String: Any])
                }
                if let variants = (schema["oneOf"] ?? schema["anyOf"]) as? [[String: Any]], let first = variants.first { return resolve(first) }
                var result = schema
                for part in schema["allOf"] as? [[String: Any]] ?? [] {
                    for (key, value) in resolve(part) {
                        if key == "properties" { result[key] = (result[key] as? [String: Any] ?? [:]).merging(value as? [String: Any] ?? [:]) { _, new in new } }
                        else { result[key] = value }
                    }
                }
                return result
            }
            var sections: [String: Set<String>] = [:]
            var paths: Set<String> = []
            let properties = root["properties"] as! [String: [String: Any]]
            for (key, definition) in properties {
                let module = TemplateModule.destination(key)
                let schema = resolve(definition)
                var fields = [([key], schema)]
                if TemplateFieldLayout.expandedObjects.contains(key), schema["type"] as? String == "object", schema["x-unverified"] as? Bool != true, let children = schema["properties"] as? [String: [String: Any]], !children.isEmpty {
                    fields = children.map { ([key, $0.key], resolve($0.value)) }
                }
                for (path, field) in fields {
                    let complex = ["array", "object"].contains(field["type"] as? String ?? "")
                    let title = TemplateFieldLayout.section(path: path, complex: complex)
                    let pointer = path.joined(separator: "/")
                    paths.insert(pointer)
                    if title == "其他选项" || (complex && title == path.last) { failures.append(format + ": " + pointer + " => " + title) }
                    sections[module, default: []].insert(title)
                }
            }
            print("\(format): \(paths.count) field paths, \(sections.values.reduce(0) { $0 + $1.count }) sections")
            print("DNS: " + (sections["DNS"] ?? []).sorted { TemplateFieldLayout.precedes($0, $1, module: "DNS") }.joined(separator: "、"))
        }
        precondition(failures.isEmpty, failures.joined(separator: "\n"))
        print("Template catalog section coverage passed: all supported fields have named sections.")
    }
}
