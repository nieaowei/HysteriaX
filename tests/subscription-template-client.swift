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
        let rejectMethods: [JSONValue] = [.string(""), .string("default"), .string("drop"), .string("reply")]
        for current: JSONValue? in [nil, .string(""), .string("default")] {
            let options = TemplateEnumPickerOptions(values: rejectMethods, current: current, defaultValue: .string("default"), supportsUnset: true, emptyUsesDefault: true)
            precondition(!options.includesUnset && options.values == Array(rejectMethods.dropFirst()))
            precondition(options.selection == JSONValue.string("default").pretty)
        }
        let domainStrategies: [JSONValue] = [.string(""), .string("as_is"), .string("prefer_ipv4")]
        for current: JSONValue? in [nil, .string("")] {
            let options = TemplateEnumPickerOptions(values: domainStrategies + [.string("")], current: current, defaultValue: nil, supportsUnset: true, emptyUsesDefault: false)
            precondition(options.includesUnset && options.values == Array(domainStrategies.dropFirst()))
            precondition(options.selection == TemplateEnumPickerOptions.unsetToken)
        }
        let strategyOptions = TemplateEnumPickerOptions(values: [.string("default"), .string("hybrid")], current: nil, defaultValue: nil, supportsUnset: true, emptyUsesDefault: false)
        precondition(strategyOptions.includesUnset && strategyOptions.values.contains(.string("default")), "Inheriting a network strategy and explicitly selecting default must remain distinct")
        let explicitNull = TemplateEnumPickerOptions(values: rejectMethods, current: .null, defaultValue: .string("default"), supportsUnset: true, emptyUsesDefault: true)
        precondition(explicitNull.values.contains(.null) && explicitNull.selection == "null")
        let futureEnum = TemplateEnumPickerOptions(values: rejectMethods, current: .string("future"), defaultValue: .string("default"), supportsUnset: true, emptyUsesDefault: true)
        precondition(futureEnum.values.contains(.string("future")) && futureEnum.selection == JSONValue.string("future").pretty)
        let listItemOptions = TemplateEnumPickerOptions(values: domainStrategies, current: .string(""), defaultValue: nil, supportsUnset: false, emptyUsesDefault: false)
        precondition(!listItemOptions.includesUnset && listItemOptions.values == domainStrategies && listItemOptions.selection == JSONValue.string("").pretty)
        // Cancel nested editors without leaking their changes into undo/redo,
        // even after the bounded undo history has dropped its oldest entries.
        let cancellable = SubscriptionTemplateStore()
        cancellable.document = .object(["common": .object(["value": .integer(0)])])
        cancellable.set(["value"], .integer(1))
        cancellable.set(["value"], .integer(2))
        cancellable.undo()
        let outerCheckpoint = cancellable.editorCheckpoint()
        cancellable.set(["value"], .integer(3))
        let innerCheckpoint = cancellable.editorCheckpoint()
        cancellable.set(["value"], .integer(4))
        cancellable.restoreEditorCheckpoint(innerCheckpoint)
        precondition(cancellable.effective["value"] == .integer(3))
        for number in 5...110 { cancellable.set(["value"], .integer(number)) }
        cancellable.error = "temporary editor error"
        cancellable.restoreEditorCheckpoint(outerCheckpoint)
        precondition(cancellable.document == outerCheckpoint.document && cancellable.error == nil)
        precondition(cancellable.undoStack == outerCheckpoint.undoStack && cancellable.redoStack == outerCheckpoint.redoStack)
        cancellable.redo(); precondition(cancellable.effective["value"] == .integer(2))
        cancellable.undo(); cancellable.undo()
        precondition(cancellable.effective["value"] == .integer(0))
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
        precondition(listSchema.resolve(timeoutDefinition, value: .string("5m"))["type"] == .string("integer"))
        precondition(listSchema.variants(timeoutDefinition).isEmpty)
        precondition(listSchema.editorDefinition(timeoutUnion, path: ["other_timeout"])["type"] == .string("integer"))
        let durationOnly: JSONValue = .object(["type": .string("string")])
        precondition(listSchema.editorDefinition(durationOnly, path: ["udp_timeout"]) == durationOnly)
        for key in ["auto_redirect_input_mark", "auto_redirect_output_mark", "auto_redirect_reset_mark", "routing_mark", "default_mark", "future_mark"] {
            let markDefinition = listSchema.editorDefinition(timeoutUnion, path: ["inbounds", "0", key])
            precondition(listSchema.initial(markDefinition) == .string(""))
            precondition(listSchema.resolve(markDefinition, value: .integer(1234))["type"] == .string("string"))
            precondition(listSchema.variants(markDefinition).isEmpty)
        }
        precondition(TemplateSchema.integerValue("-9223372036854775808") == .integer(Int.min))
        precondition(TemplateSchema.integerValue(" 65536 ") == .integer(65536))
        precondition(TemplateSchema.integerValue("18446744073709551615") == .unsignedInteger(UInt64.max))
        for invalid in ["", "-", "64 MB", "5m", "NOERROR", "0x2023", "1.5", "1e3", "18446744073709551616"] {
            precondition(TemplateSchema.integerValue(invalid) == nil)
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
            verifyCascadeChanges(root: root, format: format)
            var count = 0
            var fixedScalarCount = 0
            func walk(_ value: JSONValue, depth: Int = 0, path: [String] = []) {
                guard depth < 60 else { return }
                let types = Set(catalog.variants(value).compactMap { catalog.resolve($0)["type"].stringValue })
                if types == ["integer", "string"] {
                    let edited = catalog.editorDefinition(value, path: path)
                    let expected = path.last?.hasSuffix("mark") == true ? "string" : "integer"
                    precondition(edited["type"] == .string(expected) && catalog.variants(edited).isEmpty, "Scalar union must have one fixed editor type: \(path)")
                    fixedScalarCount += 1
                }
                let enumValues = value["enum"].arrayValue ?? value["x-enum-choices"].arrayValue ?? []
                for choice in enumValues {
                    precondition(!TemplateSchema.pickerTitle(choice).trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, "Blank enum label: \(choice.pretty)")
                }
                if !enumValues.isEmpty {
                    for current: JSONValue? in [nil, .string("future-value"), .null] + enumValues.map(Optional.some) {
                        let options = TemplateEnumPickerOptions(values: enumValues, current: current, defaultValue: value.objectValue?["default"], supportsUnset: format == "singbox", emptyUsesDefault: false)
                        let tokens = options.values.map(\.pretty) + (options.includesUnset ? [TemplateEnumPickerOptions.unsetToken] : [])
                        precondition(Set(tokens).count == tokens.count && tokens.contains(options.selection), "Enum selection must match exactly one menu item: \(path)")
                        precondition(!options.includesUnset || !options.values.contains(.string("")), "Enum must not show a second unset choice: \(path)")
                    }
                }
                if let ref = value["$ref"].stringValue {
                    precondition(root.at(ref.dropFirst(2).components(separatedBy:"/")) != nil, "unresolved schema reference: \(ref)")
                }
                if let type = value["type"].stringValue {
                    precondition(["object","array","string","number","integer","boolean","null"].contains(type), "missing typed control: \(type)")
                    count += 1
                }
                for (key, child) in value.objectValue ?? [:] { walk(child, depth: depth + 1, path: path + [key]) }
                for (index, child) in (value.arrayValue ?? []).enumerated() { walk(child, depth: depth + 1, path: path + [String(index)]) }
            }
            walk(root)
            precondition(count > 100)
            print("\(format): fixed scalar policy checked across \(fixedScalarCount) schema locations")
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
                let tlsStore = SubscriptionTemplateStore()
                tlsStore.catalog = .object(["schema": root, "format": .string("singbox")])
                var tlsPositions = 0
                var automaticPositions = 0
                var dedicatedPositions = 0
                for (name, parentPath) in [("Inbound", ["inbounds", "0"]), ("Outbound", ["outbounds", "0"]), ("DNSServer", ["dns", "servers", "0"]), ("Endpoint", ["endpoints", "0"]), ("Service", ["services", "0"]), ("HTTPClient", ["http_clients", "0"])] {
                    let definition = root["$defs"][name]
                    let variants = catalog.variants(definition)
                    for variant in variants.isEmpty ? [definition] : variants {
                        let fields = catalog.resolve(variant)["properties"].objectValue ?? [:]
                        guard let tls = fields["tls"] else { continue }
                        let path = parentPath + ["tls"]
                        let tlsSchema = catalog.resolve(tls)
                        let hasSwitch = tlsSchema["properties"]["enabled"] != .null
                        let automatic = name == "DNSServer" || name == "HTTPClient"
                        precondition(TemplateEditorGuidance.isInlineObject(path, schema: catalog), "Every TLS main object must render inline")
                        let parameter = tlsSchema["properties"]["server_name"] == .null ? "certificate_path" : "server_name"
                        let cases: [JSONValue?] = [nil, .null, .object([:]), .object([parameter: .string("keep.example")]), .object(["enabled": .bool(false), parameter: .string("keep.example")]), .object(["enabled": .bool(true), parameter: .string("keep.example")]), .object(["enabled": .null, parameter: .string("keep.example")])]
                        for existing in cases {
                            var item = catalog.initial(variant)
                            item.set(["tls"], existing)
                            var config = item
                            for key in parentPath.reversed() { config = key == "0" ? .array([config]) : .object([key: config]) }
                            tlsStore.document = .object(["schema_version": .integer(2), "common": config])
                            let before = tlsStore.document
                            let enabledPath = path + ["enabled"]
                            let fallback = TemplateEditorGuidance.defaultValue(enabledPath, schema: catalog, value: config)
                            precondition(fallback == (hasSwitch ? .bool(automatic) : nil), "Incorrect TLS enabled default: \(name).\(item["type"].pretty)")
                            let visible = TemplateEditorGuidance.visibleKeys(tlsSchema, path: path, schema: catalog, value: config)
                            if automatic {
                                precondition(!visible.contains("enabled") && visible.contains(parameter), "Automatic TLS ignores even explicit enabled=false")
                            } else if hasSwitch && existing?["enabled"] != .bool(true) {
                                precondition(visible == ["enabled"], "Unset/off TLS must hide its parameters")
                            } else {
                                precondition(visible.contains(parameter), "Enabled or dedicated TLS must expose parameters")
                            }
                            if hasSwitch, existing?.objectValue?["enabled"] == nil {
                                precondition(tlsStore.editorValue(tlsSchema["properties"]["enabled"], path: enabledPath) == .bool(automatic))
                            }
                            precondition(tlsStore.document == before, "Default previews must not materialize TLS")
                        }
                        tlsPositions += 1
                        if automatic { automaticPositions += 1 }
                        if !hasSwitch { dedicatedPositions += 1 }
                    }
                }
                let inlineHTTP: JSONValue = .object(["route": .object(["rule_set": .array([.object(["type": .string("remote"), "http_client": .object(["version": .integer(3), "tls": .object(["enabled": .bool(false), "server_name": .string("keep.example")])])])])])])
                let inlineTLSPath = ["route", "rule_set", "0", "http_client", "tls"]
                let inlineTLSSchema = catalog.resolve(catalog.definition(at: inlineTLSPath, value: inlineHTTP))
                precondition(TemplateEditorGuidance.defaultValue(inlineTLSPath + ["enabled"], schema: catalog, value: inlineHTTP) == .bool(true))
                precondition(TemplateEditorGuidance.visibleKeys(inlineTLSSchema, path: inlineTLSPath, schema: catalog, value: inlineHTTP).contains("server_name"))
                precondition(!TemplateEditorGuidance.visibleKeys(inlineTLSSchema, path: inlineTLSPath, schema: catalog, value: inlineHTTP).contains("enabled"))
                for (family, prefix, children) in [("vless", ["inbounds", "0"], ["ech", "reality"]), ("vless", ["outbounds", "0"], ["ech", "utls", "reality"])] {
                    var config: JSONValue = .object(["type": .string(family), "tls": .object(["enabled": .bool(true)])])
                    for key in prefix.reversed() { config = key == "0" ? .array([config]) : .object([key: config]) }
                    for child in children {
                        let path = prefix + ["tls", child]
                        let childSchema = catalog.resolve(catalog.definition(at: path, value: config))
                        precondition(TemplateEditorGuidance.defaultValue(path + ["enabled"], schema: catalog, value: config) == .bool(false))
                        precondition(TemplateEditorGuidance.visibleKeys(childSchema, path: path, schema: catalog, value: config) == ["enabled"])
                    }
                }
                print("TLS main objects audited: \(tlsPositions) positions; automatic=\(automaticPositions), dedicated without enabled=\(dedicatedPositions); inline HTTP and five nested switches passed")
                let typeUnions = ["CertificateProvider", "CertificateProviderReference", "DNSServer", "Endpoint", "Inbound", "Outbound", "Service", "V2RayTransport", "Rule", "DNSRule", "NestedRule", "NestedDNSRule", "HeadlessRule", "NetworkNamespace", "RuleSet"]
                let queryDefinition = catalog.resolve(root["$defs"]["DNSRule"])["properties"]["query_type"]
                let queryEditor = catalog.editorDefinition(queryDefinition, path: ["dns", "rules", "0", "query_type"])
                precondition(catalog.variants(queryEditor).map { catalog.resolve($0)["type"].stringValue ?? "" } == ["integer", "array"], "query_type must retain its integer/array selector")
                let objectUnions = Set((root["$defs"].objectValue ?? [:]).compactMap { name, definition -> String? in
                    let choices = catalog.variants(definition)
                    return choices.count > 1 && choices.contains { catalog.resolve($0)["type"].stringValue == "object" } ? name : nil
                })
                precondition(objectUnions == Set(typeUnions + ["ACMEProviderDNS01Challenge", "RuleAction", "DNSRuleAction", "HTTPClientReference", "DomainResolver"]), "Every object union must participate in the duplicate-rendering audit")
                var auditedVariants = 0
                for name in typeUnions + ["ACMEProviderDNS01Challenge", "RuleAction", "DNSRuleAction"] {
                    let definition = root["$defs"][name]
                    for variant in catalog.variants(definition) where catalog.resolve(variant)["type"].stringValue == "object" {
                        let initial = catalog.initial(variant)
                        let selected = catalog.resolve(definition, value: initial)
                        var expected: Set<String> = [name == "ACMEProviderDNS01Challenge" ? "provider" : name.hasSuffix("Action") ? "action" : "type"]
                        if initial["type"] == .string("snell") { expected.insert("version") }
                        precondition(catalog.variantFieldKeys(definition, value: initial) == expected, "Variant controls must own exactly their discriminators: \(name), \(initial.pretty)")
                        let editable = Set((selected["properties"].objectValue ?? [:]).keys).subtracting(expected)
                        if name == "CertificateProvider", initial["type"] == .string("acme") {
                            precondition(editable.contains("provider"), "ACME's server provider is distinct from the certificate provider type")
                        }
                        if name == "Endpoint", initial["type"] == .string("openconnect") {
                            precondition(editable.contains("version"), "OpenConnect version must remain independently editable")
                        }
                        if ["Rule", "DNSRule"].contains(name), initial["type"] != .string("logical") {
                            precondition(editable.contains("protocol") && editable.contains("action"), "Matching protocol and action are independent of rule type")
                        }
                        auditedVariants += 1
                    }
                }
                let httpReference = root["$defs"]["HTTPClientReference"]
                precondition(catalog.variantFieldKeys(httpReference, value: .object(["version": .integer(3)])).isEmpty, "Shared/custom selection must not consume HTTP version")
                precondition(catalog.variantFieldKeys(httpReference, value: .string("shared")).isEmpty)
                precondition(catalog.variantFieldKeys(root["$defs"]["DomainResolver"], value: .object(["server": .string("dns")])).isEmpty)
                precondition(catalog.variantFieldKeys(root["$defs"]["HTTPClient"], value: .object([:])).isEmpty, "A single object variant has no discriminator picker")
                print("Duplicate rendering ownership audited: \(typeUnions.count + 3) union definitions, \(auditedVariants) object variants")
                for key in (root["properties"].objectValue ?? [:]).keys {
                    precondition(TemplateEditorGuidance.documentURL([key], schema: catalog, value: .object([:]))?.path != "/zh/configuration", "Missing root documentation mapping: \(key)")
                }
                var guidanceValue: JSONValue = .object([
                    "dns": .object(["servers": .array([.object(["type": .string("https"), "tls": .object([:])])])]),
                    "outbounds": .array([.object(["type": .string("vless"), "tls": .object(["enabled": .bool(false), "server_name": .string("keep.example")])])])
                ])
                let dnsTLSPath = ["dns", "servers", "0", "tls"]
                let outboundTLSPath = ["outbounds", "0", "tls"]
                let tlsDefinition = catalog.resolve(root["$defs"]["OutboundTLSOptions"])
                precondition(catalog.definition(at: ["dns", "servers", "0"], value: guidanceValue) == .object(["$ref": .string("#/$defs/DNSServer")]))
                precondition(TemplateEditorGuidance.visibleKeys(tlsDefinition, path: outboundTLSPath, schema: catalog, value: guidanceValue) == ["enabled"])
                let automaticTLSFields = TemplateEditorGuidance.visibleKeys(tlsDefinition, path: dnsTLSPath, schema: catalog, value: guidanceValue)
                precondition(!automaticTLSFields.contains("enabled") && automaticTLSFields.contains("server_name"))
                precondition(TemplateEditorGuidance.section("server", path: ["dns", "rules", "0"], schema: catalog, value: guidanceValue) == "规则动作")
                precondition(TemplateEditorGuidance.section("domain_suffix", path: ["route", "rules", "0"], schema: catalog, value: guidanceValue) == "域名与规则集匹配")
                precondition(TemplateEditorGuidance.section("url", path: ["route", "rule_set", "0"], schema: catalog, value: guidanceValue) == "规则集来源")
                precondition(TemplateEditorGuidance.documentURL(["dns", "servers", "0"], schema: catalog, value: guidanceValue)?.path == "/zh/configuration/dns/server/https")
                precondition(TemplateEditorGuidance.documentURL(["outbounds", "0", "detour"], schema: catalog, value: guidanceValue)?.fragment == "detour")
                let guidedStore = SubscriptionTemplateStore()
                guidedStore.catalog = .object(["schema": root, "format": .string("singbox")])
                guidedStore.document = .object(["schema_version": .integer(2), "common": guidanceValue])
                let beforeDefaultPreview = guidedStore.document
                precondition(guidedStore.editorValue(.object(["type": .string("integer")]), path: ["dns", "servers", "0", "server_port"]) == .integer(443))
                precondition(guidedStore.document == beforeDefaultPreview)
                guidanceValue.set(["outbounds", "0", "tls", "enabled"], .bool(true))
                precondition(TemplateEditorGuidance.visibleKeys(tlsDefinition, path: outboundTLSPath, schema: catalog, value: guidanceValue).contains("server_name"))
                guidanceValue.set(["outbounds", "0", "disable_tcp_keep_alive"], .bool(true))
                let outboundDefinition = catalog.resolve(root["$defs"]["Outbound"], value: guidanceValue["outbounds"].arrayValue![0])
                let visibleDialer = TemplateEditorGuidance.visibleKeys(outboundDefinition, path: ["outbounds", "0"], schema: catalog, value: guidanceValue)
                precondition(!visibleDialer.contains("tcp_keep_alive") && !visibleDialer.contains("tcp_keep_alive_interval"))
                guidanceValue.set(["outbounds", "0", "network_strategy"], .string("fallback"))
                precondition(TemplateEditorGuidance.visibleKeys(outboundDefinition, path: ["outbounds", "0"], schema: catalog, value: guidanceValue).contains("fallback_network_type"))
                guidanceValue.set(["outbounds", "0", "detour"], .string("proxy"))
                let detouredFields = TemplateEditorGuidance.visibleKeys(outboundDefinition, path: ["outbounds", "0"], schema: catalog, value: guidanceValue)
                precondition(detouredFields.contains("detour") && !detouredFields.contains("connect_timeout"))
                let certificateChoices = catalog.variants(root["$defs"]["CertificateProviderReference"])
                precondition(certificateChoices.count == 4)
                precondition(Set(certificateChoices.enumerated().map { catalog.variantTitle($0.element, index: $0.offset) }).count == 4)
                let outboundChoices = catalog.variants(root["$defs"]["Outbound"])
                let snellChoices = outboundChoices.filter { catalog.resolve($0)["properties"]["type"]["const"] == .string("snell") }
                precondition(snellChoices.count == 2 && Set(snellChoices.map { catalog.variantTitle($0, index: 0) }).count == 2)
                let ruleSummary = TemplateEditorGuidance.summary(.object(["domain_suffix": .array([.string("example.com")]), "action": .string("route"), "outbound": .string("proxy"), "private_key": .string("must-not-show")]), path: ["route", "rules"])!
                precondition(ruleSummary.contains("example.com") && ruleSummary.contains("proxy") && !ruleSummary.contains("must-not-show"))
                // The documentation registry covers protocols, not just the HTTP Client form.
                precondition(TemplateDocumentationOrder.pages.count >= 100)
                for fields in TemplateDocumentationOrder.pages.values {
                    let keys = fields.split(separator: " ")
                    precondition(keys.count == Set(keys).count)
                }
                for (name, key) in [("Inbound", "inbounds"), ("Outbound", "outbounds"), ("Endpoint", "endpoints"), ("Service", "services"), ("CertificateProvider", "certificate_providers"), ("NetworkNamespace", "network_namespaces")] {
                    for variant in catalog.variants(root["$defs"][name]) {
                        let initial = catalog.initial(variant)
                        let config: JSONValue = .object([key: .array([initial])])
                        let path = [key, "0"]
                        let resolved = catalog.resolve(variant, value: initial)
                        let keys = Array((resolved["properties"].objectValue ?? [:]).keys)
                        let ordered = TemplateEditorGuidance.ordered(keys, path: path, schema: catalog, value: config)
                        precondition(ordered.count == keys.count && Set(ordered) == Set(keys), "Documentation ordering must never lose fields: \(name)")
                        precondition(TemplateEditorGuidance.documentURL(path, schema: catalog, value: config) != nil)
                    }
                }
                func visibleOptions(_ key: String, _ object: JSONValue) -> [String] {
                    let config: JSONValue = .object([key: .array([object])])
                    let path = [key, "0"]
                    return TemplateEditorGuidance.visibleKeys(catalog.resolve(catalog.definition(at: path, value: config), value: object), path: path, schema: catalog, value: config)
                }
                var tun: JSONValue = .object(["type": .string("tun"), "route_address": .array([.string("192.0.2.0/24")]), "auto_redirect_input_mark": .string("0x1234")])
                precondition(!visibleOptions("inbounds", tun).contains("auto_redirect"))
                tun["auto_route"] = .bool(true)
                precondition(visibleOptions("inbounds", tun).contains("auto_redirect"))
                precondition(!visibleOptions("inbounds", tun).contains("auto_redirect_input_mark"))
                tun["auto_redirect"] = .bool(true)
                precondition(visibleOptions("inbounds", tun).contains("auto_redirect_input_mark"))
                tun["dns_mode"] = .string("disabled")
                precondition(!visibleOptions("inbounds", tun).contains("dns_address"))
                let tunConfig: JSONValue = .object(["inbounds": .array([tun])])
                precondition(TemplateEditorGuidance.section("auto_redirect_input_mark", path: ["inbounds", "0"], schema: catalog, value: tunConfig) == "自动重定向")
                precondition(TemplateEditorGuidance.defaultValue(["inbounds", "0", "auto_redirect_input_mark"], schema: catalog, value: tunConfig) == .string("0x2023"))
                let listener: JSONValue = .object(["type": .string("mixed"), "detour": .string("injected"), "bind_interface": .string("en0")])
                precondition(visibleOptions("inbounds", listener).contains("bind_interface"), "Listen detour forwards to an inbound; it does not ignore listen options")
                precondition(TemplateEditorGuidance.documentURL(["inbounds", "0", "detour"], schema: catalog, value: .object(["inbounds": .array([listener])]))?.path == "/zh/configuration/shared/listen")
                // Documentation follows field ownership, including shared fields and nested headings.
                let docsConfig: JSONValue = .object([
                    "inbounds": .array([tun, listener]),
                    "endpoints": .array([.object(["type": .string("openvpn-client"), "tls": .object(["control_wrap": .object(["type": .string("tls-auth")])])])]),
                    "outbounds": .array([.object(["type": .string("urltest")]), .object(["type": .string("vless"), "domain_resolver": .object(["server": .string("dns")])])]),
                    "http_clients": .array([.object(["tag": .string("web")])]),
                    "route": .object(["rules": .array([.object(["action": .string("route"), "override_port": .integer(443)])])])
                ])
                let documentationCases: [([String], String, String?)] = [
                    (["inbounds", "0", "netns"], "inbound/tun", "netns"),
                    (["inbounds", "1", "listen_port"], "shared/listen", "listen_port"),
                    (["inbounds", "1", "detour"], "shared/listen", "detour"),
                    (["inbounds", "0", "udp_mapping"], "shared/udp-nat", "udp_mapping"),
                    (["endpoints", "0", "tls", "control_wrap", "key"], "endpoint/openvpn-client", "tlscontrol_wrapkey"),
                    (["outbounds", "0", "idle_timeout"], "outbound/urltest", "idle_timeout"),
                    (["outbounds", "1", "domain_resolver", "server"], "dns/rule_action", nil),
                    (["http_clients", "0", "idle_timeout"], "shared/http2", "idle_timeout"),
                    (["route", "rules", "0", "override_port"], "route/rule_action", "override_port"),
                    (["route", "rules", "0", "port"], "route/rule", "port"),
                    (["route", "rules", "0", "network_type"], "route/rule", "network_type")
                ]
                for (path, page, fragment) in documentationCases {
                    let url = TemplateEditorGuidance.documentURL(path, schema: catalog, value: docsConfig)
                    precondition(url?.path == "/zh/configuration/" + page && url?.fragment == fragment, "Incorrect documentation for \(path): \(String(describing: url))")
                }
                precondition(TemplateEditorGuidance.help(["route", "rules", "0", "network_strategy"], schema: catalog, value: docsConfig) == nil, "Rule action help must not inherit dialer-only restrictions")
                precondition(TemplateEditorGuidance.help(["outbounds", "1", "network_strategy"], schema: catalog, value: docsConfig) != nil)
                precondition(TemplateEditorGuidance.documentURL(["inbounds", "0", "future_option"], schema: catalog, value: docsConfig)?.fragment == nil)
                // Every literal metadata entry must name a real property with a compatible native type.
                var defaultCount = 0
                for (context, defaults) in TemplateDefaultCatalog.literals {
                    let parts = context.split(separator: ":", maxSplits: 1).map(String.init)
                    let name = parts[0], kind = parts.count > 1 ? parts[1] : ""
                    let object: JSONValue = kind.isEmpty ? .object([:]) : .object(["type": .string(kind)])
                    let definition = catalog.resolve(root["$defs"][name], value: object)
                    for (key, value) in defaults {
                        let field = definition["properties"][key]
                        precondition(field != .null, "Default metadata names a nonexistent field: \(context).\(key)")
                        let resolved = catalog.resolve(field, value: value)
                        precondition(resolved["type"].stringValue == value.schemaType, "Default metadata has incorrect type: \(context).\(key)")
                        if let choices = resolved["enum"].arrayValue {
                            precondition(choices.contains(value), "Default metadata outside enum: \(context).\(key)")
                        }
                        defaultCount += 1
                    }
                }
                let defaultCases: [([String], JSONValue, JSONValue)] = [
                    (["endpoints", "0", "tls", "version_min"], .object(["endpoints": .array([.object(["type": .string("openvpn-client")])])]), .string("1.2")),
                    (["endpoints", "0", "tls", "remote_certificate_tls"], .object(["endpoints": .array([.object(["type": .string("openvpn-server")])])]), .string("client")),
                    (["endpoints", "0", "max_clients"], .object(["endpoints": .array([.object(["type": .string("openvpn-server")])])]), .integer(1024)),
                    (["endpoints", "0", "max_clients"], .object(["endpoints": .array([.object(["type": .string("openvpn-server"), "mode": .string("static_key")])])]), .integer(1)),
                    (["endpoints", "0", "topology"], .object(["endpoints": .array([.object(["type": .string("openvpn-server"), "mode": .string("static_key")])])]), .string("p2p")),
                    (["endpoints", "0", "queue_length"], .object(["endpoints": .array([.object(["type": .string("openconnect")])])]), .integer(32)),
                    (["inbounds", "0", "heartbeat"], .object(["inbounds": .array([.object(["type": .string("tuic")])])]), .string("10s")),
                    (["inbounds", "0", "udp_timeout"], .object(["inbounds": .array([.object(["type": .string("mixed")])])]), .integer(300)),
                    (["endpoints", "0", "udp_timeout"], .object(["endpoints": .array([.object(["type": .string("wireguard")])])]), .string("5m")),
                    (["services", "0", "server_port"], .object(["services": .array([.object(["type": .string("usbip-client")])])]), .integer(3240)),
                    (["services", "0", "stun", "listen_port"], .object(["services": .array([.object(["type": .string("derp"), "stun": .object([:])])])]), .integer(3478)),
                    (["services", "0", "dashboard", "update_interval"], .object(["services": .array([.object(["type": .string("api"), "dashboard": .object([:])])])]), .string("1d")),
                    (["log", "level"], .object([:]), .string("trace")),
                    (["certificate", "store"], .object([:]), .string("system")),
                    (["experimental", "clash_api", "access_control_allow_origin"], .object([:]), .array([.string("*")]))
                ]
                for (path, config, expected) in defaultCases {
                    let preview = SubscriptionTemplateStore()
                    preview.catalog = .object(["schema": root, "format": .string("singbox")])
                    preview.document = .object(["schema_version": .integer(2), "common": config])
                    let baseline = preview.document
                    let field = catalog.definition(at: path, value: config)
                    precondition(preview.editorValue(field, path: path) == expected, "Wrong native default: \(path)")
                    precondition(preview.document == baseline && preview.undoStack.isEmpty)
                    preview.setState("set", path: path, schema: field)
                    precondition(preview.effective.at(path) == expected, "Explicit set must use the displayed default: \(path)")
                    preview.undo(); precondition(preview.document == baseline)
                    preview.set(path, .null); precondition(preview.editorValue(field, path: path) == .null)
                    preview.undo(); precondition(preview.document == baseline)
                }
                let inheritedNetwork: JSONValue = .object(["endpoints": .array([.object(["type": .string("openvpn-client"), "network": .string("tcp"), "servers": .array([.object(["server": .string("vpn.example")])])])])])
                precondition(TemplateEditorGuidance.defaultValue(["endpoints", "0", "servers", "0", "network"], schema: catalog, value: inheritedNetwork) == .string("tcp"))
                let algorithms: JSONValue = .object(["endpoints": .array([.object(["type": .string("openvpn-client")])])])
                precondition(TemplateEditorGuidance.defaultValue(["endpoints", "0", "data_ciphers"], schema: catalog, value: algorithms) == .array([.string("AES-256-GCM"), .string("AES-128-GCM"), .string("CHACHA20-POLY1305")]))
                let dnsActions: JSONValue = .object(["dns": .object(["rules": .array([.object(["action": .string("predefined")]), .object(["action": .string("reject")])])])])
                precondition(TemplateEditorGuidance.defaultValue(["dns", "rules", "0", "rcode"], schema: catalog, value: dnsActions) == .integer(0))
                precondition(TemplateEditorGuidance.defaultValue(["dns", "rules", "1", "method"], schema: catalog, value: dnsActions) == .string("default"))
                let inheritedTimeout: JSONValue = .object(["dns": .object(["timeout": .string("25s"), "rules": .array([.object(["action": .string("route"), "server": .string("dns")])])])])
                precondition(TemplateEditorGuidance.defaultValue(["dns", "rules", "0", "timeout"], schema: catalog, value: inheritedTimeout) == .string("25s"))
                let emptyOrigins = SubscriptionTemplateStore()
                emptyOrigins.catalog = .object(["schema": root, "format": .string("singbox")])
                emptyOrigins.document = .object(["schema_version": .integer(2), "common": .object(["experimental": .object(["clash_api": .object(["access_control_allow_origin": .array([])])])])])
                let emptyOriginsPath = ["experimental", "clash_api", "access_control_allow_origin"]
                precondition(emptyOrigins.editorValue(catalog.definition(at: emptyOriginsPath, value: emptyOrigins.effective), path: emptyOriginsPath) == .array([]))
                let dynamicVPN: JSONValue = .object(["endpoints": .array([.object(["type": .string("openvpn-client")])])])
                precondition(TemplateEditorGuidance.defaultValue(["endpoints", "0", "tls", "version_max"], schema: catalog, value: dynamicVPN) == nil)
                precondition(TemplateEditorGuidance.help(["endpoints", "0", "tls", "version_max"], schema: catalog, value: dynamicVPN) != nil)
                precondition(TemplateEditorGuidance.defaultValue(["endpoints", "0", "topology"], schema: catalog, value: dynamicVPN) == nil)
                precondition(TemplateEditorGuidance.defaultValue(["endpoints", "0", "nonexistent"], schema: catalog, value: dynamicVPN) == nil)
                print("Versioned literal defaults checked: \(defaultCount); contextual preview/set/null/undo regressions passed")
                // Documentation anchors must handle grouped titles and ambiguous repeated headings.
                let grouped: JSONValue = .object(["inbounds": .array([.object(["type": .string("hysteria2")])])])
                for key in ["up_mbps", "down_mbps"] {
                    precondition(TemplateEditorGuidance.documentURL(["inbounds", "0", key], schema: catalog, value: grouped)?.fragment == "up_mbps-down_mbps")
                }
                let uot: JSONValue = .object(["outbounds": .array([.object(["type": .string("shadowsocks"), "udp_over_tcp": .object(["enabled": .bool(true)])])])])
                precondition(TemplateEditorGuidance.documentURL(["outbounds", "0", "udp_over_tcp", "version"], schema: catalog, value: uot)?.path == "/zh/configuration/shared/udp-over-tcp")
                let uotPreview = SubscriptionTemplateStore()
                uotPreview.catalog = .object(["schema": root, "format": .string("singbox")])
                uotPreview.document = .object(["schema_version": .integer(2), "common": uot])
                let previewBaseline = uotPreview.document
                let uotVersionPath = ["outbounds", "0", "udp_over_tcp", "version"]
                let uotVersionDefinition = catalog.definition(at: uotVersionPath, value: uot)
                precondition(uotPreview.editorValue(uotVersionDefinition, path: uotVersionPath) == .integer(2), "UoT defaults to version 2, not the first enum choice")
                precondition(uotPreview.document == previewBaseline)
                uotPreview.set(uotVersionPath, .integer(1))
                precondition(uotPreview.editorValue(uotVersionDefinition, path: uotVersionPath) == .integer(1))
                uotPreview.undo(); precondition(uotPreview.document == previewBaseline)
                let documentedDefaults: [([String], JSONValue, JSONValue)] = [
                    (["dns", "timeout"], .object([:]), .string("10s")),
                    (["experimental", "cache_file", "path"], .object([:]), .string("cache.db")),
                    (["route", "rule_set", "0", "update_interval"], .object(["route": .object(["rule_set": .array([.object(["type": .string("remote")])])])]), .string("1d")),
                    (["endpoints", "0", "mtu"], .object(["endpoints": .array([.object(["type": .string("wireguard")])])]), .integer(1408))
                ]
                for (path, config, expected) in documentedDefaults {
                    let previewStore = SubscriptionTemplateStore()
                    previewStore.catalog = .object(["schema": root, "format": .string("singbox")])
                    previewStore.document = .object(["schema_version": .integer(2), "common": config])
                    let before = previewStore.document
                    let field = catalog.definition(at: path, value: config)
                    precondition(previewStore.editorValue(field, path: path) == expected)
                    precondition(previewStore.document == before)
                }
                precondition(TemplateDocumentationOrder.anchors["shared/multiplex"]?["enabled"] == "")
                precondition(!TemplateDocumentationOrder.pages.keys.contains { $0.contains(".zh") })
                // MAC filters need both routing switches; enabling routing alone is insufficient.
                var macTun = tun
                macTun["auto_redirect"] = .bool(false)
                precondition(!visibleOptions("inbounds", macTun).contains("include_mac_address"))
                macTun["auto_redirect"] = .bool(true)
                precondition(visibleOptions("inbounds", macTun).contains("include_mac_address"))
                for kind in ["openvpn-client", "openvpn-server"] {
                    let vpnStatic: JSONValue = .object(["type": .string(kind), "mode": .string("static_key"), "tls": .object(["server_name": .string("retained.example")])])
                    let options = visibleOptions("endpoints", vpnStatic)
                    precondition(options.contains("static_key") && !options.contains("tls") && !options.contains("renegotiate_interval"))
                    let config: JSONValue = .object(["endpoints": .array([vpnStatic])])
                    let definition = catalog.resolve(root["$defs"]["Endpoint"], value: vpnStatic)
                    precondition(TemplateEditorGuidance.hiddenConfiguredKeys(definition, path: ["endpoints", "0"], schema: catalog, value: config).contains("tls"))
                    precondition(config["endpoints"].arrayValue![0]["tls"]["server_name"] == .string("retained.example"))
                    precondition(!visibleOptions("endpoints", .object(["type": .string(kind)])).contains("static_key"))
                }
                let usb: JSONValue = .object(["type": .string("usbip-client"), "bind_interface": .string("en0"), "detour": .string("proxy")])
                precondition(visibleOptions("services", usb).contains("detour") && !visibleOptions("services", usb).contains("bind_interface"))
                for engine in ["apple", "windows"] {
                    let tls: JSONValue = .object(["enabled": .bool(true), "engine": .string(engine), "client_key_path": .string("retained.key"), "alpn": .array([.string("h2")])])
                    let config: JSONValue = .object(["outbounds": .array([.object(["type": .string("vless"), "tls": tls])])])
                    let options = TemplateEditorGuidance.visibleKeys(tlsDefinition, path: outboundTLSPath, schema: catalog, value: config)
                    precondition(options.contains("engine") && options.contains("alpn") && !options.contains("client_key_path"))
                    precondition(TemplateEditorGuidance.hiddenConfiguredKeys(tlsDefinition, path: outboundTLSPath, schema: catalog, value: config) == ["client_key_path"])
                }
                let openconnectConfig: JSONValue = .object(["endpoints": .array([.object(["type": .string("openconnect")])])])
                precondition(TemplateEditorGuidance.defaultValue(["endpoints", "0", "tcp_keep_alive"], schema: catalog, value: openconnectConfig) == nil, "OpenConnect retains OS keepalive timing when unset")
                var tailscale: JSONValue = .object(["type": .string("tailscale"), "system_interface_name": .string("ts0")])
                precondition(!visibleOptions("endpoints", tailscale).contains("system_interface_name"))
                tailscale["system_interface"] = .bool(true)
                precondition(visibleOptions("endpoints", tailscale).contains("system_interface_name"))
                var vpn: JSONValue = .object(["type": .string("openvpn-client"), "ping_restart_disabled": .bool(true), "ping_restart": .string("60s"), "renegotiate_disabled": .bool(true), "mss_fix_disabled": .bool(true)])
                precondition(Set(visibleOptions("endpoints", vpn)).isDisjoint(with: ["ping_restart", "renegotiate_interval", "mss_fix", "mss_fix_mode"]))
                vpn["ping_restart_disabled"] = .bool(false)
                precondition(visibleOptions("endpoints", vpn).contains("ping_restart"))
                var acme: JSONValue = .object(["type": .string("acme"), "disable_http_challenge": .bool(true), "alternative_http_port": .integer(8080)])
                precondition(!visibleOptions("certificate_providers", acme).contains("alternative_http_port"))
                acme["disable_http_challenge"] = .bool(false)
                precondition(visibleOptions("certificate_providers", acme).contains("alternative_http_port"))
                acme["dns01_challenge"] = .object(["provider": .string("cloudflare"), "api_token": .string("preserve-token")])
                precondition(!visibleOptions("certificate_providers", acme).contains("disable_http_challenge"))
                let actionRule: JSONValue = .object(["route": .object(["rules": .array([.object(["action": .string("route"), "override_port": .integer(443), "network_type": .array([.string("wifi")])])])])])
                precondition(TemplateEditorGuidance.section("override_port", path: ["route", "rules", "0"], schema: catalog, value: actionRule) == "规则动作")
                let vpnOrder = TemplateEditorGuidance.ordered(["auth", "data_ciphers", "data_ciphers_fallback", "cipher"], path: ["endpoints", "0"], schema: catalog, value: .object(["endpoints": .array([vpn])]))
                precondition(vpnOrder == ["cipher", "data_ciphers", "data_ciphers_fallback", "auth"], "OpenVPN controls follow the documentation instead of alphabetical order")
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
                let appleClient: JSONValue = .object([
                    "tag": .string("client"), "engine": .string("apple"), "version": .integer(3),
                    "idle_timeout": .string("30s"), "detour": .string("proxy"), "headers": .object(["Host": .string("example.com")]),
                    "future_option": .bool(true),
                    "tls": .object(["server_name": .string("example.com"), "alpn": .array([.string("h3")])])
                ])
                let clientTLS = catalog.resolve(clientDefinition["properties"]["tls"])
                let tlsFields = Set((clientTLS["properties"].objectValue ?? [:]).keys).subtracting(["enabled"])
                let goSections = TemplateHTTPClientLayout.tlsSections(clientTLS, value: .object([:]))
                let goFields = goSections.flatMap(\.keys)
                precondition(Set(goFields) == tlsFields && goFields.count == tlsFields.count, "HTTP Client must expose each supported Go TLS field exactly once")
                precondition(goSections.contains { $0.keys == ["ech"] } && goSections.contains { $0.keys == ["utls"] } && goSections.contains { $0.keys == ["reality"] })
                var futureTLS = clientTLS
                futureTLS["properties"]["future_tls_field"] = .object(["type": .string("string")])
                precondition(TemplateHTTPClientLayout.tlsSections(futureTLS, value: .object([:])).flatMap(\.keys).contains("future_tls_field"), "New schema fields must receive an editor without a hard-coded allowlist update")
                for engine in ["apple", "windows"] {
                    let systemClient: JSONValue = .object(["tls": .object(["engine": .string(engine), "alpn": .array([.string("h2")]), "client_key_path": .string("client.key")])])
                    let fields = TemplateHTTPClientLayout.tlsSections(clientTLS, value: systemClient).flatMap(\.keys)
                    precondition(fields.contains("engine") && fields.contains("alpn") && !fields.contains("client_key_path"))
                    precondition(TemplateHTTPClientLayout.incompatiblePaths(systemClient, tlsSchema: clientTLS) == [["tls", "client_key_path"]])
                    let cleared = TemplateHTTPClientLayout.clearingIncompatible(systemClient, tlsSchema: clientTLS)
                    precondition(cleared["tls"]["engine"] == .string(engine) && cleared["tls"]["alpn"] == .array([.string("h2")]))
                }
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
                for (name, actionName, count) in [("Rule", "RuleAction", 8), ("DNSRule", "DNSRuleAction", 6)] {
                    let definition: JSONValue = .object(["$ref": .string("#/$defs/\(name)")])
                    let actions = catalog.actionDefinition(definition)!
                    precondition(actions == root["$defs"][actionName])
                    let variants = catalog.variants(actions)
                    precondition(variants.count == count)
                    for logical in [false, true] {
                        let conditions: [String: JSONValue] = logical
                            ? ["type": .string("logical"), "mode": .string("and"), "rules": .array([.object(["domain": .string("keep.example")])])]
                            : ["domain": .array([.string("keep.example")]), "invert": .bool(true), "network_type": .array([.string("wifi")])]
                        for source in variants {
                            let sourceFields = catalog.resolve(source)["properties"].objectValue!
                            var original = conditions
                            for (key, field) in sourceFields where original[key] == nil { original[key] = catalog.initial(field) }
                            original["future_field"] = .string("keep")
                            for target in variants {
                                let targetFields = catalog.resolve(target)["properties"].objectValue!
                                let action = targetFields["action"]!["const"].stringValue!
                                let changed = catalog.selectingAction(action, from: definition, value: .object(original))
                                precondition(changed["action"] == .string(action))
                                for (key, expected) in conditions { precondition(changed[key] == expected, "Action changes must preserve matching conditions") }
                                precondition(changed["future_field"] == .string("keep"))
                                for key in sourceFields.keys where key != "action" {
                                    precondition(changed.objectValue?[key] == (targetFields[key] == nil && conditions[key] == nil ? nil : original[key]), "Action changes must retain shared parameters and remove obsolete ones: \(name).\(key)")
                                }
                            }
                        }
                    }
                    let routeKey = name == "Rule" ? "outbound" : "server"
                    for action in [JSONValue.null, .string("")] {
                        var implicit: JSONValue = .object([routeKey: .string("old"), "domain": .string("keep.example")])
                        if action != .null { implicit["action"] = action }
                        let changed = catalog.selectingAction("reject", from: definition, value: implicit)
                        precondition(changed.objectValue?[routeKey] == nil && changed["domain"] == implicit["domain"])
                    }
                    let unknown: JSONValue = .object(["action": .string("future-action"), routeKey: .string("keep")])
                    precondition(catalog.selectingAction("reject", from: definition, value: unknown)[routeKey] == .string("keep"))
                    precondition(catalog.selectingAction("invalid-action", from: definition, value: unknown) == unknown)
                }
                for name in ["NestedRule", "NestedDNSRule", "HeadlessRule"] {
                    precondition(catalog.actionDefinition(root["$defs"][name]) == nil, "Matching-only rules must not offer actions")
                }
                print("All route/DNS action transitions preserve conditions, shared parameters and extension fields: passed")
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
        precondition(deletion.document == beforeDeletion && deletion.pendingDeletion?.changes.contains(where: { $0.contains("/rules/0") }) == true)
        deletion.pendingDeletion = nil
        deletion.document.set(["platforms","windows","set","rules"], .array([.string("MATCH,DIRECT")]))
        deletion.layer = "windows"; deletion.deleteField(parent: ["rule-providers"], key: "A")
        precondition(deletion.effective["rule-providers"].objectValue?["A"] == nil)
        precondition(deletion.common["rule-providers"].objectValue?["A"] != nil)
        precondition(deletion.removals.contains("/rule-providers/A"))
        deletion.layer = "common"
        deletion.document = .object(["schema_version":.integer(1),"common":.object(["proxy-groups":.array([.object(["name":.string("A"),"type":.string("select"),"proxies":.array([.string("DIRECT")])]),.object(["name":.string("B"),"type":.string("select"),"proxies":.array([.string("A")])])]),"rules":.array([.string("MATCH,B")])]),"platforms":.object([:]),"injections":.array([])])
        let beforeGroupDeletion = deletion.document
        deletion.set(["proxy-groups", "0", "name"], .string("Renamed"))
        precondition(deletion.effective["proxy-groups"].arrayValue?[1]["proxies"] == .array([.string("Renamed")]))
        deletion.undo(); precondition(deletion.document == beforeGroupDeletion)
        deletion.deleteItem(path: ["proxy-groups"], index: 0)
        precondition(deletion.document == beforeGroupDeletion && deletion.pendingDeletion?.changes.contains(where: { $0.contains("/proxy-groups/1/proxies/0") }) == true)
        deletion.pendingDeletion = nil
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
    @MainActor private static func verifyCascadeChanges(root: JSONValue, format: String) {
        let store = SubscriptionTemplateStore()
        store.catalog = .object(["schema": root, "format": .string(format)])
        func load(_ json: String) {
            store.document = try! JSONDecoder().decode(JSONValue.self, from: Data(json.utf8))
            store.layer = "common"; store.undoStack = []; store.redoStack = []; store.pendingDeletion = nil
        }
        if format == "singbox" {
            load(#"{"schema_version":2,"common":{"dns":{"servers":[{"type":"udp","tag":"dns-a","server":"1.1.1.1"},{"type":"udp","tag":"dns-b","server":"resolver.example","domain_resolver":"dns-a"},{"type":"udp","tag":"dns-c","server":"dns-a"}],"rules":[{"type":"logical","mode":"or","rules":[{"domain":"one.example"},{"domain":"two.example"}],"action":"route","server":"dns-a"},{"action":"route","server":"dns-b"},{"action":"route","server":"dns-c"}],"final":"dns-a"},"outbounds":[{"type":"direct","tag":"direct","domain_resolver":{"server":"dns-a"}},{"type":"selector","tag":"select","outbounds":["direct"]}],"route":{"final":"select","rules":[{"action":"route","outbound":"select"},{"action":"reject","domain":"safe.example"}]},"unknown":{"server":"dns-a","password":"dns-a"}},"injections":[]}"#)
            let original = store.document
            store.set(["dns", "servers", "0", "tag"], .string("dns-renamed"))
            precondition(store.effective["dns"]["final"] == .string("dns-renamed"))
            precondition(store.effective["dns"]["servers"].arrayValue?[1]["domain_resolver"] == .string("dns-renamed"))
            precondition(store.effective["dns"]["servers"].arrayValue?[2]["server"] == .string("dns-a"), "A hostname must not be rewritten as a DNS tag")
            precondition(store.effective["unknown"]["password"] == .string("dns-a"))
            precondition(store.effective["outbounds"].arrayValue?[0]["domain_resolver"]["server"] == .string("dns-renamed"))
            precondition(store.undoStack.count == 1)
            store.undo(); precondition(store.document == original)
            store.redo(); precondition(store.effective["dns"]["final"] == .string("dns-renamed"))
            let renamed = store.document
            store.set(["dns", "servers", "0", "tag"], .string("dns-b"))
            precondition(store.document == renamed && store.error != nil)
            store.undo(); precondition(store.document == original)
            store.deleteItem(path: ["dns", "servers"], index: 0)
            precondition(store.document == original && store.pendingDeletion != nil)
            let pending = store.pendingDeletion!
            precondition(pending.after["common"]["dns"]["rules"].arrayValue?.count == 1)
            precondition(pending.after["common"]["dns"]["servers"].arrayValue?.map { $0["tag"] } == [.string("dns-c")])
            precondition(pending.after["common"]["outbounds"] == .array([]))
            precondition(pending.after["common"]["route"]["rules"].arrayValue?.count == 1)
            precondition(pending.after["common"]["route"].objectValue?["final"] == nil)
            precondition(pending.after["common"]["unknown"] == original["common"]["unknown"])
            store.pendingDeletion = nil; precondition(store.document == original)
            store.deleteItem(path: ["dns", "servers"], index: 0); store.confirmCascadeDeletion()
            precondition(store.document == pending.after && store.undoStack.count == 1)
            store.undo(); precondition(store.document == original)
            store.redo(); precondition(store.document == pending.after)
            store.undo(); store.deleteItem(path: ["dns", "servers"], index: 0)
            store.set(["unrelated"], .bool(true)); let changed = store.document
            store.confirmCascadeDeletion(); precondition(store.document == changed && store.error != nil)

            load(#"{"schema_version":2,"common":{"outbounds":[{"tag":"A","type":"direct"},{"tag":"B","type":"selector","outbounds":["A"]}],"route":{"rules":[{"outbound":"B"}]}},"injections":[{"group":"B"}]}"#)
            store.set(["outbounds", "1", "tag"], .string("Renamed"))
            precondition(store.document["injections"].arrayValue?[0]["group"] == .string("Renamed"))
            store.deleteItem(path: ["outbounds"], index: 0)
            precondition(store.pendingDeletion?.after["common"]["outbounds"].arrayValue?.count == 1, "An injected group may retain no static members")
            store.confirmCascadeDeletion()
            store.deleteItem(path: ["outbounds"], index: 0)
            precondition(store.pendingDeletion?.after["injections"] == .array([]))

            load(#"{"schema_version":2,"common":{"route":{"rule_set":[{"type":"inline","tag":["first","second"],"rules":[]}],"rules":[{"rule_set":["first","second"],"action":"reject"}]}},"injections":[]}"#)
            store.set(["route", "rule_set", "0", "tag"], .array([.string("renamed"), .string("second")]))
            precondition(store.effective["route"]["rules"].arrayValue?[0]["rule_set"] == .array([.string("renamed"), .string("second")]))
            store.deleteItem(path: ["route", "rule_set"], index: 0)
            precondition(store.pendingDeletion?.after["common"]["route"]["rules"] == .array([]))
            load(#"{"schema_version":2,"common":{"endpoints":[{"type":"wireguard","tag":"wg"}],"route":{"final":"wg","rules":[{"outbound":"wg"}]}},"injections":[]}"#)
            store.set(["endpoints", "0", "tag"], .string("wg-renamed"))
            precondition(store.effective["route"]["final"] == .string("wg-renamed"))
            store.deleteItem(path: ["endpoints"], index: 0)
            precondition(store.pendingDeletion?.after["common"]["route"]["rules"] == .array([]))
            let future: JSONValue = .object(["dns": .object(["rules": .array([.object(["action": .string("future-action"), "server": .string("A")])]),
                "servers": .array([.object(["type": .string("future-protocol"), "tag": .string("future"), "domain_resolver": .string("A")])])])])
            let futureIndex = TemplateReferenceIndex(format: format, schema: TemplateSchema(root: root))
            precondition(futureIndex.references(in: future).isEmpty, "Unknown action/protocol fields must not acquire fallback reference semantics")
            precondition(futureIndex.renamed(future, identity: .init(kind: "dns_server", name: "A"), new: "B") == future)
        } else {
            load(#"{"schema_version":2,"common":{"proxy-groups":[{"name":"A","type":"select","proxies":["DIRECT"]},{"name":"B","type":"select","proxies":["A","DIRECT"]}],"rules":["DOMAIN,a.example,A","DOMAIN-REGEX,^(RULE-SET,A)$,DIRECT","MATCH,B"]},"injections":[]}"#)
            let original = store.document
            store.set(["proxy-groups", "0", "name"], .string("Renamed"))
            precondition(store.effective["rules"].arrayValue?[0] == .string("DOMAIN,a.example,Renamed"))
            precondition(store.effective["rules"].arrayValue?[1] == original["common"]["rules"].arrayValue?[1])
            store.undo(); precondition(store.document == original)
            store.deleteItem(path: ["proxy-groups"], index: 0)
            precondition(store.pendingDeletion?.after["common"]["proxy-groups"].arrayValue?[0]["proxies"] == .array([.string("DIRECT")]))
            precondition(store.pendingDeletion?.after["common"]["rules"].arrayValue?.count == 2)
            store.confirmCascadeDeletion(); store.undo(); precondition(store.document == original)

            load(#"{"schema_version":2,"common":{"rule-providers":{"A":{"type":"inline","behavior":"classical","payload":[]}},"rules":["AND,((RULE-SET,A),(NOT,((DOMAIN,keep.example)))),DIRECT","DOMAIN-REGEX,^(RULE-SET,A)$,DIRECT","RULE-SET,A,DIRECT"]},"injections":[]}"#)
            store.deleteField(parent: ["rule-providers"], key: "A")
            precondition(store.pendingDeletion?.after["common"]["rules"] == .array([.string("DOMAIN-REGEX,^(RULE-SET,A)$,DIRECT")]))
            store.pendingDeletion = nil
            let beforeMissingDelete = store.document
            store.deleteField(parent: ["rule-providers"], key: "missing")
            precondition(store.document == beforeMissingDelete && store.pendingDeletion == nil)
            let nested = MihomoTemplateRule("AND,((RULE-SET,A),(NOT,((DOMAIN,keep.example)))),DIRECT")
            precondition(nested.referenceNames("rule_set") == ["A"])
            precondition(nested.renamingReference("rule_set", old: "A", new: "B").source.contains("RULE-SET,B"))
            precondition(MihomoTemplateRule("DOMAIN-REGEX,^(RULE-SET,A)$,DIRECT").referenceNames("rule_set").isEmpty)
            load(#"{"schema_version":1,"common":{"proxy-groups":[{"name":"A","type":"select","proxies":["DIRECT"]}],"rules":["MATCH,A"]},"platforms":{"windows":{"set":{"rules":["DOMAIN,win.example,A"]},"remove":[]}},"injections":[]}"#)
            let old = store.document
            store.set(["proxy-groups", "0", "name"], .string("B"))
            precondition(TemplateReferenceIndex.effective(store.document, platform: "windows")["rules"] == .array([.string("DOMAIN,win.example,B")]))
            precondition(store.document["platforms"]["macos"] == .null, "Rename must not materialize unrelated platform overlays")
            store.undo(); precondition(store.document == old)
            store.deleteItem(path: ["proxy-groups"], index: 0)
            precondition(store.pendingDeletion?.changes.contains(where: { $0.contains("windows:/rules/0") }) == true)
            store.confirmCascadeDeletion()
            precondition(TemplateReferenceIndex.effective(store.document, platform: "windows")["rules"] == .array([]))
            store.undo(); precondition(store.document == old)
        }
        let oldRules: JSONValue = .object(["route": .object(["rules": .array([
            .object(["name": .string("extension"), "domain": .string("first.example")]),
            .object(["name": .string("extension"), "domain": .string("second.example")])
        ])])])
        var shiftedRules = oldRules; shiftedRules.set(["route", "rules", "0"], nil)
        precondition(TemplateReferenceIndex.originalPath(["route", "rules", "0"], current: shiftedRules, original: oldRules) == ["route", "rules", "1"], "An unknown rule name must not be used as its identity")
        // Every indexed reference kind uses metadata, without touching unrelated strings.
        let definitions = TemplateReferenceIndex(format: format, schema: TemplateSchema(root: root)).collections
        for (kind, collection) in definitions where format == "singbox" || ["outbound", "rule_set", "sub_rule"].contains(kind) {
            let referenceSchema: JSONValue = .object(["type": .string("object"), "properties": .object([
                "link": .object(["type": .string("string"), "x-tag-reference": .string(kind)])
            ])])
            let index = TemplateReferenceIndex(format: format, schema: TemplateSchema(root: referenceSchema))
            var config: JSONValue = .object(["link": .string("A"), "unknown": .string("A")])
            let isMap = format == "mihomo" && ["rule_set", "sub_rule"].contains(kind)
            let target = collection + [isMap ? "A" : "0"]
            let item: JSONValue = .object([(format == "mihomo" && kind == "outbound") ? "name" : "tag": .string("A")])
            if isMap { config.set(target, item) } else { config.set(collection, .array([item])) }
            let renamed = index.renamed(config, identity: .init(kind: kind, name: "A"), new: "new/~name")
            precondition(renamed["link"] == .string("new/~name") && renamed["unknown"] == .string("A"), kind)
            precondition(index.exists("new/~name", kind: kind, in: renamed), kind)
            config.set(target, nil)
            let cascade = index.cascading(config, removing: [.init(kind: kind, name: "A")], injectedGroups: [])
            precondition(cascade.config.objectValue?["link"] == nil && cascade.config["unknown"] == .string("A"), kind)
        }
        print("\(format): atomic cascading rename, deletion preview, dependency closure, cancel, stale guards and undo/redo passed")
    }
}
