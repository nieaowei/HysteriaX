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
}
