import Foundation
import Synchronization

private final class TemplateSessionSpy: URLProtocol, @unchecked Sendable {
    static let requests = Mutex(0)
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        Self.requests.withLock { $0 += 1 }
        let response = HTTPURLResponse(url: request.url!, statusCode: 409, httpVersion: nil, headerFields: ["Content-Type":"application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data(#"{"error":{"code":"revision_conflict","message":"conflict"}}"#.utf8))
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}

@main struct SubscriptionTemplateClientTests {
    @MainActor static func main() async throws {
        let store = SubscriptionTemplateStore()
        store.document = .object(["schema_version":.integer(1),"common":.object(["flag":.bool(true),"list":.array([.object(["n":.integer(1)]),.object(["n":.integer(2)])]),"unknown":.object(["future":.null])]),"platforms":.object([:]),"injections":.array([])])
        store.baseline = store.document
        store.layer = "windows"
        precondition(store.state(["flag"]) == "inherit")
        store.set(["flag"], .bool(false)); precondition(store.effective["flag"] == .bool(false))
        store.set(["list", "1", "n"], .integer(0))
        precondition(store.overlay["list"].arrayValue?.count == 2)
        precondition(store.common["list"].arrayValue?[1]["n"] == .integer(2))
        store.setState("remove", path: ["flag"], schema: .object(["type":.string("boolean")]))
        precondition(store.effective.objectValue?["flag"] == nil)
        store.setState("inherit", path: ["flag"], schema: .object([:]))
        precondition(store.effective["flag"] == .bool(true))
        store.undo(); precondition(store.effective.objectValue?["flag"] == nil)
        store.redo(); precondition(store.effective["flag"] == .bool(true))
        precondition(store.effective["unknown"] == store.common["unknown"])
        let before = store.document
        let encoded = try JSONEncoder().encode(before)
        let decoded = try JSONDecoder().decode(JSONValue.self, from: encoded)
        precondition(decoded == before)
        let exactInteger = try JSONDecoder().decode(JSONValue.self, from: Data("{\"counter\":18446744073709551615}".utf8))
        precondition(exactInteger["counter"] == .unsignedInteger(UInt64.max))
        let exactEncoded = try JSONEncoder().encode(exactInteger)
        precondition(String(decoding: exactEncoded, as: UTF8.self).contains("18446744073709551615"))
        let regex = MihomoTemplateRule("DOMAIN-REGEX,^(one,two|three)$,Policy")
        precondition(regex.payload == "^(one,two|three)$" && regex.target == "Policy" && regex.source == "DOMAIN-REGEX,^(one,two|three)$,Policy")
        let options = MihomoTemplateRule("IP-CIDR,192.168.0.0/16,DIRECT,src,no-resolve")
        precondition(options.target == "DIRECT" && options.options == ["src", "no-resolve"])
        let logic = MihomoTemplateRule("AND,((RULE-SET,private),(NETWORK,tcp)),Policy")
        precondition(logic.source == "AND,((RULE-SET,private),(NETWORK,tcp)),Policy")
        for format in ["mihomo", "singbox"] {
            let url = URL(fileURLWithPath: CommandLine.arguments[1]).appendingPathComponent("\(format).schema.json")
            let root = try JSONDecoder().decode(JSONValue.self, from: Data(contentsOf: url))
            let catalog = TemplateSchema(root: root)
            var count = 0
            func walk(_ value: JSONValue, depth: Int = 0) {
                guard depth < 60 else { return }
                if let ref = value["$ref"].stringValue {
                    precondition(root.at(ref.dropFirst(2).components(separatedBy:"/")) != nil, "unresolved schema reference: \(ref)")
                }
                if let type = value["type"].stringValue {
                    precondition(["object","array","string","number","integer","boolean","null"].contains(type), "missing typed control: \(type)")
                    count += 1
                }
                for child in value.objectValue?.values ?? [:].values { walk(child, depth: depth + 1) }
                for child in value.arrayValue ?? [] { walk(child, depth: depth + 1) }
            }
            walk(root)
            precondition(count > 100)
            if format == "singbox" {
                precondition(catalog.contains("process_name", in: root["properties"]["route"], path: ["route"]))
                precondition(catalog.contains("cache_file", in: root["properties"]["experimental"], path: ["experimental"]))
                precondition(!catalog.contains("does-not-exist", in: root))
            } else {
                precondition(root["x-rule-types"].arrayValue?.count ?? 0 > 30)
                let allRules = try JSONDecoder().decode(JSONValue.self, from: Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]).appendingPathComponent("platform-capabilities.json")))
                store.catalog = .object(["schema":root,"platform_rules":allRules["mihomo"]]); store.layer = "common"
                store.restoreDefault(path: ["ipv6"], definition: root["properties"]["ipv6"])
                precondition(store.common["ipv6"] == .bool(true))
                store.restoreDefault(path: ["tun"], definition: root["properties"]["tun"])
                precondition(store.common["tun"].objectValue?["recvmsgx"] == nil)
                precondition(root["properties"]["tun"]["default"]["recvmsgx"] == .bool(true))
            }
            let common = catalog.resolve(root, value: .object([:]))
            precondition(common["properties"].objectValue?.isEmpty == false)
        }
        let originalAPI = APIClient(baseURL: URL(string: "https://original.invalid")!, token: "fixture")
        let configuration = URLSessionConfiguration.ephemeral; configuration.protocolClasses = [TemplateSessionSpy.self]
        let switchedAPI = APIClient(baseURL: URL(string: "https://different.invalid")!, token: "fixture", session: URLSession(configuration: configuration))
        store.connectedAPI = originalAPI
        store.selected = try JSONDecoder().decode(SubscriptionTemplate.self, from: Data(#"{"id":"default-mihomo","name":"Original","format":"mihomo","revision":1,"archived":false,"is_default":true,"published_version":1,"updated_at":"2026-10-18T00:00:00Z"}"#.utf8))
        let retainedDocument = store.document
        do { try await store.save(switchedAPI); preconditionFailure("a changed service must not receive the previous service's draft") } catch APIClientError.server { }
        precondition(TemplateSessionSpy.requests.withLock { $0 } == 0 && store.document == retainedDocument)
        let deletion = SubscriptionTemplateStore(); deletion.catalog = store.catalog; deletion.selected = store.selected
        deletion.document = .object(["schema_version":.integer(1),"common":.object(["rule-providers":.object(["A":.object(["type":.string("inline"),"behavior":.string("classical"),"payload":.array([])])]),"rules":.array([.string("RULE-SET,A,DIRECT")])]),"platforms":.object([:]),"injections":.array([])])
        let beforeDeletion = deletion.document
        deletion.deleteField(parent: ["rule-providers"], key: "A")
        precondition(deletion.document == beforeDeletion && deletion.error?.contains("/rules/0") == true)
        deletion.document.set(["platforms","windows","set","rules"], .array([.string("MATCH,DIRECT")]))
        deletion.layer = "windows"; deletion.deleteField(parent: ["rule-providers"], key: "A")
        precondition(deletion.effective["rule-providers"].objectValue?["A"] == nil)
        precondition(deletion.common["rule-providers"].objectValue?["A"] != nil)
        precondition(deletion.removals.contains("/rule-providers/A"))
        deletion.layer = "common"
        deletion.document = .object(["schema_version":.integer(1),"common":.object(["proxy-groups":.array([.object(["name":.string("A"),"type":.string("select"),"proxies":.array([.string("DIRECT")])]),.object(["name":.string("B"),"type":.string("select"),"proxies":.array([.string("A")])])]),"rules":.array([.string("MATCH,B")])]),"platforms":.object([:]),"injections":.array([])])
        let beforeGroupDeletion = deletion.document
        deletion.deleteItem(path: ["proxy-groups"], index: 0)
        precondition(deletion.document == beforeGroupDeletion && deletion.error?.contains("/proxy-groups/1/proxies/0") == true)
        print("Template overlay semantics, whole-array overrides, undo/redo, unknown fields, and typed catalog coverage passed.")
    }
}
