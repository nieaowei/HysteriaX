import Foundation

extension JSONValue {
    subscript(_ key: String) -> JSONValue {
        get { objectValue?[key] ?? .null }
        set { var map = objectValue ?? [:]; map[key] = newValue; self = .object(map) }
    }
    func at(_ path: [String]) -> JSONValue? {
        guard let first = path.first else { return self }
        if let map = objectValue { return map[first]?.at(Array(path.dropFirst())) }
        if let items = arrayValue, let i = Int(first), items.indices.contains(i) { return items[i].at(Array(path.dropFirst())) }
        return nil
    }
    mutating func set(_ path: [String], _ value: JSONValue?) {
        guard let first = path.first else { self = value ?? .null; return }
        if var items = arrayValue, let i = Int(first), items.indices.contains(i) {
            if path.count == 1, value == nil { items.remove(at: i) }
            else { items[i].set(Array(path.dropFirst()), value) }
            self = .array(items)
        } else {
            var map = objectValue ?? [:]
            if path.count == 1 { map[first] = value }
            else { var child = map[first] ?? .object([:]); child.set(Array(path.dropFirst()), value); map[first] = child }
            self = .object(map)
        }
    }
    var pretty: String {
        let encoder = JSONEncoder(); encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        return (try? String(data: encoder.encode(self), encoding: .utf8)) ?? ""
    }
    static func merging(_ base: JSONValue, _ overlay: JSONValue) -> JSONValue {
        guard var a = base.objectValue, let b = overlay.objectValue else { return overlay }
        for (k, v) in b { a[k] = a[k].map { merging($0, v) } ?? v }
        return .object(a)
    }
}


