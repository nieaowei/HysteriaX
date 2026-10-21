import Foundation

/// The pinned kernel treats comma-containing regex/logical payloads specially.
struct MihomoTemplateRule: Equatable {
    var kind: String
    var payload: String
    var target: String
    var options: [String]
    init(_ source: String) {
        let parts = source.components(separatedBy: ",").map { $0.trimmingCharacters(in: .whitespaces) }
        kind = parts.first?.uppercased() ?? "MATCH"
        if kind == "MATCH" { payload = ""; target = parts.count > 1 ? parts[1] : "DIRECT"; options = Array(parts.dropFirst(2)) }
        else if Self.commaPayloadKinds.contains(kind), parts.count >= 3 {
            payload = parts.dropFirst().dropLast().joined(separator: ","); target = parts.last ?? "DIRECT"; options = []
        } else {
            payload = parts.count > 1 ? parts[1] : ""; target = parts.count > 2 ? parts[2] : "DIRECT"; options = Array(parts.dropFirst(3))
        }
    }
    static let commaPayloadKinds: Set<String> = ["AND", "OR", "NOT", "SUB-RULE", "DOMAIN-REGEX", "PROCESS-NAME-REGEX", "PROCESS-PATH-REGEX"]
    var source: String { ([kind] + (kind == "MATCH" ? [] : [payload]) + [target] + options).joined(separator: ",") }
    var supportsNoResolve: Bool { ["GEOIP", "IP-ASN", "IP-CIDR", "IP-CIDR6", "IP-SUFFIX", "RULE-SET"].contains(kind) }
    var argumentKind: String {
        if kind == "RULE-SET" { return "rule_set" }
        if ["AND", "OR", "NOT", "SUB-RULE"].contains(kind) { return "predicate" }
        return "value"
    }
    func referenceNames(_ kind: String) -> [String] {
        if kind == "outbound" { return self.kind == "SUB-RULE" ? [] : [target] }
        if kind == "sub_rule" { return self.kind == "SUB-RULE" ? [target] : [] }
        guard kind == "rule_set" else { return [] }
        if self.kind == "RULE-SET" { return [payload] }
        guard ["AND", "OR", "NOT", "SUB-RULE"].contains(self.kind) else { return [] }
        var names: [String] = []
        _ = Self.mapRuleSets(payload) { names.append($0); return $0 }
        return names
    }
    func renamingReference(_ kind: String, old: String, new: String) -> Self {
        guard referenceNames(kind).contains(old) else { return self }
        var next = self
        if (kind == "outbound" && self.kind != "SUB-RULE") || (kind == "sub_rule" && self.kind == "SUB-RULE") {
            if target == old { next.target = new }
        }
        if kind == "rule_set" {
            if self.kind == "RULE-SET", payload == old { next.payload = new }
            else if ["AND", "OR", "NOT", "SUB-RULE"].contains(self.kind) {
                next.payload = Self.mapRuleSets(payload) { $0 == old ? new : $0 }
            }
        }
        return next
    }
    /// Traverse logical predicates, never regex/domain payloads that merely
    /// contain text resembling RULE-SET syntax.
    private static func mapRuleSets(_ raw: String, depth: Int = 0, transform: (String) -> String) -> String {
        guard depth < 40 else { return raw }
        let text = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if text.first == "(" {
            var balance = 0
            var start = text.startIndex
            var parts: [String] = []
            for index in text.indices {
                if balance == 0 && text[index] != "(" && text[index] != "," && !text[index].isWhitespace { return raw }
                if text[index] == "(" { balance += 1 }
                if text[index] == ")" {
                    balance -= 1
                    if balance == 0 {
                        let content = String(text[text.index(after: start)..<index])
                        parts.append("(" + mapRuleSets(content, depth: depth + 1, transform: transform) + ")")
                    }
                }
                if balance == 0, text[index] == "," { start = text.index(after: index) }
                else if balance == 0, text[index].isWhitespace { start = text.index(after: index) }
                if balance < 0 { return raw }
            }
            return balance == 0 && !parts.isEmpty ? parts.joined(separator: ",") : raw
        }
        guard let comma = text.firstIndex(of: ",") else { return raw }
        let kind = text[..<comma].trimmingCharacters(in: .whitespaces).uppercased()
        let rest = String(text[text.index(after: comma)...])
        if kind == "RULE-SET" {
            var pieces = rest.components(separatedBy: ",")
            pieces[0] = transform(pieces[0].trimmingCharacters(in: .whitespaces))
            return String(text[...comma]) + pieces.joined(separator: ",")
        }
        if ["AND", "OR", "NOT"].contains(kind) {
            return String(text[...comma]) + mapRuleSets(rest, depth: depth + 1, transform: transform)
        }
        return raw
    }
}
