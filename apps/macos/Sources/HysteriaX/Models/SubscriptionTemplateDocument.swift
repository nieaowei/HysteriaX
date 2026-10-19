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
                    result[k] = k == "properties" ? .merging(result[k], v) : v
                }
            }
        }
        let variants = schema["oneOf"].arrayValue ?? schema["anyOf"].arrayValue ?? []
        if !variants.isEmpty {
            let selected = variants.first { variant in
                let props = resolve(variant, value: value, depth: depth + 1)["properties"].objectValue ?? [:]
                let constants = props.filter { $0.value.objectValue?["const"] != nil }
                if !constants.isEmpty { return constants.allSatisfy { value[$0.key] == $0.value["const"] } }
                return variant["type"].stringValue == value.schemaType
            } ?? variants[0]
            result = resolve(selected, value: value, depth: depth + 1)
        }
        return result
    }
    func contains(_ query: String, in schema: JSONValue, path: [String] = [], visited: Set<String> = []) -> Bool {
        guard !query.isEmpty else { return true }
        if path.joined(separator: "/").localizedCaseInsensitiveContains(query) { return true }
        if let reference = schema["$ref"].stringValue {
            guard !visited.contains(reference), let target = root.at(reference.dropFirst(2).components(separatedBy: "/")) else { return false }
            return contains(query, in: target, path: path, visited: visited.union([reference]))
        }
        for (key, child) in schema["properties"].objectValue ?? [:] {
            if contains(query, in: child, path: path + [key], visited: visited) { return true }
        }
        for union in ["allOf", "anyOf", "oneOf"] {
            for child in schema[union].arrayValue ?? [] { if contains(query, in: child, path: path, visited: visited) { return true } }
        }
        for child in [schema.objectValue?["items"], schema.objectValue?["additionalProperties"]].compactMap({ $0 }) {
            if contains(query, in: child, path: path, visited: visited) { return true }
        }
        return false
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
extension JSONValue {
    var schemaType: String {
        switch self { case .object: "object"; case .array: "array"; case .bool: "boolean"; case .integer, .unsignedInteger: "integer"; case .decimal: "number"; case .null: "null"; case .string: "string" }
    }
}