struct TemplateSchema {
    let root: JSONValue
    static func pickerTitle(_ value: JSONValue) -> String {
        guard let text = value.stringValue else { return value.pretty }
        if text.isEmpty { return L10n.text("未设置") }
        return text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? value.pretty : text
    }
    static func isObjectResolver(_ path: [String]) -> Bool {
        ["domain_resolver", "default_domain_resolver"].contains(path.last ?? "")
    }
    func isRequired(_ path: [String], value: JSONValue) -> Bool {
        guard let key = path.last else { return false }
        var definition = root
        var parentPath: [String] = []
        for component in path.dropLast() {
            let parent = resolve(editorDefinition(definition, path: parentPath), value: value.at(parentPath) ?? .null)
            if parent["type"].stringValue == "array", Int(component) != nil {
                definition = parent["items"]
            } else {
                definition = parent["properties"].objectValue?[component] ?? parent["additionalProperties"]
            }
            parentPath.append(component)
        }
        let parent = resolve(editorDefinition(definition, path: parentPath), value: value.at(parentPath) ?? .null)
        return parent["required"].arrayValue?.contains(.string(key)) == true
            || TemplateRequiredFields.contains(key, definition: definition, schema: parent,
                value: value.at(parentPath) ?? .null, path: parentPath, root: root)
    }
    /// Upstream types a few kernel tag fields as bare strings. Supply the reference kind
    /// locally so the shared catalog stays verbatim while the editor still enumerates tags.
    static func referenceKind(_ schema: JSONValue, path: [String]) -> String? {
        if let kind = schema["x-tag-reference"].stringValue { return kind }
        if path == ["route", "default_http_client"] { return "http_client" }
        return nil
    }
    func editorDefinition(_ definition: JSONValue, path: [String]) -> JSONValue {
        let preferredType: String? = switch path.last {
        case "udp_timeout": "integer"
        case "auto_redirect_input_mark", "auto_redirect_output_mark", "auto_redirect_reset_mark": "string"
        default: nil
        }
        if let preferredType {
            var choices = variants(definition)
            if let index = choices.firstIndex(where: { resolve($0)["type"].stringValue == preferredType }) {
                // Prefer the requested type for new values without changing existing values.
                choices.insert(choices.remove(at: index), at: 0)
                var field = definition.objectValue ?? [:]
                for key in ["$ref", "oneOf", "allOf"] { field.removeValue(forKey: key) }
                field["anyOf"] = .array(choices)
                return .object(field)
            }
        }
        if ["routing_mark", "default_mark"].contains(path.last ?? "") {
            let string = variants(definition).first { resolve($0)["type"].stringValue == "string" }
            var field = resolve(string ?? definition)
            field["type"] = .string("string")
            return field
        }
        guard Self.isObjectResolver(path),
              let object = variants(definition).first(where: { resolve($0)["type"].stringValue == "object" }) else { return definition }
        return resolve(object)
    }
    func scalarListItemDefinition(_ definition: JSONValue) -> JSONValue? {
        let choices = variants(definition).map { resolve($0) }
        guard let scalarType = choices.first(where: { ["string", "integer"].contains($0["type"].stringValue ?? "") })?["type"].stringValue,
              choices.allSatisfy({
                  $0["type"].stringValue == scalarType ||
                  ($0["type"].stringValue == "array" && resolve($0["items"])["type"].stringValue == scalarType)
              }),
              let array = choices.first(where: { $0["type"].stringValue == "array" }),
              variants(array["items"]).allSatisfy({ resolve($0)["type"].stringValue == scalarType }) else { return nil }
        return array["items"]
    }
    func variants(_ definition: JSONValue, depth: Int = 0) -> [JSONValue] {
        guard depth < 50 else { return [] }
        if let ref = definition["$ref"].stringValue, ref.hasPrefix("#/") {
            return variants(root.at(ref.dropFirst(2).components(separatedBy: "/")) ?? .null, depth: depth + 1)
        }
        if let choices = definition["oneOf"].arrayValue ?? definition["anyOf"].arrayValue { return choices }
        return (definition["allOf"].arrayValue ?? []).flatMap { variants($0, depth: depth + 1) }
    }
    func variantTitle(_ definition: JSONValue, index: Int) -> String {
        let schema = resolve(definition)
        if let title = TemplateHTTPClientLayout.referenceTitle(schema) { return title }
        if let constant = schema.objectValue?["const"] { return Self.pickerTitle(constant) }
        for key in ["type", "action", "provider", "protocol", "version"] {
            if let value = schema["properties"][key].objectValue?["const"] { return Self.pickerTitle(value) }
            if let value = schema["properties"][key]["enum"].arrayValue?.compactMap(\.stringValue).first(where: { !$0.isEmpty }) { return Self.pickerTitle(.string(value)) }
        }
        return Self.pickerTitle(schema["title"].stringValue.map(JSONValue.string) ?? schema["type"].stringValue.map(JSONValue.string) ?? .string(String(index + 1)))
    }
    func selecting(_ variant: JSONValue, from definition: JSONValue, value: JSONValue) -> JSONValue {
        let nextSchema = resolve(variant)
        guard let nextProperties = nextSchema["properties"].objectValue else { return initial(variant) }
        let previous = resolve(definition, value: value)["properties"].objectValue ?? [:]
        let recognized = previous.allSatisfy { key, field in
            if let constant = field.objectValue?["const"] { return value[key] == constant }
            if ["type", "provider"].contains(key), let choices = field["enum"].arrayValue {
                return choices.contains(value[key]) || (value.objectValue?[key] == nil && choices.contains(.string("")))
            }
            return true
        }
        let oldProperties = recognized ? previous : [:]
        var next = value.objectValue ?? [:]
        // Keep shared fields and unknown extension keys; discard only old typed fields.
        for key in oldProperties.keys where nextProperties[key] == nil { next[key] = nil }
        for (key, field) in nextProperties {
            if let constant = field.objectValue?["const"] { next[key] = constant }
            else if ["type", "provider"].contains(key), let choices = field["enum"].arrayValue,
                    !choices.contains(next[key] ?? .null), let first = choices.first { next[key] = first }
        }
        return .object(next)
    }
    func resolve(_ schema: JSONValue, value: JSONValue = .null, depth: Int = 0) -> JSONValue {
        guard depth < 50 else { return .object([:]) }
        if let ref = schema["$ref"].stringValue, ref.hasPrefix("#/") {
            return resolve(root.at(ref.dropFirst(2).components(separatedBy: "/")) ?? .object([:]), value: value, depth: depth + 1)
        }
        var result = schema
        if let parts = schema["allOf"].arrayValue {
            for part in parts {
                let resolved = resolve(part, value: value, depth: depth + 1)
                for (k, v) in resolved.objectValue ?? [:] {
                    if k == "required" {
                        result[k] = .array(Array(Set((result[k].arrayValue ?? []).compactMap(\.stringValue) + (v.arrayValue ?? []).compactMap(\.stringValue))).sorted().map(JSONValue.string))
                    } else {
                        result[k] = k == "properties" ? .merging(result[k], v) : v
                    }
                }
            }
        }
        let variants = schema["oneOf"].arrayValue ?? schema["anyOf"].arrayValue ?? []
        if !variants.isEmpty {
            let selected = variants.first { variant in
                let props = resolve(variant, value: value, depth: depth + 1)["properties"].objectValue ?? [:]
                let discriminators = props.filter { key, field in
                    field.objectValue?["const"] != nil || (["type", "provider"].contains(key) && field["enum"].arrayValue != nil)
                }
                if !discriminators.isEmpty {
                    return discriminators.allSatisfy { key, field in
                        if let constant = field.objectValue?["const"] {
                            // Incomplete drafts may omit a secondary discriminator (action or
                            // version). Keep the branch matching their explicit type/provider.
                            if !["type", "provider"].contains(key), value.objectValue?[key] == nil { return true }
                            return value[key] == constant
                        }
                        let choices = field["enum"].arrayValue ?? []
                        return choices.contains(value[key]) || (value.objectValue?[key] == nil && choices.contains(.string("")))
                    }
                }
                return variant["type"].stringValue == value.schemaType
            } ?? variants[0]
            result = resolve(selected, value: value, depth: depth + 1)
        }
        return result
    }
    func initial(_ schema: JSONValue) -> JSONValue {
        let s = resolve(schema)
        if let value = s.objectValue?["default"] ?? s.objectValue?["const"] ?? s["enum"].arrayValue?.first { return value }
        switch s["type"].stringValue {
        case "object": return .object((s["properties"].objectValue ?? [:]).compactMapValues { $0.objectValue?["const"] }); case "array": return .array([]); case "boolean": return .bool(false)
        case "integer", "number": return .integer(s["minimum"].integerValue ?? 0)
        default: return .string("")
        }
    }
}
enum TemplateLineListCodec {
    static func text(_ values: [JSONValue]) -> String {
        values.map { $0.stringValue ?? $0.pretty }.joined(separator: "\n")
    }
    static func parse(_ text: String, integers: Bool) -> [JSONValue]? {
        var values: [JSONValue] = []
        for line in text.components(separatedBy: .newlines) where !line.isEmpty {
            if !integers { values.append(.string(line)); continue }
            let number = line.trimmingCharacters(in: .whitespaces)
            if number.isEmpty { continue }
            if let value = Int(number) { values.append(.integer(value)) }
            else if let value = UInt64(number) { values.append(.unsignedInteger(value)) }
            else { return nil }
        }
        return values
    }
}

