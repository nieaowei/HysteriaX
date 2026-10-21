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
        let visible = TemplateEditorGuidance.visibleKeys(definition, path: path, schema: store.schema, value: store.effective)
        let ordered = TemplateFieldLayout.dialerFields(in: visible)
        return ordered + (visible.contains("protect_path") ? ["protect_path"] : [])
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
        ForEach(TemplateHTTPClientLayout.tlsSections(tlsSchema, value: value)) { section in
        Section(L10n.text(section.title)) {
            let tlsProperties = tlsSchema["properties"].objectValue ?? [:]
            if section.title == "TLS 基础设置" {
                Text(L10n.text("TLS 随 HTTPS 请求自动启用。"))
                    .font(.caption).foregroundStyle(.secondary)
            }
            let visible = Set(TemplateEditorGuidance.visibleKeys(tlsSchema, path: path + ["tls"], schema: store.schema, value: store.effective))
            ForEach(section.keys.filter { visible.contains($0) }, id: \.self) { key in
                if let field = tlsProperties[key] {
                    fieldView(key, definition: field, parent: path + ["tls"], inlineObject: ["ech", "utls", "reality"].contains(key))
                }
            }
        }
        }
        let unknownTLSKeys = (value["tls"].objectValue ?? [:]).keys.filter { tlsSchema["properties"].objectValue?[$0] == nil }.sorted()
        if !unknownTLSKeys.isEmpty {
            Section(L10n.text("TLS 未验证字段")) {
                Text(unknownTLSKeys.joined(separator: "、")).textSelection(.enabled)
                Text(L10n.text("未验证字段：保留原值，可通过源码编辑。"))
                    .font(.caption).foregroundStyle(.secondary)
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
        Set(["tag", "engine", "version", "disable_version_fallback", "headers", "tls"] + TemplateHTTPClientLayout.http2 + TemplateHTTPClientLayout.quic + TemplateFieldLayout.dialerFields(in: Array(properties.keys)) + ["protect_path"])
    }
    @ViewBuilder private func fieldView(_ key: String, definition: JSONValue, parent: [String]? = nil, inlineObject: Bool = false) -> some View {
        let fieldPath = (parent ?? path) + [key]
        SubscriptionTemplateFields(store: store, definition: definition, path: fieldPath, depth: depth + 1, showsFieldHeader: !inlineObject, expandsObjectInline: inlineObject)
    }
}
