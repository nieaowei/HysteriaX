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
    func definition(at path: [String], value: JSONValue) -> JSONValue {
        var definition = root
        var parentPath: [String] = []
        for component in path {
            let parent = resolve(editorDefinition(definition, path: parentPath), value: value.at(parentPath) ?? .null)
            if parent["type"].stringValue == "array", Int(component) != nil {
                definition = parent["items"]
            } else {
                definition = parent["properties"].objectValue?[component] ?? parent["additionalProperties"]
            }
            parentPath.append(component)
        }
        return definition
    }
    func isRequired(_ path: [String], value: JSONValue) -> Bool {
        guard let key = path.last else { return false }
        let parentPath = Array(path.dropLast())
        let definition = definition(at: parentPath, value: value)
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
        let choices = variants(definition)
        let types = Set(choices.compactMap { resolve($0)["type"].stringValue })
        if types == ["integer", "string"] {
            let type = path.last?.hasSuffix("mark") == true ? "string" : "integer"
            let selected = choices.first { resolve($0)["type"].stringValue == type }!
            var metadata = definition.objectValue ?? [:]
            for key in ["$ref", "oneOf", "anyOf", "allOf"] { metadata[key] = nil }
            var field = JSONValue.merging(.object(metadata), resolve(selected))
            if let fallback = field.objectValue?["default"], fallback.schemaType != type { field.set(["default"], nil) }
            field["x-editor-fixed-scalar"] = .string(type)
            return field
        }
        guard Self.isObjectResolver(path),
              let object = variants(definition).first(where: { resolve($0)["type"].stringValue == "object" }) else { return definition }
        return resolve(object)
    }
    static func integerValue(_ text: String) -> JSONValue? {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if let value = Int(text) { return .integer(value) }
        if let value = UInt64(text) { return .unsignedInteger(value) }
        return nil
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
        if let choices = definition["oneOf"].arrayValue ?? definition["anyOf"].arrayValue {
            return choices.flatMap { choice in
                if choice["oneOf"].arrayValue != nil || choice["anyOf"].arrayValue != nil {
                    return variants(choice, depth: depth + 1)
                }
                return [choice]
            }
        }
        return (definition["allOf"].arrayValue ?? []).flatMap { variants($0, depth: depth + 1) }
    }
    /// Fields represented by the selected variant's label belong to that picker.
    /// Other enums (for example HTTP version or matching protocol) remain editable.
    func variantFieldKeys(_ definition: JSONValue, value: JSONValue) -> Set<String> {
        guard variants(definition).count > 1 else { return [] }
        let selected = resolve(definition, value: value)
        guard selected["type"].stringValue == "object",
              TemplateHTTPClientLayout.referenceTitle(selected) == nil else { return [] }
        let properties = selected["properties"]
        for key in ["type", "action", "provider", "protocol", "version"] {
            if properties[key].objectValue?["const"] != nil ||
                properties[key]["enum"].arrayValue?.compactMap(\.stringValue).contains(where: { !$0.isEmpty }) == true {
                if key == "type", properties["version"].objectValue?["const"] != nil { return [key, "version"] }
                return [key]
            }
        }
        return []
    }
    /// Actions are a separate union inside a rule's matching-condition union.
    /// Search compositions only: nested matching rules must not acquire actions.
    func actionDefinition(_ definition: JSONValue, depth: Int = 0) -> JSONValue? {
        guard depth < 50 else { return nil }
        if let ref = definition["$ref"].stringValue, ref.hasPrefix("#/") {
            return actionDefinition(root.at(ref.dropFirst(2).components(separatedBy: "/")) ?? .null, depth: depth + 1)
        }
        let choices = definition["oneOf"].arrayValue ?? definition["anyOf"].arrayValue ?? []
        let actions = choices.compactMap { resolve($0)["properties"]["action"]["const"].stringValue }
        if actions.count > 1, actions.count == choices.count, Set(actions).count == choices.count {
            return definition
        }
        for part in choices + (definition["allOf"].arrayValue ?? []) {
            if let action = actionDefinition(part, depth: depth + 1) { return action }
        }
        return nil
    }
    func selectingAction(_ action: String, from definition: JSONValue, value: JSONValue) -> JSONValue {
        guard let actions = actionDefinition(definition),
              let variant = variants(actions).first(where: { resolve($0)["properties"]["action"]["const"] == .string(action) }) else { return value }
        var current = value
        // An omitted/empty action means route in sing-box. Recognize its fields
        // when removing parameters that do not belong to the newly selected action.
        if current.objectValue?["action"] == nil || current["action"] == .string("") { current["action"] = .string("route") }
        var next = selecting(variant, from: actions, value: current)
        // A key can also belong to the matching conditions (network_type is
        // shared with the direct action). Retain it if the complete rule supports it.
        let supported = resolve(definition, value: next)["properties"].objectValue ?? [:]
        for (key, original) in current.objectValue ?? [:] where next.objectValue?[key] == nil && supported[key] != nil {
            next[key] = original
        }
        return next
    }
    func variantTitle(_ definition: JSONValue, index: Int) -> String {
        let schema = resolve(definition)
        if let title = TemplateHTTPClientLayout.referenceTitle(schema) { return title }
        if schema["x-tag-reference"].stringValue == "certificate_provider" { return L10n.text("共享证书提供者") }
        if let constant = schema.objectValue?["const"] { return Self.pickerTitle(constant) }
        for key in ["type", "action", "provider", "protocol", "version"] {
            if let value = schema["properties"][key].objectValue?["const"] {
                if key == "type", let version = schema["properties"]["version"].objectValue?["const"] {
                    return Self.pickerTitle(value) + " · " + Self.pickerTitle(version)
                }
                return Self.pickerTitle(value)
            }
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

/// Keeps the unset row separate from JSON values and folds documented empty
/// aliases into their default choice without changing the stored document.
struct TemplateEnumPickerOptions {
    let values: [JSONValue]
    let includesUnset: Bool
    let selection: String
    static let unsetToken = "$unset"

    init(values: [JSONValue], current: JSONValue?, defaultValue: JSONValue?, supportsUnset: Bool, emptyUsesDefault: Bool) {
        let empty = JSONValue.string("")
        let emptyAlias = emptyUsesDefault && values.contains(empty)
            && defaultValue != empty && defaultValue.map(values.contains) == true
        includesUnset = supportsUnset && defaultValue == nil
        var choices = values.reduce(into: [JSONValue]()) { result, value in
            if !result.contains(where: { $0.pretty == value.pretty }) { result.append(value) }
        }
        if emptyAlias || includesUnset { choices.removeAll { $0 == empty } }
        if includesUnset && (current == nil || current == empty) {
            selection = Self.unsetToken
        } else {
            let selected = (emptyAlias && current == empty ? defaultValue : current) ?? defaultValue ?? choices.first ?? .null
            if !choices.contains(where: { $0.pretty == selected.pretty }) { choices.append(selected) }
            selection = selected.pretty
        }
        self.values = choices
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
