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
        func exercise(_ name: String, path: [String], initial: JSONValue, kinds: [String], inline: Bool = false, discriminator: String = "type") {
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
            settle()
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
                    let variants = store.schema.variants(definition)
                    let variant = variants.first { store.schema.variantTitle($0, index: 0) == kind }!
                    let previous = store.document
                    store.set(path, store.schema.selecting(variant, from: definition, value: store.effective.at(path)!))
                    settle()
                    precondition(store.effective.at(path)?[discriminator] == .string(kind))
                    if store.document != previous {
                        let next = store.document
                        store.undo(); settle()
                        precondition(store.document == previous)
                        store.redo(); settle()
                        precondition(store.document == next)
                    }
                }
            }
            print("\(name) type switching and undo/redo: passed")
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
        exercise("DNSRuleAction", path: ["dns", "rules", "0"], initial: .object([
            "action": .string("route"), "server": .string("dns-main"), "rewrite_ttl": .integer(60)
        ]), kinds: ["reject", "route"], discriminator: "action")

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
