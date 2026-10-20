import SwiftUI

/// Native typed controls are generated from the same versioned field definitions as the server.
/// Unverified fields remain visible in source and are never presented as validated controls.
struct SubscriptionTemplateFields: View {
    @Bindable var store: SubscriptionTemplateStore
    let definition: JSONValue
    let path: [String]
    var depth = 0
    var isEditorRoot = false
    var showsFieldHeader = true
    var expandsObjectInline = false
    @State private var editingObject = false
    private var selectedItem: Int? {
        get { store.collectionSelections[SubscriptionTemplateStore.pointer(path)].flatMap(Int.init) }
        nonmutating set { store.collectionSelections[SubscriptionTemplateStore.pointer(path)] = newValue.map(String.init) }
    }
    private var selectedKey: String? {
        get { store.collectionSelections[SubscriptionTemplateStore.pointer(path)] }
        nonmutating set { store.collectionSelections[SubscriptionTemplateStore.pointer(path)] = newValue }
    }
    // Preview an unset field without materializing it in the document.
    // The control's setter creates an explicit value only when the user edits it.
    private var editingDefinition: JSONValue { store.schema.editorDefinition(definition, path: path) }
    private var value: JSONValue { store.editorValue(editingDefinition, path: path) }
    private var schema: JSONValue { store.schema.resolve(editingDefinition, value: value) }
    private var properties: [String: JSONValue] { schema["properties"].objectValue ?? [:] }
    private var title: String { path.last ?? "" }
    private var displaysObjectInline: Bool { expandsObjectInline || title == "default_domain_resolver" }
    private var usesCollectionOverview: Bool {
        title == "domain_resolver" || (path.first == "dns" && title == "predefined")
    }
    private var scalarListItemDefinition: JSONValue? {
        store.schema.scalarListItemDefinition(editingDefinition)
    }
    @ViewBuilder private var fieldActionItems: some View {
        Button(L10n.text("恢复默认")) { store.restoreDefault(path: path, definition: definition) }
        Button(L10n.text("显式 null")) { store.set(path, .null) }
    }
    private var enumChoices: [JSONValue]? {
        guard var values = schema["enum"].arrayValue ?? schema["x-enum-choices"].arrayValue else { return nil }
        if store.effective.at(path) != nil, !values.contains(value) { values.append(value) }
        return values.reduce(into: []) { result, choice in
            if !result.contains(where: { $0.pretty == choice.pretty }) { result.append(choice) }
        }
    }
    private var usesNativeControl: Bool {
        let variants = store.schema.variants(editingDefinition)
        return schema["x-unverified"].boolValue != true
            && schema.objectValue?["const"] == nil
            && variants.count <= 1
            && (["boolean", "string", "integer", "number"].contains(schema["type"].stringValue ?? "") || enumChoices?.isEmpty == false)
    }
    private var nativeControl: some View {
        LabeledContent {
            nativeValueControl.labelsHidden()
                .accessibilityIdentifier("templates.value.\(SubscriptionTemplateStore.pointer(path))")
        } label: {
            TemplateFieldLabel(store: store, path: path, title: schema["title"].stringValue ?? title, value: value)
        }
    }
    @ViewBuilder private var nativeValueControl: some View {
        let label = schema["title"].stringValue ?? title
        if let values = enumChoices, !values.isEmpty {
            enumPicker(label, values: values)
        } else {
            switch schema["type"].stringValue {
            case "boolean":
                Toggle(label, isOn: Binding(get: { value.boolValue ?? false }, set: { store.set(path, .bool($0)) }))
                    .toggleStyle(.switch)
            case "integer":
                TextField(label, text: Binding(get: { value.stringValue ?? value.pretty }, set: { text in
                    if let number = Int(text) { store.set(path, .integer(number)) }
                    else if let number = UInt64(text) { store.set(path, .unsignedInteger(number)) }
                    else { store.set(path, .string(text)) }
                })).textFieldStyle(.roundedBorder)
            case "number":
                TextField(label, value: Binding(get: {
                    if case .decimal(let number) = value { return number }
                    return Double(value.integerValue ?? 0)
                }, set: { store.set(path, .decimal($0)) }), format: .number).textFieldStyle(.roundedBorder)
            default:
                let reference = TemplateSchema.referenceKind(schema, path: path)
                if let reference, reference != "network_namespace" {
                    referencePicker(label, kind: reference)
                } else if reference == "network_namespace" {
                    HStack {
                        stringInput(label)
                        Menu(L10n.text("选择命名空间引用")) {
                            ForEach(store.referenceNames("network_namespace"), id: \.self) { name in
                                Button(name) { store.set(path, .string(name)) }
                            }
                        }
                    }
                } else {
                    stringInput(label)
                }
            }
        }
    }
    private func stringInput(_ label: String) -> some View {
        let isMark = ["routing_mark", "default_mark"].contains(title)
        return TextField(label, text: Binding(get: {
            value.stringValue ?? (isMark && value != .null ? value.pretty : "")
        }, set: { store.set(path, .string($0)) }), prompt: isMark ? Text(verbatim: "0x1234") : nil, axis: .vertical)
            .textFieldStyle(.roundedBorder)
            .frame(minWidth: 0, idealWidth: 200, maxWidth: .infinity)
    }
    private func enumPicker(_ label: String, values: [JSONValue]) -> some View {
        Picker(label, selection: Binding(get: { value.pretty }, set: { token in
            guard let choice = values.first(where: { $0.pretty == token }) else { return }
            store.set(path, choice)
        })) {
            if values.isEmpty { Text(L10n.text("未设置")).tag(value.pretty) }
            ForEach(values, id: \.pretty) { choice in
                Text(TemplateSchema.pickerTitle(choice)).tag(choice.pretty)
            }
        }.disabled(values.isEmpty)
    }
    /// Unset removes the key instead of storing an empty string, and an unknown existing tag
    /// stays visible without adding a second blank choice next to "未设置".
    @ViewBuilder private func referencePicker(_ label: String, kind: String) -> some View {
        let names = store.referenceNames(kind)
        Picker(label, selection: Binding(get: { value.stringValue ?? "" }, set: { next in
            if next.isEmpty {
                store.setState(store.layer == "common" ? "inherit" : "remove", path: path, schema: definition)
            } else {
                store.set(path, .string(next))
            }
        })) {
            Text(L10n.text("未设置")).tag("")
            ForEach(names, id: \.self) { Text(TemplateSchema.pickerTitle(.string($0))).tag($0) }
            if let current = value.stringValue, !current.isEmpty, !names.contains(current) { Text(TemplateSchema.pickerTitle(.string(current))).tag(current) }
        }
    }
    private var isCacheFile: Bool { path == ["experimental", "cache_file"] }
    private var showsObjectDetails: Bool {
        !isCacheFile || (store.effective.at(path + ["enabled"]) ?? store.initialValue(properties["enabled"] ?? .null, path: path + ["enabled"])).boolValue == true
    }
    private var members: [String] {
        let keys = properties.keys.sorted()
        guard isCacheFile, properties["enabled"] != nil else { return keys }
        return ["enabled"] + (showsObjectDetails ? keys.filter { $0 != "enabled" } : [])
    }
    private var optimisticEnabled: Bool {
        value["enabled"].boolValue ?? value.boolValue ?? false
    }
    private func setOptimistic(_ key: String, _ next: JSONValue) {
        var object = value.objectValue ?? ["enabled": .bool(optimisticEnabled)]
        if object["timeout"] == nil { object["timeout"] = .string("3d") }
        object[key] = next
        store.set(path, .object(object))
    }
    @ViewBuilder private var optimisticFields: some View {
        Toggle("optimistic_enable", isOn: Binding(get: { optimisticEnabled }, set: { setOptimistic("enabled", .bool($0)) }))
            .toggleStyle(.switch)
            .accessibilityIdentifier("templates.value.\(SubscriptionTemplateStore.pointer(path + ["enabled"]))")
        if optimisticEnabled {
            TextField("optimistic_timeout", text: Binding(get: { value["timeout"].stringValue ?? "3d" }, set: { setOptimistic("timeout", .string($0)) }))
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("templates.value.\(SubscriptionTemplateStore.pointer(path + ["timeout"]))")
        }
    }
    var body: AnyView {
        if path == ["dns", "optimistic"] {
            return AnyView(optimisticFields
                .disabled(!store.allowed(path, value: value))
                .contextMenu { fieldActionItems.disabled(!store.allowed(path, value: value)) }
                .id(SubscriptionTemplateStore.pointer(path)))
        }
        return AnyView(VStack(alignment: .leading, spacing: 8) {
            if depth < 24 {
                if path.isEmpty { objectFields }
                else if usesNativeControl {
                    nativeControl
                        .disabled(!store.allowed(path, value: value))
                        .help(schema["description"].stringValue ?? title)
                }
                else if isEditorRoot { control }
                else {
                    if showsFieldHeader {
                        HStack {
                            TemplateFieldLabel(store: store, path: path, title: schema["title"].stringValue ?? title, value: value)
                            if schema["title"].stringValue != nil { Text(title).font(.caption.monospaced()).foregroundStyle(.secondary) }
                        }
                    }
                    if let description = schema["description"].stringValue { Text(description).font(.caption).foregroundStyle(.secondary) }
                    if !store.allowed(path, value: value) {
                        Text(L10n.text("此字段由目标客户端管理。"))
                            .font(.caption).foregroundStyle(.orange)
                    }
                    control.disabled(!store.allowed(path, value: value))
                }
            } else { Text(L10n.text("嵌套层级过深，请在源码中编辑。")) }
        }.accessibilityElement(children: .contain)
        .contextMenu {
            fieldActionItems.disabled(schema["x-unverified"].boolValue == true || !store.allowed(path, value: value))
        }
        .onAppear { selectFocused(store.focusedPath) }
        .id(SubscriptionTemplateStore.pointer(path))
        .onChange(of: store.focusGeneration) { _, _ in
            selectFocused(store.focusedPath)
        }
        .sheet(isPresented: $editingObject) {
            TemplateStructuredFieldEditor(store: store, definition: editingDefinition, path: path, depth: depth, title: schema["title"].stringValue ?? title)
        })
    }
    private func selectFocused(_ pointer: String) {
        let target = SubscriptionTemplateStore.path(pointer)
        if target.starts(with: path), target.count > path.count {
            if let index = Int(target[path.count]) { selectedItem = index } else { selectedKey = target[path.count] }
            if schema["type"].stringValue == "object", !isEditorRoot, !displaysObjectInline, !usesCollectionOverview, path != ["dns", "optimistic"] {
                editingObject = true
            }
        }
    }
    @ViewBuilder private var control: some View {
        let variants = store.schema.variants(editingDefinition)
        if variants.count > 1, scalarListItemDefinition == nil {
            Picker(L10n.text("配置类型"), selection: Binding(get: { variantIndex(variants) }, set: { index in
                guard variants.indices.contains(index) else { return }
                store.set(path, store.schema.selecting(variants[index], from: definition, value: value))
            })) {
                ForEach(Array(variants.enumerated()), id: \.offset) { index, variant in
                    Text(store.schema.variantTitle(variant, index: index)).tag(index)
                }
            }
        }
        if schema["x-unverified"].boolValue == true {
            Text(L10n.text("未验证字段：保留原值，可通过源码编辑。"))
                .foregroundStyle(.orange).font(.caption)
        } else if let itemDefinition = scalarListItemDefinition {
            TemplateScalarListEditor(store: store, path: path, itemDefinition: itemDefinition, allowsScalar: true)
        } else if let values = enumChoices {
            enumPicker(title, values: values)
        } else if let constant = schema.objectValue?["const"] {
            Text(TemplateSchema.pickerTitle(constant)).foregroundStyle(.secondary)
        } else {
            switch schema["type"].stringValue ?? value.schemaType {
            case "boolean": Toggle(title, isOn: Binding(get: { value.boolValue ?? false }, set: { store.set(path, .bool($0)) })).toggleStyle(.switch).labelsHidden()
            case "integer": TextField(title, text: Binding(get: { value.stringValue ?? value.pretty }, set: { text in
                if let number = Int(text) { store.set(path, .integer(number)) }
                else if let number = UInt64(text) { store.set(path, .unsignedInteger(number)) }
                else { store.set(path, .string(text)) }
            })).textFieldStyle(.roundedBorder).accessibilityIdentifier("templates.value.\(SubscriptionTemplateStore.pointer(path))")
            case "number": TextField(title, value: Binding(get: { if case .decimal(let n) = value { return n }; return Double(value.integerValue ?? 0) }, set: { store.set(path, .decimal($0)) }), format: .number).textFieldStyle(.roundedBorder).accessibilityIdentifier("templates.value.\(SubscriptionTemplateStore.pointer(path))")
            case "object":
                if isEditorRoot || displaysObjectInline { objectFields }
                else if TemplateSchema.isObjectResolver(path) {
                    TemplateCollectionFields(store: store, definition: editingDefinition, path: path, depth: depth, isSingleObject: true)
                } else if path.first == "dns", title == "predefined" {
                    TemplateMapFields(store: store, definition: schema["additionalProperties"], path: path, depth: depth)
                }
                else {
                    HStack {
                        Text(L10n.text("共 {0} 个字段", String(value.objectValue?.count ?? 0))).foregroundStyle(.secondary)
                        Spacer()
                        Button(L10n.text("编辑…"), systemImage: "pencil") { editingObject = true }
                            .accessibilityIdentifier("templates.object.edit.\(SubscriptionTemplateStore.pointer(path))")
                    }
                }
            case "array":
                if store.selected?.format == "mihomo", path == ["rules"] || (path.first == "sub-rules" && path.count == 2) {
                    MihomoTemplateRules(store: store, path: path)
                        .accessibilityIdentifier("templates.collection.\(SubscriptionTemplateStore.pointer(path))")
                } else { arrayFields }
            default:
                let reference = TemplateSchema.referenceKind(schema, path: path)
                if reference == "network_namespace" {
                    HStack {
                        TextField(title, text: Binding(get: { value.stringValue ?? "" }, set: { store.set(path, .string($0)) })).textFieldStyle(.roundedBorder)
                        Menu(L10n.text("选择命名空间引用")) { ForEach(store.referenceNames("network_namespace"), id: \.self) { name in Button(name) { store.set(path, .string(name)) } } }
                    }
                } else if let reference {
                    referencePicker(title, kind: reference)
                } else {
                    TextField(title, text: Binding(get: { value.stringValue ?? "" }, set: { store.set(path, .string($0)) }), axis: .vertical)
                        .textFieldStyle(.roundedBorder)
                }
            }
        }
        if schema["x-unverified"].boolValue != true, !isEditorRoot, scalarListItemDefinition == nil, !["object", "array"].contains(schema["type"].stringValue ?? value.schemaType) {
        HStack {
            if value == .null { Text("null").foregroundStyle(.secondary) }
            if let nativeDefault = schema.objectValue?["default"], nativeDefault.objectValue == nil, nativeDefault.arrayValue == nil {
                Text(L10n.text("默认：{0}", nativeDefault.stringValue ?? nativeDefault.pretty)).foregroundStyle(.secondary)
            } else { Text(L10n.text("未设置时由内核采用默认值")).foregroundStyle(.secondary) }
        }.font(.caption)
        }
    }
    @ViewBuilder private var objectFields: some View {
        let properties = self.properties
        let dialerKeys = TemplateFieldLayout.dialerFields(in: members)
        ForEach(members.filter { !dialerKeys.contains($0) }, id: \.self) { key in
            if path.isEmpty && key == "proxies" {
                LabeledContent(key, value: L10n.text("动态注入，只读"))
            } else if let field = properties[key] {
                SubscriptionTemplateFields(store: store, definition: field, path: path + [key], depth: depth + 1).padding(.vertical, 4)
            }
        }
        if !dialerKeys.isEmpty {
            TemplateDialerSettings(store: store, path: path, title: schema["title"].stringValue ?? title) {
                ForEach(dialerKeys, id: \.self) { key in
                    if let field = properties[key] {
                        SubscriptionTemplateFields(store: store, definition: field, path: path + [key], depth: depth + 1).padding(.vertical, 4)
                    }
                }
            }
        }
        if showsObjectDetails {
            if let additional = schema.objectValue?["additionalProperties"], additional.objectValue != nil {
                TemplateMapFields(store: store, definition: additional, path: path, knownKeys: Set(properties.keys), depth: depth)
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
    }
    @ViewBuilder private var arrayFields: some View {
        let itemSchema = store.schema.resolve(schema["items"])
        let itemVariants = store.schema.variants(schema["items"])
        if ["string", "integer"].contains(itemSchema["type"].stringValue ?? ""), itemVariants.count <= 1,
           itemSchema["x-unverified"].boolValue != true,
           (value.arrayValue ?? []).allSatisfy({ $0.schemaType == itemSchema["type"].stringValue }) {
            TemplateScalarListEditor(store: store, path: path, itemDefinition: schema["items"])
        } else {
            TemplateCollectionFields(store: store, definition: schema["items"], path: path, depth: depth)
        }
    }
    private func variantIndex(_ variants: [JSONValue]) -> Int {
        variants.firstIndex { candidate in store.schema.resolve(candidate, value: value) == schema } ?? 0
    }
}

struct TemplateFieldLabel: View {
    @Bindable var store: SubscriptionTemplateStore
    let path: [String]
    let title: String
    var value: JSONValue? = nil
    private var platforms: [String] { store.supportedPlatforms(path, value: value ?? store.effective.at(path)) }
    var body: some View {
        HStack(spacing: 6) {
            Text(title).textSelection(.enabled)
            if store.schema.isRequired(path, value: store.effective) {
                Text(verbatim: "*").foregroundStyle(.red)
                    .accessibilityLabel(L10n.text("必填"))
                    .accessibilityIdentifier("templates.required.\(SubscriptionTemplateStore.pointer(path))")
            }
            if platforms.count < TemplatePlatform.all.count {
                if platforms.isEmpty {
                    tag(L10n.text("客户端管理"))
                } else {
                    ForEach(platforms, id: \.self) { platform in tag(TemplatePlatform.title(platform)) }
                }
            }
        }
    }
    private func tag(_ title: String) -> some View {
        Text(title).font(.caption2).foregroundStyle(.secondary)
            .padding(.horizontal, 6).padding(.vertical, 2)
            .background(.quaternary, in: Capsule())
            .fixedSize()
            .accessibilityIdentifier("templates.platformTag.\(SubscriptionTemplateStore.pointer(path))")
    }
}
