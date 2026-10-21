import AppKit
import SwiftUI

/// Render the real editor while schema-changing actions invalidate its old rows.
@main struct TemplatePickerTransitionChecks {
    @MainActor static func main() throws {
        _ = NSApplication.shared
        let root = try JSONDecoder().decode(JSONValue.self, from: Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1])))
        let store = SubscriptionTemplateStore()
        store.catalog = .object(["schema": root, "format": .string("singbox")])
        let host = NSHostingView(rootView: AnyView(EmptyView()))
        let window = NSWindow(contentRect: NSRect(x: -2000, y: -2000, width: 850, height: 700), styleMask: [.borderless], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = host
        window.orderFront(nil)
        defer { window.contentView = nil; window.close() }
        func settle() {
            host.layoutSubtreeIfNeeded()
            RunLoop.current.run(until: Date().addingTimeInterval(0.1))
            host.layoutSubtreeIfNeeded()
        }
        func descendants<T: NSView>(_ type: T.Type, in view: NSView) -> [T] {
            (view as? T).map { [$0] } ?? view.subviews.flatMap { descendants(type, in: $0) }
        }
        func editableStrings() -> [String] {
            descendants(NSTextField.self, in: host).filter { $0.isEditable }.map(\.stringValue)
                + descendants(NSTextView.self, in: host).filter { $0.isEditable }.map(\.string)
        }
        func enterText(_ text: String) {
            if let field = descendants(NSTextField.self, in: host).first(where: { $0.isEditable }) {
                window.makeFirstResponder(field)
                if let editor = field.currentEditor() as? NSTextView {
                    editor.string = text
                    editor.didChangeText()
                } else {
                    field.stringValue = text
                    field.delegate?.controlTextDidChange?(Notification(name: NSControl.textDidChangeNotification, object: field))
                }
            } else if let editor = descendants(NSTextView.self, in: host).first(where: { $0.isEditable }) {
                editor.string = text
                editor.didChangeText()
            } else { preconditionFailure("Expected an editable scalar input") }
            settle()
        }
        let enumCases: [(JSONValue, [String], [String])] = [
            (.object(["route": .object(["rules": .array([.object(["action": .string("reject"), "method": .string("")])])])]), ["route", "rules", "0", "method"], ["default", "drop", "reply"]),
            (.object(["dns": .object(["rules": .array([.object(["action": .string("reject")])])])]), ["dns", "rules", "0", "method"], ["default", "drop", "reply"]),
            (.object(["inbounds": .array([.object(["type": .string("tproxy"), "udp_mapping": .string("")])])]), ["inbounds", "0", "udp_mapping"], ["endpoint_independent", "address_dependent", "address_and_port_dependent"]),
            (.object(["inbounds": .array([.object(["type": .string("shadowtls"), "version": .integer(3), "wildcard_sni": .string("")])])]), ["inbounds", "0", "wildcard_sni"], ["off", "authed", "all"]),
            (.object(["route": .object(["rules": .array([.object(["action": .string("resolve"), "strategy": .string("")])])])]), ["route", "rules", "0", "strategy"], [L10n.text("未设置（由内核决定）"), "as_is", "prefer_ipv4", "prefer_ipv6", "ipv4_only", "ipv6_only"]),
            (.object(["outbounds": .array([.object(["type": .string("direct")])])]), ["outbounds", "0", "network_strategy"], [L10n.text("未设置（继承默认）"), L10n.text("按顺序连接（default）"), "fallback", "hybrid"])
        ]
        for (config, path, expected) in enumCases {
            store.document = .object(["schema_version": .integer(2), "common": config])
            host.rootView = AnyView(Form {
                SubscriptionTemplateFields(store: store, definition: store.schema.definition(at: path, value: store.effective), path: path)
            }.formStyle(.grouped))
            let baseline = store.document
            settle()
            // SwiftUI menu pickers do not expose NSPopUpButton menu items.
            // Assert the shared presentation model, then render the real field.
            let field = store.schema.resolve(store.schema.definition(at: path, value: store.effective), value: store.effective.at(path) ?? .null)
            let options = TemplateEditorGuidance.enumPickerOptions(field, path: path, schema: store.schema, value: store.effective)
            let titles = (options.includesUnset ? [TemplateEditorGuidance.enumPickerTitle(nil, path: path, schema: store.schema)] : [])
                + options.values.map { TemplateEditorGuidance.enumPickerTitle($0, path: path, schema: store.schema) }
            precondition(titles == expected, "Unexpected enum menu at \(path): \(titles)")
            precondition(store.document == baseline, "Opening an enum must not normalize existing empty values")
            store.set(path, options.values.last!)
            settle()
            precondition(store.effective.at(path)?.stringValue == expected.last)
            store.undo(); settle(); precondition(store.document == baseline)
        }
        print("Enum presentation and native rendering merge empty aliases, preserve inheritance and keep existing drafts unchanged: passed")
        let fixedPath = ["inbounds", "0", "udp_timeout"]
        store.document = .object(["schema_version": .integer(2), "common": .object(["inbounds": .array([.object([
            "type": .string("mixed"), "udp_timeout": .string("5m")
        ])])])])
        host.rootView = AnyView(SubscriptionTemplateFields(store: store, definition: store.schema.definition(at: fixedPath, value: store.effective), path: fixedPath))
        let legacyDocument = store.document
        settle()
        precondition(editableStrings().contains("5m") && store.document == legacyDocument)
        enterText("123")
        precondition(store.effective.at(fixedPath) == .integer(123), "Fixed scalar edits must store JSON integers")
        let validInteger = store.document
        for invalid in ["5m", "64 MB", "1.5", "18446744073709551616"] {
            enterText(invalid)
            precondition(store.document == validInteger, "Invalid integer drafts must not become JSON strings")
        }
        enterText("18446744073709551615")
        precondition(store.effective.at(fixedPath) == .unsignedInteger(UInt64.max))
        store.undo(); settle(); precondition(store.document == validInteger)
        store.redo(); settle(); precondition(store.effective.at(fixedPath) == .unsignedInteger(UInt64.max))
        window.makeFirstResponder(nil)
        let markPath = ["inbounds", "0", "auto_redirect_input_mark"]
        store.document = .object(["schema_version": .integer(2), "common": .object(["inbounds": .array([.object([
            "type": .string("tun"), "auto_redirect_input_mark": .integer(8227)
        ])])])])
        host.rootView = AnyView(SubscriptionTemplateFields(store: store, definition: store.schema.definition(at: markPath, value: store.effective), path: markPath))
        let legacyMark = store.document
        settle()
        precondition(editableStrings().contains("8227") && store.document == legacyMark)
        enterText("0x2024")
        precondition(store.effective.at(markPath) == .string("0x2024"), "Mark edits must always store JSON strings")
        window.makeFirstResponder(nil)
        print("Fixed scalar native editing: integer serialization, invalid drafts, UInt64, legacy preservation and string marks passed")
        let inlineTLSCases: [([String], JSONValue, Bool)] = [
            (["inbounds", "0"], .object(["type": .string("http")]), false),
            (["outbounds", "0"], .object(["type": .string("vless")]), false),
            (["services", "0"], .object(["type": .string("api")]), false),
            (["dns", "servers", "0"], .object(["type": .string("https")]), true),
            (["http_clients", "0"], .object(["tag": .string("http")]), true)
        ]
        for (parent, initial, automatic) in inlineTLSCases {
            var item = initial
            item["tls"] = .object(["server_name": .string("tls-inline.example")])
            var config = item
            for key in parent.reversed() { config = key == "0" ? .array([config]) : .object([key: config]) }
            store.document = .object(["schema_version": .integer(2), "common": config])
            store.focusedPath = ""
            let path = parent + ["tls"]
            host.rootView = AnyView(Form {
                SubscriptionTemplateFields(store: store, definition: store.schema.definition(at: path, value: store.effective), path: path)
            }.formStyle(.grouped))
            let before = store.document
            settle()
            precondition(editableStrings().contains("tls-inline.example") == automatic, "TLS parameters must reflect the actual enabled default")
            precondition(store.document == before, "Rendering inline TLS must not populate its default")
            store.focusedPath = SubscriptionTemplateStore.pointer(path + ["server_name"]); settle()
            precondition(window.sheets.isEmpty, "Focusing a TLS parameter must not open a TLS sheet")
            for enabled in [true, false, true] {
                let previous = store.document
                store.set(path + ["enabled"], .bool(enabled)); settle()
                precondition(editableStrings().contains("tls-inline.example") == (automatic || enabled))
                precondition(store.effective.at(path)?["server_name"] == .string("tls-inline.example"))
                precondition(window.sheets.isEmpty)
                let next = store.document
                store.undo(); settle(); precondition(store.document == previous)
                store.redo(); settle(); precondition(store.document == next)
            }
        }
        for kind in ["openconnect", "openvpn-client", "openvpn-server"] {
            let parameter = kind == "openvpn-server" ? "certificate_path" : "server_name"
            store.document = .object(["schema_version": .integer(2), "common": .object(["endpoints": .array([.object([
                "type": .string(kind), "tls": .object([parameter: .string("dedicated-tls.example")])
            ])])])])
            let path = ["endpoints", "0", "tls"]
            store.focusedPath = ""
            host.rootView = AnyView(Form {
                SubscriptionTemplateFields(store: store, definition: store.schema.definition(at: path, value: store.effective), path: path)
            }.formStyle(.grouped))
            let before = store.document
            settle()
            precondition(editableStrings().contains("dedicated-tls.example"), "Dedicated TLS without enabled must remain visible")
            store.focusedPath = SubscriptionTemplateStore.pointer(path + [parameter]); settle()
            precondition(window.sheets.isEmpty && store.document == before)
        }
        store.focusedPath = ""
        print("Inline TLS inputs, enabled defaults, automatic TLS, diagnostic focus and dedicated VPN TLS: passed")
        let vpnDefaults: JSONValue = .object(["endpoints": .array([.object(["type": .string("openvpn-server")])])])
        for (key, expected) in [("mtu", "1500"), ("max_clients", "1024"), ("renegotiate_bytes", "")] {
            store.document = .object(["schema_version": .integer(2), "common": vpnDefaults])
            let baseline = store.document
            let path = ["endpoints", "0", key]
            host.rootView = AnyView(SubscriptionTemplateFields(store: store, definition: store.schema.definition(at: path, value: store.effective), path: path))
            settle()
            let fields = descendants(NSTextField.self, in: host).filter { $0.isEditable }
            precondition(fields.contains { $0.stringValue == expected }, "Native input must show the actual default or remain blank: \(key)")
            precondition(store.document == baseline)
        }
        for key in ["version_min", "version_max", "remote_certificate_tls"] {
            let path = ["endpoints", "0", "tls", key]
            let baseline = store.document
            host.rootView = AnyView(SubscriptionTemplateFields(store: store, definition: store.schema.definition(at: path, value: store.effective), path: path))
            settle()
            precondition(store.document == baseline)
        }
        store.document = .object(["schema_version": .integer(2), "common": .object([:])])
        let originsPath = ["experimental", "clash_api", "access_control_allow_origin"]
        let origins = store.schema.scalarListItemDefinition(store.schema.definition(at: originsPath, value: store.effective))!
        let beforeOrigins = store.document
        host.rootView = AnyView(TemplateScalarListEditor(store: store, path: originsPath, itemDefinition: origins, allowsScalar: true))
        settle(); precondition(store.document == beforeOrigins)
        store.set(originsPath, .array([])); settle()
        precondition(store.effective.at(originsPath) == .array([]))
        store.undo(); settle(); precondition(store.document == beforeOrigins)
        print("Native default inputs, enum rendering and list previews preserve the draft: passed")

        func exercise(_ name: String, path: [String], initial: JSONValue, kinds: [String], inline: Bool = false, discriminator: String = "type", actionSwitch: Bool = false) {
            let definition = root["$defs"][name]
            precondition(definition != .null)
            var config = initial
            for key in path.reversed() { config = key == "0" ? .array([config]) : .object([key: config]) }
            store.document = .object(["schema_version": .integer(2), "common": config])
            store.expandedPaths = []
            store.focusedPath = ""
            if inline {
                host.rootView = AnyView(SubscriptionTemplateFields(store: store, definition: definition, path: path, expandsObjectInline: true))
            } else {
                host.rootView = AnyView(TemplateStructuredFieldEditor(store: store, definition: definition, path: path, depth: 0, title: name))
            }
            let beforeRender = store.document
            settle()
            precondition(store.document == beforeRender, "Opening an editor must not rewrite its discriminators")
            let properties = store.schema.resolve(definition, value: initial)["properties"].objectValue ?? [:]
            if let key = TemplateFieldLayout.dialerFields(in: Array(properties.keys)).first {
                let expansionKey = SubscriptionTemplateStore.pointer(path) + "/$dialer"
                precondition(!store.expandedPaths.contains(expansionKey), "Advanced settings must start collapsed")
                store.focusedPath = SubscriptionTemplateStore.pointer(path + [key]); settle()
                precondition(store.expandedPaths.contains(expansionKey), "Diagnostic focus must reveal dialer fields")
                store.focusedPath = ""
            }
            for _ in 0..<2 {
                for kind in kinds {
                    let switchingDefinition = actionSwitch ? store.schema.actionDefinition(definition)! : definition
                    let variants = store.schema.variants(switchingDefinition)
                    let variant = variants.first { store.schema.variantTitle($0, index: 0) == kind }!
                    let previous = store.document
                    let current = store.effective.at(path)!
                    store.set(path, actionSwitch ? store.schema.selectingAction(kind, from: definition, value: current) : store.schema.selecting(variant, from: definition, value: current))
                    settle()
                    precondition(store.effective.at(path)?[discriminator] == .string(kind))
                    if actionSwitch {
                        for key in ["type", "domain", "mode", "rules", "network_type", "future_field"] where initial.objectValue?[key] != nil {
                            precondition(store.effective.at(path)?[key] == initial[key], "Action switching must preserve rule conditions and extensions")
                        }
                    }
                    if store.document != previous {
                        let next = store.document
                        store.undo(); settle()
                        precondition(store.document == previous)
                        store.redo(); settle()
                        precondition(store.document == next)
                    }
                }
            }
            print("\(name) \(discriminator) switching and undo/redo: passed")
        }
        exercise("RuleSet", path: ["route", "rule_set", "0"], initial: .object([
            "type": .string("remote"), "tag": .string("test"), "format": .string("binary"), "url": .string("https://example.invalid/rules.srs")
        ]), kinds: ["local", "inline", "remote"])
        exercise("Outbound", path: ["outbounds", "0"], initial: .object([
            "type": .string("shadowsocks"), "tag": .string("test"), "method": .string("aes-128-gcm"), "server": .string("example.invalid"), "password": .string("test")
        ]), kinds: ["socks", "direct", "shadowsocks"])
        exercise("DNSServer", path: ["dns", "servers", "0"], initial: .object([
            "type": .string("https"), "tag": .string("test"), "server": .string("example.invalid"), "path": .string("/dns-query")
        ]), kinds: ["local", "udp", "https"])
        exercise("V2RayTransport", path: ["outbounds", "0", "transport"], initial: .object([
            "type": .string("ws"), "path": .string("/test"), "max_early_data": .integer(2048)
        ]), kinds: ["http", "grpc", "ws"], inline: true)
        exercise("ACMEProviderDNS01Challenge", path: ["certificate_providers", "0", "dns01_challenge"], initial: .object([
            "provider": .string("alidns"), "access_key_id": .string("keep-id"), "access_key_secret": .string("keep-secret")
        ]), kinds: ["cloudflare", "acmedns", "alidns"], discriminator: "provider")
        exercise("DNSRuleAction", path: ["dns", "rules", "0"], initial: .object([
            "action": .string("route"), "server": .string("dns-main"), "rewrite_ttl": .integer(60)
        ]), kinds: ["reject", "route"], discriminator: "action")
        for logical in [false, true] {
            var rule: JSONValue = logical
                ? .object(["type": .string("logical"), "mode": .string("and"), "rules": .array([.object(["domain": .string("keep.example")])])])
                : .object(["domain": .string("keep.example"), "network_type": .array([.string("wifi")])])
            rule["future_field"] = .string("keep")
            rule["outbound"] = .string("proxy")
            exercise("Rule", path: ["route", "rules", "0"], initial: rule, kinds: ["direct", "sniff", "reject", "route"], discriminator: "action", actionSwitch: true)
            rule.set(["outbound"], nil)
            rule["server"] = .string("dns-main")
            exercise("DNSRule", path: ["dns", "rules", "0"], initial: rule, kinds: ["predefined", "reject", "route"], discriminator: "action", actionSwitch: true)
        }
        exercise("CertificateProviderReference", path: ["inbounds", "0", "tls", "certificate_provider"], initial: .object([
            "type": .string("acme"), "domain": .array([.string("example.com")])
        ]), kinds: ["acme", "cloudflare-origin-ca", "tailscale"])

        let outboundDefinition = root["$defs"]["Outbound"]
        store.document = .object(["schema_version": .integer(2), "common": .object([
            "outbounds": .array([.object(["type": .string("snell"), "version": .integer(4), "psk": .string("test")])])
        ])])
        host.rootView = AnyView(TemplateStructuredFieldEditor(store: store, definition: outboundDefinition, path: ["outbounds", "0"], depth: 0, title: "Snell"))
        settle()
        for version in [6, 4, 6] {
            let target = store.schema.variants(outboundDefinition).first { candidate in
                let resolved = store.schema.resolve(candidate)
                return resolved["properties"]["type"]["const"] == .string("snell") && resolved["properties"]["version"]["const"] == .integer(version)
            }!
            store.set(["outbounds", "0"], store.schema.selecting(target, from: outboundDefinition, value: store.effective["outbounds"].arrayValue![0])); settle()
            precondition(store.effective["outbounds"].arrayValue![0]["version"] == .integer(version))
        }
        print("Nested Snell version branches: switching passed")

        let tlsDefinition = root["$defs"]["OutboundTLSOptions"]
        store.document = .object(["schema_version": .integer(2), "common": .object([
            "outbounds": .array([.object(["type": .string("vless"), "tls": .object(["enabled": .bool(false), "server_name": .string("keep.example")])])])
        ])])
        host.rootView = AnyView(TemplateStructuredFieldEditor(store: store, definition: tlsDefinition, path: ["outbounds", "0", "tls"], depth: 0, title: "TLS"))
        settle()
        for flag in [true, false, true] {
            store.set(["outbounds", "0", "tls", "enabled"], .bool(flag)); settle()
            precondition(store.effective.at(["outbounds", "0", "tls", "server_name"]) == .string("keep.example"))
        }
        print("TLS feature gating preserves hidden fields: passed")

        // Exercise modes with preserved hidden data in the real SwiftUI hierarchy.
        for kind in ["openvpn-client", "openvpn-server"] {
            let path = ["endpoints", "0"]
            let initial: JSONValue = .object(["type": .string(kind), "tls": .object(["server_name": .string("keep.example")]), "static_key": .string("preserve-static-key")])
            store.document = .object(["schema_version": .integer(2), "common": .object(["endpoints": .array([initial])])])
            host.rootView = AnyView(TemplateStructuredFieldEditor(store: store, definition: root["$defs"]["Endpoint"], path: path, depth: 0, title: kind))
            settle()
            for mode in ["static_key", "tls"] {
                let before = store.document
                store.set(path + ["mode"], .string(mode)); settle()
                precondition(host.fittingSize.height.isFinite)
                precondition(store.effective.at(path)?["tls"] == initial["tls"])
                precondition(store.effective.at(path)?["static_key"] == initial["static_key"])
                store.undo(); settle(); precondition(store.document == before)
                store.redo(); settle()
            }
        }
        store.document = .object(["schema_version": .integer(2), "common": .object([
            "outbounds": .array([.object(["type": .string("vless"), "tls": .object(["enabled": .bool(true), "client_key_path": .string("keep.key")])])])
        ])])
        host.rootView = AnyView(TemplateStructuredFieldEditor(store: store, definition: tlsDefinition, path: ["outbounds", "0", "tls"], depth: 0, title: "TLS engines"))
        for engine in ["apple", "windows", "go"] {
            let previous = store.document
            store.set(["outbounds", "0", "tls", "engine"], .string(engine)); settle()
            precondition(store.effective["outbounds"].arrayValue![0]["tls"]["client_key_path"] == .string("keep.key"))
            store.undo(); settle(); precondition(store.document == previous)
            store.redo(); settle()
        }
        print("OpenVPN session modes and general TLS engines preserve hidden configuration through undo/redo: passed")

        // Render the new protocol groups while dependent rows disappear and reappear.
        let optionCases: [(String, [String], JSONValue, [String])] = [
            ("Inbound", ["inbounds", "0"], .object(["type": .string("tun"), "address": .array([.string("172.18.0.1/30")]), "auto_redirect_input_mark": .string("0x1234"), "route_address": .array([.string("192.0.2.0/24")])]), ["auto_route", "auto_redirect"]),
            ("Endpoint", ["endpoints", "0"], .object(["type": .string("tailscale"), "system_interface_name": .string("ts0")]), ["system_interface"]),
            ("Endpoint", ["endpoints", "0"], .object(["type": .string("openvpn-client"), "server": .string("vpn.example"), "mss_fix": .integer(1400), "ping_restart": .string("60s"), "renegotiate_interval": .string("2h")]), ["mss_fix_disabled", "ping_restart_disabled", "renegotiate_disabled"]),
            ("CertificateProvider", ["certificate_providers", "0"], .object(["type": .string("acme"), "domain": .array([.string("example.com")]), "alternative_http_port": .integer(8080), "alternative_tls_port": .integer(8443)]), ["disable_http_challenge", "disable_tls_alpn_challenge"]),
            ("OutboundMultiplexOptions", ["outbounds", "0", "multiplex"], .object(["padding": .bool(true), "max_streams": .integer(4)]), ["enabled"]),
            ("OutboundRealityOptions", ["outbounds", "0", "tls", "reality"], .object(["public_key": .string("preserve-key")]), ["enabled"])
        ]
        for (name, path, initial, switches) in optionCases {
            var config = initial
            for key in path.reversed() { config = key == "0" ? .array([config]) : .object([key: config]) }
            store.document = .object(["schema_version": .integer(2), "common": config])
            host.rootView = AnyView(TemplateStructuredFieldEditor(store: store, definition: root["$defs"][name], path: path, depth: 0, title: name))
            let untouched = store.document
            settle()
            precondition(store.document == untouched, "Rendering documented defaults must not populate an untouched draft")
            for key in switches {
                for flag in [true, false, true] {
                    let previous = store.document
                    store.set(path + [key], .bool(flag)); settle()
                    for (originalKey, originalValue) in initial.objectValue ?? [:] where originalKey != key && !switches.contains(originalKey) {
                        precondition(store.effective.at(path)?[originalKey] == originalValue, "Conditional rows must preserve existing values")
                    }
                    store.undo(); settle(); precondition(store.document == previous)
                    store.redo(); settle()
                }
            }
        }
        print("TUN, Tailscale, OpenVPN, ACME and shared feature controls: conditional rendering and undo/redo passed")
        let quicInbound: JSONValue = .object(["type": .string("hysteria2")])
        store.document = .object(["schema_version": .integer(2), "common": .object(["inbounds": .array([quicInbound])])])
        let memoryPath = ["inbounds", "0", "stream_receive_window"]
        let memoryDefinition = store.schema.definition(at: memoryPath, value: store.effective)
        host.rootView = AnyView(SubscriptionTemplateFields(store: store, definition: memoryDefinition, path: memoryPath))
        for size in [JSONValue.string("64 MB"), .integer(65536), .unsignedInteger(UInt64.max)] {
            store.set(memoryPath, size); settle()
            precondition(store.effective.at(memoryPath) == size)
            store.undo(); settle(); store.redo(); settle()
        }
        print("Fixed integer receive window editor preserves existing values and UInt64 through undo/redo: passed")


        let predefined: [String: JSONValue] = Dictionary(uniqueKeysWithValues: (0..<20).map { ("domain-\($0).example", .array([.string("192.0.2.1")])) })
        store.document = .object(["schema_version": .integer(2), "common": .object(["dns": .object(["predefined": .object(predefined)])])])
        host.rootView = AnyView(TemplateMapFields(store: store, definition: .object(["type": .string("array"), "items": .object(["type": .string("string")])]), path: ["dns", "predefined"], depth: 0))
        settle()
        precondition(host.fittingSize.height > 700, "Predefined overview must grow with its rows")
        store.deleteField(parent: ["dns", "predefined"], key: "domain-10.example"); settle()
        store.undo(); settle()
        precondition(store.effective.at(["dns", "predefined"]) == .object(predefined))
        print("Large predefined overview and deletion undo: passed")

        for clientPath in [["http_clients", "0"], ["route", "rule_set", "0", "http_client"]] {
            let clientDefinition = clientPath.first == "http_clients" ? root["$defs"]["HTTPClient"] : root["$defs"]["HTTPClientReference"]
            let client: JSONValue = .object([
                "tag": .string("web"), "version": .integer(3), "initial_packet_size": .integer(1200),
                "headers": .object(["X-Test": .array([.string("one"), .string("two")])]),
                "tls": .object(["server_name": .string("example.com"), "alpn": .array([.string("h3")])])
            ])
            var config: JSONValue = .object([:]); config.set(clientPath, client)
            // Build array parents explicitly rather than relying on object-path creation.
            if clientPath.first == "http_clients" { config = .object(["http_clients": .array([client])]) }
            else { config = .object(["route": .object(["rule_set": .array([.object(["http_client": client])])])]) }
            store.document = .object(["schema_version": .integer(2), "common": config])
            host.rootView = AnyView(TemplateStructuredFieldEditor(store: store, definition: clientDefinition, path: clientPath, depth: 0, title: "HTTP Client"))
            let beforeRender = store.document
            settle(); precondition(store.document == beforeRender)
            for key in ["ech", "utls", "reality"] {
                store.set(clientPath + ["tls", key, "enabled"], .bool(true)); settle()
                precondition(store.effective.at(clientPath)?["tls"][key]["enabled"] == .bool(true))
                store.set(clientPath + ["tls", key, "enabled"], .bool(false)); settle()
            }
            for engine in ["apple", "windows", "go"] {
                store.set(clientPath + ["tls", "engine"], .string(engine)); settle()
                precondition(store.effective.at(clientPath)?["tls"]["server_name"] == .string("example.com"))
            }
            for version in [0, 1, 2, 3] {
                store.set(clientPath + ["version"], .integer(version)); settle()
                precondition(store.effective.at(clientPath)?["headers"] == client["headers"])
            }
            store.set(clientPath + ["engine"], .string("apple")); settle()
            let beforeClearing = store.document
            let tlsSchema = store.schema.resolve(root["$defs"]["OutboundTLSOptions"])
            let current = store.effective.at(clientPath)!
            store.set(clientPath, TemplateHTTPClientLayout.clearingIncompatible(current, tlsSchema: tlsSchema)); settle()
            precondition(TemplateHTTPClientLayout.incompatiblePaths(store.effective.at(clientPath)!, tlsSchema: tlsSchema).isEmpty)
            store.undo(); settle(); precondition(store.document == beforeClearing)
            store.redo(); settle()
            store.set(clientPath + ["engine"], .string("go")); settle()
            precondition(store.effective.at(clientPath)?["headers"] == client["headers"])
        }
        print("Shared and inline HTTP clients: version/engine switching, headers and cleanup undo/redo passed")

        store.document = .object(["schema_version": .integer(2), "common": .object([:]), "injections": .array([
            .object(["group": .string("one")]), .object(["group": .string("two")])
        ])])
        host.rootView = AnyView(SubscriptionTemplateInjections(store: store))
        settle()
        for _ in 0..<2 {
            store.edit { $0["injections"] = .array([]) }; settle()
            store.undo(); settle()
        }
        print("Injection row removal and undo: passed")

        let listPath = ["tag_test"]
        let shortValues: [JSONValue] = (0..<12).map { .string("entry-\($0).example.invalid") }
        store.set(listPath, .array(shortValues))
        host.rootView = AnyView(TemplateScalarListEditor(store: store, path: listPath, itemDefinition: .object(["type": .string("string")])))
        for width in [360, 640, 900] {
            window.setContentSize(NSSize(width: width, height: 700)); settle()
            precondition(host.fittingSize.height.isFinite && host.fittingSize.width.isFinite)
            precondition(store.effective.at(listPath) == .array(shortValues), "Tag rendering must not modify values")
        }
        for values in [[JSONValue.string(""), .string("duplicate"), .string("duplicate")], [.string("line one\nline two"), .string("last")]] {
            store.set(listPath, .array(values)); settle()
            precondition(store.effective.at(listPath) == .array(values), "External updates must preserve empty and multiline strings")
            store.undo(); settle()
            precondition(store.effective.at(listPath) == .array(shortValues))
            store.redo(); settle()
            precondition(store.effective.at(listPath) == .array(values))
            store.set(listPath, .array(shortValues)); settle()
        }
        print("Tag list width changes, external updates and undo/redo preserve values: passed")
    }
}
