import SwiftUI

/// Shared clients and inline HTTPClientReference objects use the same form.
struct TemplateHTTPClientFields: View {
    @Bindable var store: SubscriptionTemplateStore
    let definition: JSONValue
    let path: [String]
    let depth: Int
    private var value: JSONValue { store.effective.at(path) ?? .null }
    private var properties: [String: JSONValue] { definition["properties"].objectValue ?? [:] }
    private var tlsSchema: JSONValue { store.schema.resolve(properties["tls"] ?? .null, value: value["tls"]) }
    private var dialerKeys: [String] {
        let ordered = TemplateFieldLayout.dialerFields(in: Array(properties.keys))
        return ordered + (properties["protect_path"] == nil ? [] : ["protect_path"])
    }
    var body: some View {
        Section(L10n.text("基础信息")) {
            if let field = properties["tag"] { fieldView("tag", definition: field) }
            if let field = properties["engine"] { fieldView("engine", definition: field) }
            if TemplateHTTPClientLayout.isApple(value) {
                Text(L10n.text("Apple 引擎仅在 Apple 平台可用，HTTP 版本由系统管理。"))
                    .font(.caption).foregroundStyle(.secondary)
            } else {
                Picker(selection: Binding(get: { value["version"].integerValue ?? 0 }, set: { store.set(path + ["version"], .integer($0)) })) {
                    ForEach(versionChoices, id: \.self) { version in
                        Text(TemplateHTTPClientLayout.versionTitle(version)).tag(version)
                    }
                } label: {
                    TemplateFieldLabel(store: store, path: path + ["version"], title: "version")
                }.accessibilityIdentifier("templates.httpClient.version")
                if let field = properties["disable_version_fallback"] { fieldView("disable_version_fallback", definition: field) }
            }
        }
        if let headers = properties["headers"] {
            Section(L10n.text("请求标头")) {
                TemplateMapFields(store: store, definition: store.schema.resolve(headers)["additionalProperties"], path: path + ["headers"], depth: depth + 1)
            }
        }
        let transportKeys = TemplateHTTPClientLayout.transportKeys(value)
        if !transportKeys.isEmpty {
            Section(TemplateHTTPClientLayout.version(value) == 3 ? L10n.text("QUIC 参数") : L10n.text("HTTP/2 参数")) {
                ForEach(transportKeys, id: \.self) { key in
                    if let field = properties[key] { fieldView(key, definition: field) }
                }
            }
        }
        Section("TLS") {
            let tlsProperties = tlsSchema["properties"].objectValue ?? [:]
            ForEach(TemplateHTTPClientLayout.tlsBasic, id: \.self) { key in
                if let field = tlsProperties[key] { fieldView(key, definition: field, parent: path + ["tls"]) }
            }
            if !TemplateHTTPClientLayout.isApple(value) {
                DisclosureGroup(L10n.text("TLS 高级设置")) {
                    ForEach(tlsProperties.keys.filter { $0 != "enabled" && !TemplateHTTPClientLayout.tlsBasic.contains($0) }.sorted(), id: \.self) { key in
                        if let field = tlsProperties[key] { fieldView(key, definition: field, parent: path + ["tls"]) }
                    }
                }
            }
        }
        Section {
            TemplateDialerSettings(store: store, path: path, title: L10n.text("HTTP 客户端")) {
                ForEach(dialerKeys, id: \.self) { key in
                    if let field = properties[key] { fieldView(key, definition: field) }
                }
            }
        }
        let incompatible = TemplateHTTPClientLayout.incompatiblePaths(value, tlsSchema: tlsSchema)
        if !incompatible.isEmpty {
            Section(L10n.text("不兼容的已有配置")) {
                Text(incompatible.map { $0.joined(separator: ".") }.joined(separator: "、"))
                    .font(.caption).textSelection(.enabled)
                Button(L10n.text("清除不兼容字段")) {
                    store.set(path, TemplateHTTPClientLayout.clearingIncompatible(value, tlsSchema: tlsSchema))
                }
            }
        }
        let otherKeys = properties.keys.filter { !knownKeys.contains($0) }.sorted()
        if !otherKeys.isEmpty {
            Section(L10n.text("其他选项")) {
                ForEach(otherKeys, id: \.self) { key in
                    if let field = properties[key] { fieldView(key, definition: field) }
                }
            }
        }
    }
    private var versionChoices: [Int] {
        let current = value["version"].integerValue ?? 0
        return [0, 1, 2, 3] + ([0, 1, 2, 3].contains(current) ? [] : [current])
    }
    private var knownKeys: Set<String> {
        Set(["tag", "engine", "version", "disable_version_fallback", "headers", "tls"] + TemplateHTTPClientLayout.http2 + TemplateHTTPClientLayout.quic + dialerKeys)
    }
    @ViewBuilder private func fieldView(_ key: String, definition: JSONValue, parent: [String]? = nil) -> some View {
        let fieldPath = (parent ?? path) + [key]
        if ["stream_receive_window", "connection_receive_window"].contains(key) {
            LabeledContent {
                TextField(key, text: Binding(get: {
                    guard let current = store.effective.at(fieldPath) else { return "" }
                    return current.stringValue ?? current.pretty
                }, set: { store.set(fieldPath, TemplateHTTPClientLayout.memoryValue($0)) }), prompt: Text(verbatim: "64 MB"))
                    .textFieldStyle(.roundedBorder).labelsHidden()
                    .accessibilityIdentifier("templates.value.\(SubscriptionTemplateStore.pointer(fieldPath))")
            } label: {
                TemplateFieldLabel(store: store, path: fieldPath, title: key)
            }
        } else {
            SubscriptionTemplateFields(store: store, definition: definition, path: fieldPath, depth: depth + 1)
        }
    }
}
