import Foundation
import Synchronization
import Observation

private final class TemplateObservationFlag: Sendable {
    let value = Mutex(false)
}

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

private final class TemplateSaveSession: URLProtocol, @unchecked Sendable {
    static let requests = Mutex(0)
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        Self.requests.withLock { $0 += 1 }
        let parsing = request.url?.path.hasSuffix("/source") == true
        let response = HTTPURLResponse(url: request.url!, statusCode: parsing ? 400 : 200, httpVersion: nil, headerFields: ["Content-Type":"application/json"])!
        let body = parsing ? #"{"error":{"code":"bad_request","message":"invalid source"}}"# : #"{"id":"default-mihomo","name":"Original","format":"mihomo","revision":2,"archived":false,"is_default":true,"published_version":1,"updated_at":"2026-10-19T00:00:00Z"}"#
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data(body.utf8)); client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}

private final class OfflineTemplateSession: URLProtocol, @unchecked Sendable {
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() { client?.urlProtocol(self, didFailWithError: URLError(.notConnectedToInternet)) }
    override func stopLoading() {}
}

private final class DelayedTemplatePreview: URLProtocol, @unchecked Sendable {
    static let started = Mutex(0)
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        Self.started.withLock { $0 += 1 }
        DispatchQueue.global().asyncAfter(deadline: .now() + 0.15) { [self] in
            let response = HTTPURLResponse(url: request.url!, statusCode: 200, httpVersion: nil, headerFields: ["Content-Type":"application/json"])!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: Data(#"{"source":"old preview","diagnostics":[]}"#.utf8))
            client?.urlProtocolDidFinishLoading(self)
        }
    }
    override func stopLoading() {}
}

