import Foundation

private struct ProviderFixture: Decodable {
    let provider: String
    let required: [String]
    let optional: [String]
    let config: [String: String]
}

@main
struct ACMEDNSClientTests {
    static func main() throws {
        let fixtures = try JSONDecoder().decode(
            [ProviderFixture].self,
            from: Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))
        )
        precondition(Set(fixtures.map(\.provider)) == Set(ACMEDNSProvider.allCases.map(\.rawValue)))
        for fixture in fixtures {
            let config = fixture.config.mapValues(JSONValue.string)
            var draft = ACMEDNSDraft(provider: fixture.provider.uppercased(), config: config)
            precondition(draft.provider == fixture.provider)
            precondition(draft.validationError == nil)
            precondition(draft.configuration.compactMapValues(\.stringValue) == fixture.config)
            precondition(draft.definition!.fields.filter(\.required).map(\.key) == fixture.required)
            precondition(draft.definition!.fields.filter { !$0.required }.map(\.key) == fixture.optional)
            for key in fixture.required {
                var invalid = draft
                invalid.removeValue(for: key)
                precondition(invalid.validationError != nil)
                invalid.setValue(" \n\t", for: key)
                precondition(invalid.validationError != nil)
            }
            for key in fixture.optional {
                draft.setValue(" \n", for: key)
                precondition(draft.configuration[key] == nil)
            }
            precondition(draft.validationError == nil)
        }

        var draft = ACMEDNSDraft(provider: "cloudflare", config: [
            "cloudflare_api_token": .string("original-secret"),
            "legacy_option": .string("preserve-exactly"),
            "legacy_empty": .string(""),
        ])
        precondition(draft.unknownKeys == ["legacy_empty", "legacy_option"])
        draft.provider = "porkbun"
        precondition(draft.configuration.isEmpty)
        precondition(draft.validationError != nil)
        draft.setValue("new-key", for: "porkbun_api_key")
        draft.setValue("new-secret", for: "porkbun_api_secret_key")
        precondition(draft.validationError == nil)
        precondition(draft.configuration["cloudflare_api_token"] == nil)
        draft.provider = "cloudflare"
        precondition(draft.configuration["cloudflare_api_token"]?.stringValue == "original-secret")
        precondition(draft.configuration["legacy_option"]?.stringValue == "preserve-exactly")
        precondition(draft.configuration["legacy_empty"]?.stringValue == "")
        draft.removeValue(for: "legacy_option")
        precondition(draft.configuration["legacy_option"] == nil)
        draft.provider = "porkbun"
        precondition(draft.configuration["porkbun_api_secret_key"]?.stringValue == "new-secret")
        draft.provider = "namedotcom"
        precondition(draft.validationError != nil)
        precondition(ACMEDNSDraft().validationError != nil)

        var namecheap = ACMEDNSDraft(provider: "namecheap", config: [
            "namecheap_api_key": .string("key"), "namecheap_api_user": .string("user"),
        ])
        for ip in ["127.0.0.1", "203.0.113.1", ""] {
            namecheap.setValue(ip, for: "namecheap_client_ip")
            precondition(namecheap.validationError == nil)
        }
        for ip in ["999.1.1.1", "::1", "invalid", " 203.0.113.1", "1.2.3"] {
            namecheap.setValue(ip, for: "namecheap_client_ip")
            precondition(namecheap.validationError != nil)
        }
        namecheap.removeValue(for: "namecheap_client_ip")
        for endpoint in ["https://api.namecheap.com/xml.response", "http://localhost/xml.response", ""] {
            namecheap.setValue(endpoint, for: "namecheap_api_endpoint")
            precondition(namecheap.validationError == nil)
        }
        for endpoint in ["ftp://example.com", "example.com", "https://", "https://example.com/a b"] {
            namecheap.setValue(endpoint, for: "namecheap_api_endpoint")
            precondition(namecheap.validationError != nil)
        }

        let original: [String: JSONValue] = [
            "acme": .object(["type": .string("dns"), "dns": .object([
                "name": .string("cloudflare"), "config": .object([
                    "cloudflare_api_token": .string("secret"), "unknown": .string("unknown-secret"),
                ]),
            ])]), "listen": .string(":443"),
        ]
        let redacted = ACMEDNSDraft.redactingDNSValues(in: original)
        precondition(redacted["listen"]?.stringValue == original["listen"]?.stringValue)
        let redactedValues = redacted["acme"]!.objectValue!["dns"]!.objectValue!["config"]!.objectValue!
        precondition(redactedValues.values.allSatisfy { $0.stringValue == "<redacted>" })
        precondition(original["acme"]!.objectValue!["dns"]!.objectValue!["config"]!.objectValue!["cloudflare_api_token"]?.stringValue == "secret")
        print("ACME DNS client tests passed: 8 providers, required/optional fields, legacy preservation, provider isolation, validation and preview redaction.")
    }
}
