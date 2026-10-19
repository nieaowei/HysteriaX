import SwiftUI

/// Native typed controls are generated from the same versioned field definitions as the server.
/// Unverified fields remain visible in source and are never presented as validated controls.
struct SubscriptionTemplateFields: View {
    @Bindable var store: SubscriptionTemplateStore
    let definition: JSONValue
    let path: [String]
    var depth = 0
    @State private var newKey = ""
    @State private var expanded = false
    private var expansion: Binding<Bool> { Binding(get: { expanded || !store.search.isEmpty || store.focusedPath.hasPrefix(SubscriptionTemplateStore.pointer(path) + "/") }, set: { expanded = $0 }) }
    private var value: JSONValue { store.effective.at(path) ?? .null }
    private var schema: JSONValue { store.schema.resolve(definition, value: value) }
    private var properties: [String: JSONValue] { schema["properties"].objectValue ?? [:] }
    private var title: String { path.last ?? "" }
    private var withinArray: Bool { path.indices.contains { store.effective.at(Array(path.prefix($0)))?.arrayValue != nil } }
    private var enumChoices: [JSONValue]? {
        guard var values = schema["enum"].arrayValue ?? schema["x-enum-choices"].arrayValue else { return nil }
        if store.effective.at(path) != nil, !values.contains(value) { values.append(value) }
        return values
    }
    private var members: [String] {
        properties.keys.filter { key in
            store.schema.contains(store.search, in: properties[key]!, path: path + [key])
        }.sorted()
    }
    var body: AnyView {
        AnyView(VStack(alignment: .leading, spacing: 8) {
            if depth < 24 {
                if path.isEmpty { objectFields }
                else {
                    HStack {
                        Text(title).font(.system(.body, design: .monospaced)).textSelection(.enabled)
                        Spacer()
                        Text(store.layer == "common" ? L10n.text("通用配置") : (store.state(path) == "inherit" ? L10n.text("继承通用配置") : TemplatePlatform.title(store.layer)))
                            .font(.caption).foregroundStyle(.secondary)
                        if !withinArray { Picker(L10n.text("字段状态"), selection: Binding(get: { store.state(path) }, set: { store.setState($0, path: path, schema: definition) })) {
                            Text(store.layer == "common" ? L10n.text("未设置") : L10n.text("继承")).tag("inherit")
                            Text(L10n.text("设置")).tag("set")
                            if store.layer != "common" { Text(L10n.text("移除")).tag("remove") }
                        }.labelsHidden().frame(width: 108)
                        .disabled(!store.allowed(path) && store.state(path) != "set")
                        .accessibilityIdentifier("templates.state.\(SubscriptionTemplateStore.pointer(path))") }
                    }
                    if !store.allowed(path, value: value) {
                        Text(L10n.text("此字段由目标客户端管理，或只能在支持的平台覆盖层设置。"))
                            .font(.caption).foregroundStyle(.orange)
                    }
                    if store.state(path) != "remove", store.effective.at(path) != nil {
                        control.disabled(!store.allowed(path, value: value))
                    }
                }
            } else { Text(L10n.text("嵌套层级过深，请在源码中编辑。")) }
        }.accessibilityElement(children: .contain))
    }
    @ViewBuilder private var control: some View {
        let variants = definition["oneOf"].arrayValue ?? definition["anyOf"].arrayValue ?? []
        if variants.count > 1 {
            Picker(L10n.text("配置类型"), selection: Binding(get: { variantIndex(variants) }, set: { index in
                let initial = store.initialValue(variants[index], path: path)
                if let props = store.schema.resolve(variants[index])["properties"].objectValue {
                    var next = value
                    for (key, field) in props { if let constant = field.objectValue?["const"] { next[key] = constant } }
                    store.set(path, next)
                } else { store.set(path, initial) }
            })) {
                ForEach(variants.indices, id: \.self) { index in
                    Text(variantName(variants[index], index: index)).tag(index)
                }
            }
        }
        if schema["x-unverified"].boolValue == true {
            Text(L10n.text("未验证字段：保留原值，可通过源码编辑。"))
                .foregroundStyle(.orange).font(.caption)
        } else if let values = enumChoices {
            Picker(title, selection: Binding(get: { values.firstIndex(of: value) ?? 0 }, set: { store.set(path, values[$0]) })) {
                ForEach(values.indices, id: \.self) { i in Text(values[i].stringValue ?? values[i].pretty).tag(i) }
            }
        } else if let constant = schema.objectValue?["const"] {
            Text(constant.stringValue ?? constant.pretty).foregroundStyle(.secondary)
        } else {
            switch schema["type"].stringValue ?? value.schemaType {
            case "boolean": Toggle(title, isOn: Binding(get: { value.boolValue ?? false }, set: { store.set(path, .bool($0)) })).labelsHidden()
            case "integer": TextField(title, text: Binding(get: { value.stringValue ?? value.pretty }, set: { text in
                if let number = Int(text) { store.set(path, .integer(number)) }
                else if let number = UInt64(text) { store.set(path, .unsignedInteger(number)) }
                else { store.set(path, .string(text)) }
            })).textFieldStyle(.roundedBorder).accessibilityIdentifier("templates.value.\(SubscriptionTemplateStore.pointer(path))")
            case "number": TextField(title, value: Binding(get: { if case .decimal(let n) = value { return n }; return Double(value.integerValue ?? 0) }, set: { store.set(path, .decimal($0)) }), format: .number).textFieldStyle(.roundedBorder).accessibilityIdentifier("templates.value.\(SubscriptionTemplateStore.pointer(path))")
            case "object": DisclosureGroup(isExpanded: expansion) { objectFields.padding(.leading, 10) } label: { Text(L10n.text("编辑对象字段")) }
            case "array":
                if store.selected?.format == "mihomo", path == ["rules"] || (path.first == "sub-rules" && path.count == 2) {
                    DisclosureGroup(L10n.text("分流规则"), isExpanded: expansion) { MihomoTemplateRules(store: store, path: path) }
                } else { arrayFields }
            default:
                if schema["x-tag-reference"].stringValue == "network_namespace" {
                    HStack {
                        TextField(title, text: Binding(get: { value.stringValue ?? "" }, set: { store.set(path, .string($0)) })).textFieldStyle(.roundedBorder)
                        Menu(L10n.text("选择命名空间引用")) { ForEach(store.referenceNames("network_namespace"), id: \.self) { name in Button(name) { store.set(path, .string(name)) } } }
                    }
                } else if let reference = schema["x-tag-reference"].stringValue {
                    Picker(title, selection: Binding(get: { value.stringValue ?? "" }, set: { store.set(path, .string($0)) })) {
                        Text(L10n.text("未设置")).tag("")
                        ForEach(store.referenceNames(reference), id: \.self) { Text($0).tag($0) }
                        if let current = value.stringValue, !store.referenceNames(reference).contains(current) { Text(current).tag(current) }
                    }
                } else {
                    TextField(title, text: Binding(get: { value.stringValue ?? "" }, set: { store.set(path, .string($0)) }), axis: .vertical)
                        .textFieldStyle(.roundedBorder)
                }
            }
        }
        HStack {
            Button(L10n.text("恢复默认")) { store.restoreDefault(path: path, definition: definition) }.buttonStyle(.borderless)
            Button("null") { store.set(path, .null) }.buttonStyle(.borderless).help(L10n.text("显式空值；不代表移除字段。"))
            if value == .null { Text("null").foregroundStyle(.secondary) }
            if let nativeDefault = schema.objectValue?["default"], nativeDefault.objectValue == nil, nativeDefault.arrayValue == nil {
                Text(L10n.text("默认：{0}", nativeDefault.stringValue ?? nativeDefault.pretty)).foregroundStyle(.secondary)
            } else { Text(L10n.text("未设置时由内核采用默认值")).foregroundStyle(.secondary) }
        }.font(.caption)
    }
    @ViewBuilder private var objectFields: some View {
        ForEach(members, id: \.self) { key in
            if path.isEmpty && key == "proxies" {
                LabeledContent(key, value: L10n.text("动态注入，只读"))
            } else {
                SubscriptionTemplateFields(store: store, definition: properties[key]!, path: path + [key], depth: depth + 1).padding(.vertical, 4)
            }
        }
        if let additional = schema.objectValue?["additionalProperties"], additional.objectValue != nil {
            ForEach((value.objectValue ?? [:]).keys.filter { properties[$0] == nil }.sorted(), id: \.self) { key in
                HStack(alignment: .top) {
                    SubscriptionTemplateFields(store: store, definition: additional, path: path + [key], depth: depth + 1)
                    Button(role: .destructive) {
                        store.deleteField(parent: path, key: key)
                    } label: { Image(systemName: "minus.circle") }.accessibilityLabel(L10n.text("删除字段"))
                }
            }
            HStack {
                TextField(L10n.text("新字段名称"), text: $newKey)
                Button(L10n.text("添加字段")) {
                    guard !newKey.isEmpty else { return }
                    store.set(path + [newKey], store.initialValue(additional, path: path + [newKey])); newKey = ""
                }.disabled(newKey.isEmpty)
            }
        } else {
            let unknown = (value.objectValue ?? [:]).keys.filter { properties[$0] == nil }.sorted()
            if !unknown.isEmpty {
                DisclosureGroup(L10n.text("未验证字段")) {
                    ForEach(unknown, id: \.self) { key in Text(key).textSelection(.enabled).foregroundStyle(.orange) }
                    Text(L10n.text("未验证字段：保留原值，可通过源码编辑。"))
                }
            }
        }
    }
    @ViewBuilder private var arrayFields: some View {
        let items = value.arrayValue ?? []
        DisclosureGroup("\(L10n.text("数组项目")) (\(items.count))", isExpanded: expansion) {
            ForEach(items.indices, id: \.self) { i in
                VStack(alignment: .leading) {
                    HStack {
                        Text("\(i + 1)").foregroundStyle(.secondary)
                        Spacer()
                        Button { move(i, by: -1) } label: { Image(systemName: "arrow.up") }.disabled(i == 0).accessibilityLabel(L10n.text("上移"))
                        Button { move(i, by: 1) } label: { Image(systemName: "arrow.down") }.disabled(i + 1 == items.count).accessibilityLabel(L10n.text("下移"))
                        Button(role: .destructive) { store.deleteItem(path: path, index: i) } label: { Image(systemName: "minus.circle") }.accessibilityLabel(L10n.text("删除项目"))
                    }
                    SubscriptionTemplateFields(store: store, definition: schema["items"], path: path + [String(i)], depth: depth + 1)
                }.padding(8)
                Divider()
            }
            Button(L10n.text("添加项目")) { store.set(path, .array(items + [store.initialValue(schema["items"], path: path + [String(items.count)])])) }
        }
    }
    private func move(_ index: Int, by offset: Int) {
        var items = value.arrayValue ?? []; let target = index + offset
        guard items.indices.contains(target) else { return }; items.swapAt(index, target); store.set(path, .array(items))
    }
    private func variantIndex(_ variants: [JSONValue]) -> Int {
        variants.firstIndex { candidate in store.schema.resolve(candidate, value: value) == schema } ?? 0
    }
    private func variantName(_ candidate: JSONValue, index: Int) -> String {
        let resolved = store.schema.resolve(candidate)
        let constants = resolved["properties"].objectValue?.values.compactMap { $0["const"].stringValue } ?? []
        return constants.first ?? resolved["title"].stringValue ?? resolved["type"].stringValue ?? "\(index + 1)"
    }
}