@main struct SubscriptionTemplateClientTests {
    @MainActor static func main() async throws {
        let lineIntegers: [JSONValue] = [.integer(-1), .integer(0), .unsignedInteger(UInt64.max)]
        precondition(TemplateLineListCodec.parse("-1\n\n 0 \n18446744073709551615", integers: true) == lineIntegers)
        precondition(TemplateLineListCodec.parse(TemplateLineListCodec.text(lineIntegers), integers: true) == lineIntegers)
        for invalid in ["1\nno", "1.5", "18446744073709551616"] {
            precondition(TemplateLineListCodec.parse(invalid, integers: true) == nil)
        }
        precondition(TemplateLineListCodec.parse("001\ntext", integers: false) == [.string("001"), .string("text")])
        var tags = TemplateTokenListDraft([.string("same"), .string("same"), .string("third")])
        let firstTag = tags.tokens[0].id
        let secondTag = tags.tokens[1].id
        let thirdTag = tags.tokens[2].id
        tags.remove(secondTag)
        precondition(tags.values == [.string("same"), .string("third")] && tags.tokens[0].id == firstTag)
        precondition(tags.move(thirdTag, to: firstTag) && tags.values == [.string("third"), .string("same")])
        precondition(tags.submit("one\ntwo\n", integers: false, replacing: firstTag))
        precondition(tags.values == [.string("third"), .string("one"), .string("two")] && tags.tokens[1].id == firstTag)
        let beforeInvalidPaste = tags.tokens
        precondition(!tags.submit("1\ninvalid\n2", integers: true) && tags.tokens == beforeInvalidPaste)
        precondition(!tags.move(UUID(), to: firstTag))
        tags.synchronize([.string("one"), .string("third"), .string("one")])
        precondition(tags.tokens[0].id == firstTag && tags.tokens[1].id == thirdTag && tags.tokens[2].id != firstTag)
        precondition(tags.submit("-1\n18446744073709551615", integers: true))
        precondition(Array(tags.values.suffix(2)) == [.integer(-1), .unsignedInteger(UInt64.max)])
        let listSchema = TemplateSchema(root: .object([:]))
        let requiredSchema = TemplateSchema(root: .object(["type": .string("object"), "properties": .object([
            "items": .object(["type": .string("array"), "items": .object(["oneOf": .array([
                .object(["allOf": .array([
                    .object(["type": .string("object"), "properties": .object(["type": .object(["const": .string("remote")])]), "required": .array([.string("type")])]),
                    .object(["properties": .object(["url": .object(["type": .string("string")])]), "required": .array([.string("url")])])
                ])]),
                .object(["type": .string("object"), "properties": .object(["type": .object(["const": .string("local")])]), "required": .array([.string("type")])])
            ])])])
        ])]))
        let remoteValue: JSONValue = .object(["items": .array([.object(["type": .string("remote")])])])
        let localValue: JSONValue = .object(["items": .array([.object(["type": .string("local")])])])
        precondition(requiredSchema.isRequired(["items", "0", "url"], value: remoteValue))
        precondition(requiredSchema.isRequired(["items", "0", "type"], value: remoteValue))
        precondition(!requiredSchema.isRequired(["items", "0", "url"], value: localValue))
        precondition(!requiredSchema.isRequired(["items"], value: remoteValue))
        precondition(!requiredSchema.isRequired([], value: remoteValue))
        let timeoutUnion: JSONValue = .object(["anyOf": .array([
            .object(["type": .string("string")]),
            .object(["type": .string("integer"), "minimum": .integer(0)])
        ])])
        let timeoutDefinition = listSchema.editorDefinition(timeoutUnion, path: ["inbounds", "0", "udp_timeout"])
        precondition(listSchema.initial(timeoutDefinition) == .integer(0))
        precondition(listSchema.resolve(timeoutDefinition, value: .string("5m"))["type"] == .string("string"))
        precondition(listSchema.editorDefinition(timeoutUnion, path: ["other_timeout"]) == timeoutUnion)
        let durationOnly: JSONValue = .object(["type": .string("string")])
        precondition(listSchema.editorDefinition(durationOnly, path: ["udp_timeout"]) == durationOnly)
        for key in ["auto_redirect_input_mark", "auto_redirect_output_mark", "auto_redirect_reset_mark"] {
            let markDefinition = listSchema.editorDefinition(timeoutDefinition, path: ["inbounds", "0", key])
            precondition(listSchema.initial(markDefinition) == .string(""))
            precondition(listSchema.resolve(markDefinition, value: .integer(1234))["type"] == .string("integer"))
            precondition(listSchema.variants(markDefinition).count == 2)
        }
        for type in ["string", "integer"] {
            let item: JSONValue = .object(["type": .string(type)])
            let union: JSONValue = .object(["anyOf": .array([item, .object(["type": .string("array"), "items": item])])])
            precondition(listSchema.scalarListItemDefinition(union) == item)
        }
        precondition(listSchema.scalarListItemDefinition(.object(["anyOf": .array([
            .object(["type": .string("integer")]),
            .object(["type": .string("array"), "items": .object(["type": .string("object")])])
        ])])) == nil)
        let unified = SubscriptionTemplateStore()
        unified.document = .object([
            "schema_version": .integer(1),
            "common": .object(["flag": .bool(true), "unknown": .object(["value": .null])]),
            "platforms": .object(["windows": .object(["set": .object(["flag": .bool(false), "list": .array([.integer(0)])]), "remove": .array([])])]),
            "injections": .array([.object(["group": .string("Select")])])
        ])
        unified.catalog = .object(["platform_rules": .array([
            .object(["path": .string("/inbounds/*/type"), "value": .string("tproxy"), "platforms": .array([.string("linux")])]),
            .object(["path": .string("/tun/**/device"), "platforms": .array([.string("linux"), .string("windows")])]),
            .object(["path": .string("/client_owned"), "platforms": .array([])])
        ])])
        let legacy = unified.document
        precondition(unified.needsPlatformConversion)
        precondition(unified.singleConfiguration(from: "windows")["common"]["flag"] == .bool(false))
        precondition(unified.document == legacy, "Review must not modify the legacy document")
        unified.convertToSingleConfiguration(from: "windows")
        precondition(!unified.needsPlatformConversion && unified.document["schema_version"] == .integer(2))
        precondition(unified.common["unknown"]["value"] == .null && unified.common["list"] == .array([.integer(0)]))
        precondition(unified.document["injections"] == legacy["injections"])
        precondition(unified.allowed(["inbounds", "0", "type"], value: .string("tproxy")))
        precondition(unified.supportedPlatforms(["inbounds", "0", "type"], value: .string("tproxy")) == ["linux"])
        precondition(unified.supportedPlatforms(["inbounds", "0", "type"], value: .string("mixed")) == TemplatePlatform.all)
        precondition(unified.supportedPlatforms(["tun", "nested", "device"]) == ["linux", "windows"])
        precondition(!unified.allowed(["client_owned"]))
        unified.undo(); precondition(unified.document == legacy)
        unified.redo(); precondition(unified.document["schema_version"] == .integer(2))
        let store = SubscriptionTemplateStore()
        let pickerChoices = SubscriptionTemplateStore()
        pickerChoices.document = .object(["schema_version": .integer(2), "common": .object([
            "http_clients": .array([
                .object(["tag": .string("")]),
                .object(["tag": .string("main")]),
                .object(["tag": .string("main")]),
                .object(["tag": .array([.string(""), .string("backup"), .string("main")])])
            ]),
            "route": .object(["default_http_client": .string("main")])
        ])])
        precondition(pickerChoices.referenceNames("http_client") == ["backup", "main"])
        let clientPath = ["route", "default_http_client"]
        pickerChoices.setState("inherit", path: clientPath, schema: .object(["type": .string("string")]))
        precondition(pickerChoices.effective.at(clientPath) == nil)
        precondition(pickerChoices.referenceNames("http_client") == ["backup", "main"])
        pickerChoices.undo()
        precondition(pickerChoices.effective.at(clientPath) == .string("main"))
        let resolver: JSONValue = .object(["anyOf": .array([
            .object(["type": .string("string")]),
            .object(["type": .string("object"), "properties": .object([
                "server": .object(["type": .string("string")]),
                "timeout": .object(["type": .string("string")])
            ])])
        ])])
        let resolverEditor = SubscriptionTemplateStore()
        resolverEditor.document = .object(["schema_version": .integer(2), "common": .object([
            "outbounds": .array([.object(["domain_resolver": .string("dns-main")])]),
            "route": .object(["default_domain_resolver": .string("dns-default")])
        ])])
        let resolverPath = ["outbounds", "0", "domain_resolver"]
        let objectResolver = resolverEditor.schema.editorDefinition(resolver, path: resolverPath)
        precondition(objectResolver["type"] == .string("object") && resolverEditor.schema.variants(objectResolver).isEmpty)
        let beforeResolverEdit = resolverEditor.document
        precondition(resolverEditor.editorValue(objectResolver, path: resolverPath)["server"] == .string("dns-main"))
        precondition(resolverEditor.editorValue(.object(["type": .string("string")]), path: resolverPath + ["server"]) == .string("dns-main"))
        precondition(resolverEditor.document == beforeResolverEdit)
        resolverEditor.set(resolverPath + ["timeout"], .string("5s"))
        precondition(resolverEditor.effective.at(resolverPath) == .object(["server": .string("dns-main"), "timeout": .string("5s")]))
        resolverEditor.undo(); precondition(resolverEditor.document == beforeResolverEdit)
        let defaultResolverPath = ["route", "default_domain_resolver"]
        let defaultResolverDefinition = resolverEditor.schema.editorDefinition(resolver, path: defaultResolverPath)
        precondition(defaultResolverDefinition["type"] == .string("object") && resolverEditor.schema.variants(defaultResolverDefinition).isEmpty)
        precondition(resolverEditor.editorValue(defaultResolverDefinition, path: defaultResolverPath)["server"] == .string("dns-default"))
        resolverEditor.set(defaultResolverPath + ["timeout"], .string("3s"))
        precondition(resolverEditor.effective.at(defaultResolverPath) == .object(["server": .string("dns-default"), "timeout": .string("3s")]))
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
                for choice in value["enum"].arrayValue ?? value["x-enum-choices"].arrayValue ?? [] {
                    precondition(!TemplateSchema.pickerTitle(choice).trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, "Blank enum label: \(choice.pretty)")
                }
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
            if format == "mihomo" {
                let group: JSONValue = .object(["proxy-groups": .array([.object(["name": .string("test"), "type": .string("select")])])])
                precondition(catalog.isRequired(["proxy-groups", "0", "name"], value: group))
                precondition(catalog.isRequired(["proxy-groups", "0", "proxies"], value: group))
                var automaticGroup = group
                automaticGroup.set(["proxy-groups", "0", "include-all"], .bool(true))
                precondition(!catalog.isRequired(["proxy-groups", "0", "proxies"], value: automaticGroup))
                let provider: JSONValue = .object(["rule-providers": .object(["test": .object(["type": .string("http")])])])
                precondition(catalog.isRequired(["rule-providers", "test", "url"], value: provider))
                precondition(catalog.isRequired(["rule-providers", "test", "behavior"], value: provider))
                precondition(!catalog.isRequired(["rule-providers", "test", "path"], value: provider))
            }
            if format == "singbox" {
                // Every documented annotation must point to a real field in the pinned catalog.
                for (group, fields) in TemplateRequiredFields.singbox {
                    let parts = group.split(separator: ":", maxSplits: 1).map(String.init)
                    var probeRoot = root
                    probeRoot["properties"]["probe"] = .object(["$ref": .string("#/$defs/" + parts[0])])
                    let probe = TemplateSchema(root: probeRoot)
                    let value: JSONValue = .object(["probe": .object(["type": .string(parts.count == 2 ? parts[1] : ""), "enabled": .bool(true)])])
                    for field in fields {
                        precondition(probe.isRequired(["probe", field], value: value), "Missing documented required marker: \(group).\(field)")
                    }
                }
                var requiredConfig: JSONValue = .object([
                    "dns": .object(["servers": .array([.object(["type": .string("https")])])]),
                    "outbounds": .array([.object(["type": .string("selector")])]),
                    "route": .object(["rule_set": .array([.object(["type": .string("remote"), "url": .string("https://example.com/rules.srs?token=value")])])])
                ])
                precondition(catalog.isRequired(["dns", "servers", "0", "server"], value: requiredConfig))
                precondition(!catalog.isRequired(["dns", "servers", "0", "server_port"], value: requiredConfig))
                precondition(catalog.isRequired(["outbounds", "0", "outbounds"], value: requiredConfig))
                precondition(catalog.isRequired(["route", "rule_set", "0", "url"], value: requiredConfig))
                precondition(!catalog.isRequired(["route", "rule_set", "0", "format"], value: requiredConfig))
                requiredConfig.set(["route", "rule_set", "0", "url"], .string("https://example.com/download"))
                precondition(catalog.isRequired(["route", "rule_set", "0", "format"], value: requiredConfig))
                requiredConfig.set(["dns", "servers", "0", "type"], .string("local"))
                precondition(!catalog.isRequired(["dns", "servers", "0", "server"], value: requiredConfig))
                requiredConfig.set(["endpoints"], .array([.object(["type": .string("wireguard"), "peers": .array([.object([:])])])]))
                precondition(catalog.isRequired(["endpoints", "0", "peers", "0", "public_key"], value: requiredConfig))
                requiredConfig.set(["inbounds"], .array([.object(["type": .string("vless"), "users": .array([.object([:])])])]))
                precondition(catalog.isRequired(["inbounds", "0", "users", "0", "uuid"], value: requiredConfig))
                requiredConfig.set(["outbounds"], .array([.object(["type": .string("hysteria2"), "server_ports": .array([.string("1000:2000")])])]))
                precondition(!catalog.isRequired(["outbounds", "0", "server_port"], value: requiredConfig))
                requiredConfig.set(["outbounds", "0", "server_ports"], .string("1000:2000"))
                precondition(!catalog.isRequired(["outbounds", "0", "server_port"], value: requiredConfig))
                requiredConfig.set(["outbounds", "0", "realm"], .object([:]))
                precondition(!catalog.isRequired(["outbounds", "0", "server"], value: requiredConfig))
                requiredConfig.set(["outbounds"], .array([.object(["type": .string("vless"), "tls": .object(["reality": .object(["enabled": .bool(true)])])])]))
                precondition(catalog.isRequired(["outbounds", "0", "tls", "reality", "public_key"], value: requiredConfig))
                requiredConfig.set(["outbounds", "0", "tls", "reality", "enabled"], .bool(false))
                precondition(!catalog.isRequired(["outbounds", "0", "tls", "reality", "public_key"], value: requiredConfig))
                let ruleSet = root["$defs"]["RuleSet"]
                let clientReference = root["$defs"]["HTTPClientReference"]
                let clientDefinition = catalog.resolve(root["$defs"]["HTTPClient"])
                precondition(TemplateHTTPClientLayout.matches(clientDefinition))
                precondition(TemplateHTTPClientLayout.transportKeys(.object([:])) == TemplateHTTPClientLayout.http2)
                precondition(TemplateHTTPClientLayout.transportKeys(.object(["version": .integer(1)])).isEmpty)
                precondition(TemplateHTTPClientLayout.transportKeys(.object(["version": .integer(3)])) == TemplateHTTPClientLayout.http2 + TemplateHTTPClientLayout.quic)
                precondition(TemplateHTTPClientLayout.transportKeys(.object(["engine": .string("apple")])).isEmpty)
                precondition(TemplateHTTPClientLayout.memoryValue("64 MB") == .string("64 MB"))
                precondition(TemplateHTTPClientLayout.memoryValue("65536") == .integer(65536))
                precondition(TemplateHTTPClientLayout.memoryValue("18446744073709551615") == .unsignedInteger(UInt64.max))
                let appleClient: JSONValue = .object([
                    "tag": .string("client"), "engine": .string("apple"), "version": .integer(3),
                    "idle_timeout": .string("30s"), "detour": .string("proxy"), "headers": .object(["Host": .string("example.com")]),
                    "future_option": .bool(true),
                    "tls": .object(["server_name": .string("example.com"), "alpn": .array([.string("h3")])])
                ])
                let clientTLS = catalog.resolve(clientDefinition["properties"]["tls"])
                let clearedClient = TemplateHTTPClientLayout.clearingIncompatible(appleClient, tlsSchema: clientTLS)
                precondition(TemplateHTTPClientLayout.incompatiblePaths(appleClient, tlsSchema: clientTLS) == [["version"], ["idle_timeout"], ["tls", "alpn"]])
                precondition(clearedClient["tls"] == .object(["server_name": .string("example.com")]))
                for key in ["tag", "engine", "detour", "headers", "future_option"] { precondition(clearedClient[key] == appleClient[key]) }
                precondition(TemplateHTTPClientLayout.incompatiblePaths(clearedClient, tlsSchema: clientTLS).isEmpty)
                precondition(catalog.resolve(clientReference, value: .object(["engine": .string("go")]))["type"] == .string("object"), "An optional version enum must not act as a type discriminator")
                let inlineRuleSet = catalog.variants(ruleSet).first { catalog.variantTitle($0, index: 0) == "inline" }!
                let remoteRuleSet = catalog.variants(ruleSet).first { catalog.variantTitle($0, index: 0) == "remote" }!
                let switchedInline = catalog.selecting(inlineRuleSet, from: ruleSet, value: .object([
                    "type": .string("remote"), "tag": .string("test"), "url": .string("https://example.invalid/rules"), "format": .string("binary")
                ]))
                precondition(switchedInline["type"] == .string("inline") && switchedInline.objectValue?["url"] == nil && switchedInline.objectValue?["format"] == nil)
                let switchedImplicit = catalog.selecting(remoteRuleSet, from: ruleSet, value: .object([
                    "tag": .string("test"), "rules": .array([]), "future-field": .string("keep")
                ]))
                precondition(switchedImplicit["type"] == .string("remote") && switchedImplicit.objectValue?["rules"] == nil && switchedImplicit["future-field"] == .string("keep"))
                for kind in ["remote", "local", "inline"] {
                    let current: JSONValue = .object(["type": .string(kind), "tag": .string("rules")])
                    let resolved = catalog.resolve(ruleSet, value: current)
                    let fields = resolved["properties"].objectValue ?? [:]
                    precondition(fields[kind == "remote" ? "url" : kind == "local" ? "path" : "rules"] != nil, "Wrong rule_set variant for \(kind)")
                    if kind != "inline" { precondition(fields["format"]?["enum"] == .array([.string("source"), .string("binary")])) }
                }
                let outbound = root["properties"]["outbounds"]["items"]
                let variants = catalog.variants(outbound)
                precondition(variants.count > 10, "Referenced outbound types must be available in the editor")
                let direct = variants.first { catalog.resolve($0)["properties"]["type"]["const"] == .string("direct") }!
                let current: JSONValue = .object(["type": .string("shadowsocks"), "tag": .string("Proxy"), "server": .string("example.com"), "server_port": .integer(443), "method": .string("aes-128-gcm"), "password": .string("fixture"), "future-field": .unsignedInteger(UInt64.max)])
                let switched = catalog.selecting(direct, from: outbound, value: current)
                precondition(switched["type"] == .string("direct") && switched["tag"] == .string("Proxy"))
                precondition(switched.objectValue?["server"] == nil && switched.objectValue?["password"] == nil && switched.objectValue?["method"] == nil)
                precondition(switched["future-field"] == current["future-field"], "Type changes must retain unknown extension values")
                let unknown = catalog.selecting(direct, from: outbound, value: .object(["type": .string("future-protocol"), "server": .string("retained.example")]))
                precondition(unknown["server"] == .string("retained.example"), "Unrecognized source types must not lose their fields")
            }
            if format != "singbox" {
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
        deletion.set(["proxy-groups", "0", "name"], .string("Renamed"))
        precondition(deletion.document == beforeGroupDeletion && deletion.error != nil)
        deletion.deleteItem(path: ["proxy-groups"], index: 0)
        precondition(deletion.document == beforeGroupDeletion && deletion.error?.contains("/proxy-groups/1/proxies/0") == true)
        let identity = TemplateEditorID(service: " HTTPS://EXAMPLE.COM:443/ ", template: "same")
        precondition(identity == TemplateEditorID(service: "https://example.com", template: "same"))
        precondition(identity != TemplateEditorID(service: "https://second.invalid", template: "same"))
        let decodedIdentity = try JSONDecoder().decode(TemplateEditorID.self, from: JSONEncoder().encode(identity))
        precondition(decodedIdentity == identity)
        let sibling = SubscriptionTemplateStore(); sibling.document = deletion.document; sibling.baseline = sibling.document
        let siblingBaseline = sibling.document
        deletion.set(["zero"], .integer(0)); deletion.undo()
        precondition(sibling.document == siblingBaseline && !sibling.dirty && sibling.undoStack.isEmpty)
        let nullDiff = SubscriptionTemplateStore.difference(.object([:]), .object(["future":.null]))
        precondition(nullDiff.arrayValue?.first?["operation"] == .string("add"))
        let previewConfiguration = URLSessionConfiguration.ephemeral; previewConfiguration.protocolClasses = [DelayedTemplatePreview.self]
        let previewAPI = APIClient(baseURL: URL(string: "https://preview.invalid")!, token: "fixture", session: URLSession(configuration: previewConfiguration))
        let previewModel = SubscriptionTemplateStore(); previewModel.connectedAPI = previewAPI; previewModel.selected = store.selected
        previewModel.document = sibling.document; previewModel.baseline = previewModel.document
        let oldPreview = Task { try await previewModel.render(previewAPI, user: "user", platform: "windows", version: "", reveal: false) }
        while DelayedTemplatePreview.started.withLock({ $0 }) == 0 { try await Task.sleep(for: .milliseconds(5)) }
        previewModel.set(["zero"], .integer(7))
        try await oldPreview.value
        precondition(previewModel.preview.isEmpty && previewModel.previewStale)
        try await previewModel.render(previewAPI, user: "user", platform: "windows", version: "", reveal: false)
        precondition(!previewModel.previewStale && previewModel.preview["source"] == .string("old preview"))
        previewModel.undo(); precondition(previewModel.previewStale && previewModel.preview.isEmpty)
        let suspendedDocument = previewModel.document
        previewModel.sessionAvailable = { _ in false }
        do { try await previewModel.save(previewAPI); preconditionFailure("suspended editor must not save") } catch is CancellationError { }
        precondition(previewModel.document == suspendedDocument)
        previewModel.sessionAvailable = nil; previewModel.closed = true
        do { try await previewModel.render(previewAPI, user: "user", platform: "windows", version: "", reveal: false); preconditionFailure("closed editor must not render") } catch is CancellationError { }
        let saveConfiguration = URLSessionConfiguration.ephemeral; saveConfiguration.protocolClasses = [TemplateSaveSession.self]
        let saveAPI = APIClient(baseURL: URL(string: "https://save.invalid")!, token: "fixture", session: URLSession(configuration: saveConfiguration))
        let savingModel = SubscriptionTemplateStore(); savingModel.connectedAPI = saveAPI; savingModel.selected = store.selected
        savingModel.document = sibling.document; savingModel.baseline = sibling.document
        savingModel.set(["zero"], .integer(5))
        try await savingModel.save(saveAPI)
        precondition(!savingModel.dirty && savingModel.selected?.revision == 2 && !savingModel.saving)
        let validDocument = savingModel.document
        savingModel.sourceMode = true; savingModel.sourceBaseline = "valid source"; savingModel.source = "["
        do { try await savingModel.save(saveAPI); preconditionFailure("invalid source must prevent saving and closing") } catch { }
        precondition(savingModel.source == "[" && savingModel.dirty && savingModel.sourceMode && savingModel.document == validDocument && !savingModel.saving)
        savingModel.sourceMode = false; savingModel.remoteConflict = sibling.document
        let beforeConflictPublication = TemplateSaveSession.requests.withLock { $0 }
        do { try await savingModel.preparePublish(saveAPI); preconditionFailure("known revision conflicts must block publication review") } catch APIClientError.server { }
        do { try await savingModel.publish(saveAPI); preconditionFailure("known revision conflicts must block publication") } catch APIClientError.server { }
        precondition(TemplateSaveSession.requests.withLock { $0 } == beforeConflictPublication)
        let offlineConfiguration = URLSessionConfiguration.ephemeral; offlineConfiguration.protocolClasses = [OfflineTemplateSession.self]
        let offlineAPI = APIClient(baseURL: URL(string: "https://offline.invalid")!, token: "fixture", session: URLSession(configuration: offlineConfiguration))
        let offlineModel = SubscriptionTemplateStore(); offlineModel.connectedAPI = offlineAPI; offlineModel.selected = store.selected
        offlineModel.document = sibling.document; offlineModel.baseline = sibling.document
        offlineModel.set(["zero"], .integer(9))
        let offlineDocument = offlineModel.document
        do { try await offlineModel.save(offlineAPI); preconditionFailure("offline save must fail") } catch { }
        precondition(offlineModel.dirty && offlineModel.document == offlineDocument && !offlineModel.saving)
        let failedSaveModel = SubscriptionTemplateStore(); failedSaveModel.connectedAPI = switchedAPI; failedSaveModel.selected = store.selected
        failedSaveModel.document = offlineDocument; failedSaveModel.baseline = sibling.document
        do { try await failedSaveModel.save(switchedAPI); preconditionFailure("revision conflict must fail") } catch { }
        precondition(failedSaveModel.document == offlineDocument && failedSaveModel.dirty && failedSaveModel.selected?.revision == 1)
        let reconnectingModel = SubscriptionTemplateStore()
        reconnectingModel.connectedAPI = originalAPI; reconnectingModel.sessionAvailable = { _ in false }
        let observedConnection = TemplateObservationFlag()
        withObservationTracking { _ = reconnectingModel.suspended } onChange: { observedConnection.value.withLock { $0 = true } }
        reconnectingModel.connectedAPI = switchedAPI
        precondition(observedConnection.value.withLock { $0 }, "reconnection must update the window's suspended state")
        print("Workbench identity, session isolation, structured null diff, suspension, and stale preview guards passed.")
        print("Template overlay semantics, whole-array overrides, undo/redo, unknown fields, and typed catalog coverage passed.")
    }
}