/// Stable identities keep duplicate values and pending drag operations distinct.
struct TemplateTokenListDraft {
    struct Token: Identifiable, Equatable {
        let id: UUID
        let value: JSONValue
        init(id: UUID = UUID(), value: JSONValue) { self.id = id; self.value = value }
    }
    private(set) var tokens: [Token]
    var values: [JSONValue] { tokens.map(\.value) }
    init(_ values: [JSONValue]) { tokens = values.map { Token(value: $0) } }
    mutating func synchronize(_ values: [JSONValue]) {
        guard values != self.values else { return }
        let available = Dictionary(grouping: tokens) { $0.value.pretty }
        var consumed: [String: Int] = [:]
        tokens = values.map { value in
            let key = value.pretty
            let index = consumed[key, default: 0]
            consumed[key] = index + 1
            if let matches = available[key], matches.indices.contains(index) { return Token(id: matches[index].id, value: value) }
            return Token(value: value)
        }
    }
    @discardableResult mutating func submit(_ text: String, integers: Bool, replacing id: UUID? = nil) -> Bool {
        guard let parsed = TemplateLineListCodec.parse(text, integers: integers), !parsed.isEmpty else { return false }
        if let id {
            guard let index = tokens.firstIndex(where: { $0.id == id }) else { return false }
            tokens.remove(at: index)
            tokens.insert(contentsOf: parsed.enumerated().map { Token(id: $0.offset == 0 ? id : UUID(), value: $0.element) }, at: index)
        } else { tokens.append(contentsOf: parsed.map { Token(value: $0) }) }
        return true
    }
    mutating func remove(_ id: UUID) { tokens.removeAll { $0.id == id } }
    @discardableResult mutating func move(_ id: UUID, to target: UUID) -> Bool {
        guard let source = tokens.firstIndex(where: { $0.id == id }),
              let destination = tokens.firstIndex(where: { $0.id == target }), source != destination else { return false }
        tokens.insert(tokens.remove(at: source), at: destination)
        return true
    }
}

extension JSONValue {
    var schemaType: String {
        switch self { case .object: "object"; case .array: "array"; case .bool: "boolean"; case .integer, .unsignedInteger: "integer"; case .decimal: "number"; case .null: "null"; case .string: "string" }
    }
}

struct TemplateEditorID: Hashable, Codable {
    let service: String
    let template: String
    init(service: String, template: String) {
        var parts = URLComponents(string: service.trimmingCharacters(in: .whitespacesAndNewlines))
        let scheme = parts?.scheme?.lowercased(); let host = parts?.host?.lowercased()
        parts?.scheme = scheme; parts?.host = host
        if parts?.scheme == "https", parts?.port == 443 { parts?.port = nil }
        self.service = (parts?.string ?? service).trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        self.template = template
    }
}
