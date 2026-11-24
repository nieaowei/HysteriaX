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
    @State private var integerDraft: String?
    private var selectedItem: Int? {
        get { store.collectionSelections[SubscriptionTemplateStore.pointer(path)].flatMap(Int.init) }
        nonmutating set { store.collectionSelections[SubscriptionTemplateStore.pointer(path)] = newValue.map(String.init) }
    }
    private var selectedKey: String? {
        get { store.collectionSelections[SubscriptionTemplateStore.pointer(path)] }
        nonmutating set { store.collectionSelections[SubscriptionTemplateStore.pointer(path)] = newValue }
    }
    private var pointer: String { SubscriptionTemplateStore.pointer(path) }
    private var title: String { path.last ?? "" }
    private func displaysObjectInline(for presentation: SubscriptionTemplateStore.TemplateFieldPresentation) -> Bool {
        expandsObjectInline || TemplateEditorGuidance.isInlineObject(path, schema: store.schema)
    }
    private var usesCollectionOverview: Bool {
        title == "domain_resolver" || (path.first == "dns" && title == "predefined")
    }
    private func showsVariantPicker(for presentation: SubscriptionTemplateStore.TemplateFieldPresentation) -> Bool {
        guard !path.isEmpty, presentation.variants.count > 1, presentation.scalarListItemDefinition == nil else { return false }
        // A collapsed object delegates type editing to its detail editor. Mixed
        // string/object references still need this picker to select a shared tag.
        let delegatesToDetail = presentation.schema["type"].stringValue == "object"
            && !isEditorRoot
            && !displaysObjectInline(for: presentation)
            && presentation.schema["x-unverified"].boolValue != true
            && presentation.variants.allSatisfy { store.schema.resolve($0)["type"].stringValue == "object" }
        return !delegatesToDetail
    }
    @ViewBuilder private func fieldActionItems(allowed: Bool, isUnverified: Bool) -> some View {
        if let url = TemplateEditorGuidance.documentURL(path, schema: store.schema, value: store.effective) {
            Link(L10n.text("查看配置文档"), destination: url)
        }
        Button(L10n.text("恢复默认")) { store.restoreDefault(path: path, definition: definition) }
        Button(L10n.text("显式 null")) { store.set(path, .null) }
    }
    private func nativeControl(presentation: SubscriptionTemplateStore.TemplateFieldPresentation) -> some View {
        LabeledContent {
            nativeValueControl(presentation: presentation).labelsHidden()
                .accessibilityIdentifier("templates.value.\(pointer)")
        } label: {
            TemplateFieldLabel(
                store: store,
                path: path,
                title: presentation.schema["title"].stringValue ?? title,
                value: presentation.value,
                showsResetAction: true,
                onReset: { store.restoreDefault(path: path, definition: definition) },
                presentation: presentation
            )
        }
    }
    @ViewBuilder private func nativeValueControl(presentation: SubscriptionTemplateStore.TemplateFieldPresentation) -> some View {
        let label = presentation.schema["title"].stringValue ?? title
        if let values = presentation.enumChoices, !values.isEmpty {
            enumPicker(label, presentation: presentation)
        } else {
            switch presentation.schema["type"].stringValue {
            case "boolean":
                Toggle(label, isOn: Binding(get: { presentation.value.boolValue ?? false }, set: { store.set(path, .bool($0)) }))
                    .toggleStyle(.switch)
            case "integer":
                integerInput(label, presentation: presentation)
            case "number":
                TextField(label, value: Binding(get: {
                    if case .decimal(let number) = presentation.value { return number }
                    return Double(presentation.value.integerValue ?? 0)
                }, set: { store.set(path, .decimal($0)) }), format: .number).textFieldStyle(.roundedBorder)
            default:
                if let reference = presentation.referenceKind, reference != "network_namespace" {
                    referencePicker(label, kind: reference, presentation: presentation)
                } else if presentation.referenceKind == "network_namespace" {
                    HStack {
                        stringInput(label, presentation: presentation)
                        Menu(L10n.text("选择命名空间引用")) {
                            ForEach(store.referenceNames("network_namespace"), id: \.self) { name in
                                Button(name) { store.set(path, .string(name)) }
                            }
                        }
                    }
                } else {
                    stringInput(label, presentation: presentation)
                }
            }
        }
    }
    private func integerInput(_ label: String, presentation: SubscriptionTemplateStore.TemplateFieldPresentation) -> some View {
        let fixed = presentation.schema["x-editor-fixed-scalar"] == .string("integer")
        let value = presentation.value
        return VStack(alignment: .leading, spacing: 4) {
            TextField(label, text: Binding(get: {
                integerDraft ?? (presentation.hasPreviewValue ? (value.stringValue ?? value.scalarText) : "")
            }, set: { text in
                if fixed { integerDraft = text }
                if let number = TemplateSchema.integerValue(text) { store.set(path, number) }
                else if !fixed { store.setString(path, text, definition: definition) }
            })).textFieldStyle(.roundedBorder)
            if fixed, let draft = integerDraft, TemplateSchema.integerValue(draft) == nil {
                Text(L10n.text("请输入整数。")).font(.caption).foregroundStyle(.orange)
            } else if fixed, value.stringValue != nil {
                Text(L10n.text("原有字符串值已保留；修改时请输入整数。"))
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
        .onChange(of: value) { _, _ in integerDraft = nil }
    }
    @ViewBuilder
    private func stringInput(_ label: String, presentation: SubscriptionTemplateStore.TemplateFieldPresentation) -> some View {
        let isMark = title.hasSuffix("mark")
        let value = presentation.value
        let currentText = value.stringValue ?? (isMark && value != .null ? value.scalarText : "")
        let prompt = isMark ? "0x1234" : (presentation.isSingbox ? TemplateEditorGuidance.prompt(definition, path: path) : nil)
        let isMultiline = ["certificate", "key", "private_key"].contains(title) || currentText.contains("\n")
        let binding = Binding(get: { currentText }, set: { store.setString(path, $0, definition: definition) })
        if isMultiline {
            TextField(label, text: binding, prompt: prompt.map { Text(verbatim: $0) }, axis: .vertical)
                .textFieldStyle(.roundedBorder)
                .frame(minWidth: 0, idealWidth: 200, maxWidth: .infinity)
        } else {
            TextField(label, text: binding, prompt: prompt.map { Text(verbatim: $0) })
                .textFieldStyle(.roundedBorder)
                .frame(minWidth: 0, idealWidth: 200, maxWidth: .infinity)
        }
    }
    private func enumPicker(_ label: String, presentation: SubscriptionTemplateStore.TemplateFieldPresentation) -> some View {
        let options = presentation.enumOptions ?? TemplateEditorGuidance.enumPickerOptions(presentation.schema, path: path, schema: store.schema, value: store.effective)
        return Picker(label, selection: Binding(get: { options.selectionToken }, set: { token in
            if options.includesUnset && token == TemplateEnumPickerOptions.unsetToken {
                store.setState("inherit", path: path, schema: definition)
                return
            }
            guard let choice = options.values.first(where: { $0.fastToken == token }) else { return }
            store.set(path, choice)
        })) {
            if options.includesUnset {
                Text(TemplateEditorGuidance.enumPickerTitle(nil, path: path, schema: store.schema)).tag(TemplateEnumPickerOptions.unsetToken)
            }
            ForEach(options.values, id: \.fastToken) { choice in
                Text(TemplateEditorGuidance.enumPickerTitle(choice, path: path, schema: store.schema)).tag(choice.fastToken)
            }
        }.disabled(presentation.enumChoices?.isEmpty != false)
    }
    /// Unset removes the key instead of storing an empty string, and an unknown existing tag
    /// stays visible without adding a second blank choice next to "未设置".
    @ViewBuilder private func referencePicker(_ label: String, kind: String, presentation: SubscriptionTemplateStore.TemplateFieldPresentation) -> some View {
        let names = store.referenceNames(kind)
        let current = presentation.value.stringValue
        Picker(label, selection: Binding(get: { current ?? "" }, set: { next in
            if next.isEmpty {
                store.setState("inherit", path: path, schema: definition)
            } else {
                store.set(path, .string(next))
            }
        })) {
            Text(L10n.text("未设置")).tag("")
            ForEach(names, id: \.self) { Text(TemplateSchema.pickerTitle(.string($0))).tag($0) }
            if let current, !current.isEmpty, !names.contains(current) { Text(TemplateSchema.pickerTitle(.string(current))).tag(current) }
        }
    }
    private var isCacheFile: Bool { path == ["experimental", "cache_file"] }
    private func showsObjectDetails(schema: JSONValue) -> Bool {
        let properties = schema["properties"].objectValue ?? [:]
        return !isCacheFile || (store.effective.at(path + ["enabled"]) ?? store.initialValue(properties["enabled"] ?? .null, path: path + ["enabled"])).boolValue == true
    }
    private func members(for presentation: SubscriptionTemplateStore.TemplateFieldPresentation) -> [String] {
        let schema = presentation.schema
        let properties = schema["properties"].objectValue ?? [:]
        let owned = showsVariantPicker(for: presentation) ? store.schema.variantFieldKeys(presentation.editingDefinition, value: presentation.value) : []
        let visible = TemplateEditorGuidance.visibleKeys(schema, path: path, schema: store.schema, value: store.effective).filter { !owned.contains($0) }
        let keys = TemplateEditorGuidance.ordered(visible, path: path, schema: store.schema, value: store.effective)
        guard isCacheFile, properties["enabled"] != nil else { return keys }
        return ["enabled"] + (showsObjectDetails(schema: schema) ? keys.filter { $0 != "enabled" } : [])
    }
    private func optimisticEnabled(value: JSONValue) -> Bool {
        value["enabled"].boolValue ?? value.boolValue ?? false
    }
    private func setOptimistic(_ key: String, _ next: JSONValue, current: JSONValue) {
        var object = current.objectValue ?? ["enabled": .bool(optimisticEnabled(value: current))]
        if key != "timeout", object["timeout"] == nil { object["timeout"] = .string("3d") }
        if key == "timeout", next.stringValue?.isEmpty == true { object[key] = nil }
        else { object[key] = next }
        store.set(path, .object(object))
    }
    @ViewBuilder private func optimisticFields(presentation: SubscriptionTemplateStore.TemplateFieldPresentation) -> some View {
        let enabled = optimisticEnabled(value: presentation.value)
        Toggle("optimistic_enable", isOn: Binding(get: { enabled }, set: { setOptimistic("enabled", .bool($0), current: presentation.value) }))
            .toggleStyle(.switch)
            .accessibilityIdentifier("templates.value.\(SubscriptionTemplateStore.pointer(path + ["enabled"]))")
        if enabled {
            TextField("optimistic_timeout", text: Binding(get: { presentation.value["timeout"].stringValue ?? "3d" }, set: { setOptimistic("timeout", .string($0), current: presentation.value) }))
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("templates.value.\(SubscriptionTemplateStore.pointer(path + ["timeout"]))")
        }
    }
    @ViewBuilder
    var body: some View {
        let presentation = store.fieldPresentation(path: path, definition: definition)
        let isUnverified = presentation.schema["x-unverified"].boolValue == true
        if path == ["dns", "optimistic"] {
            optimisticFields(presentation: presentation)
                .disabled(!presentation.allowed)
                .contextMenu { fieldActionItems(allowed: presentation.allowed, isUnverified: false).disabled(!presentation.allowed) }
                .id(pointer)
        } else if path.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                if depth < 24 {
                    objectFields(presentation: presentation)
                } else {
                    Text(L10n.text("嵌套层级过深，请在源码中编辑。"))
                }
            }
            .accessibilityElement(children: .contain)
            .contextMenu {
                fieldActionItems(allowed: presentation.allowed, isUnverified: isUnverified).disabled(isUnverified || !presentation.allowed)
            }
            .id(pointer)
        } else {
            let usesNative = !isUnverified
                && presentation.schema.objectValue?["const"] == nil
                && presentation.variants.count <= 1
                && (["boolean", "string", "integer", "number"].contains(presentation.schema["type"].stringValue ?? "") || presentation.enumChoices?.isEmpty == false)
            if depth < 24, usesNative, !isEditorRoot {
                nativeControl(presentation: presentation)
                    .disabled(!presentation.allowed)
                    .help(presentation.helpText)
                    .accessibilityElement(children: .contain)
                    .contextMenu {
                        fieldActionItems(allowed: presentation.allowed, isUnverified: isUnverified).disabled(isUnverified || !presentation.allowed)
                    }
                    .id(pointer)
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    if depth < 24 {
                        if isEditorRoot {
                            control(presentation: presentation)
                        } else {
                            if showsFieldHeader {
                                HStack {
                                    TemplateFieldLabel(
                                        store: store,
                                        path: path,
                                        title: presentation.schema["title"].stringValue ?? title,
                                        value: presentation.value,
                                        presentation: presentation
                                    )
                                    if presentation.schema["title"].stringValue != nil { Text(title).font(.caption.monospaced()).foregroundStyle(.secondary) }
                                }
                            }
                            if let description = presentation.schema["description"].stringValue { Text(description).font(.caption).foregroundStyle(.secondary) }
                            if !presentation.allowed {
                                Text(L10n.text("此字段由目标客户端管理。"))
                                    .font(.caption).foregroundStyle(.orange)
                            }
                            control(presentation: presentation).disabled(!presentation.allowed)
                        }
                    } else {
                        Text(L10n.text("嵌套层级过深，请在源码中编辑。"))
                    }
                }
                .accessibilityElement(children: .contain)
                .contextMenu {
                    fieldActionItems(allowed: presentation.allowed, isUnverified: isUnverified).disabled(isUnverified || !presentation.allowed)
                }
                .onAppear { selectFocused(store.focusedPath, presentation: presentation) }
                .onChange(of: store.focusGeneration) { _, _ in
                    selectFocused(store.focusedPath, presentation: presentation)
                }
                .id(pointer)
                .sheet(isPresented: $editingObject) {
                    TemplateStructuredFieldEditor(
                        store: store,
                        definition: presentation.editingDefinition,
                        path: path,
                        depth: depth,
                        title: presentation.schema["title"].stringValue ?? title
                    )
                }
            }
        }
    }
    private func selectFocused(_ pointer: String, presentation: SubscriptionTemplateStore.TemplateFieldPresentation) {
        let target = SubscriptionTemplateStore.path(pointer)
        if target.starts(with: path), target.count > path.count {
            if let index = Int(target[path.count]) { selectedItem = index } else { selectedKey = target[path.count] }
            if presentation.schema["type"].stringValue == "object",
               !isEditorRoot,
               !displaysObjectInline(for: presentation),
               !usesCollectionOverview,
               path != ["dns", "optimistic"] {
                editingObject = true
            }
        }
    }
    @ViewBuilder private func control(presentation: SubscriptionTemplateStore.TemplateFieldPresentation) -> some View {
        let variants = presentation.variants
        let schema = presentation.schema
        let value = presentation.value
        if showsVariantPicker(for: presentation) {
            Picker(L10n.text("配置类型"), selection: Binding(get: { variantIndex(variants, schema: schema, value: value) }, set: { index in
                guard variants.indices.contains(index) else { return }
                store.set(path, store.schema.selecting(variants[index], from: definition, value: value))
            })) {
                ForEach(Array(variants.enumerated()), id: \.offset) { index, variant in
                    Text(store.schema.variantTitle(variant, index: index)).tag(index)
                }
            }.accessibilityIdentifier("templates.variant.\(pointer)")
        }
        if schema["x-unverified"].boolValue == true {
            Text(L10n.text("未验证字段：保留原值，可通过源码编辑。"))
                .foregroundStyle(.orange).font(.caption)
        } else if let itemDefinition = presentation.scalarListItemDefinition {
            TemplateScalarListEditor(store: store, path: path, itemDefinition: itemDefinition, allowsScalar: true)
        } else if presentation.enumChoices != nil {
            enumPicker(title, presentation: presentation)
        } else if let constant = schema.objectValue?["const"] {
            Text(TemplateSchema.pickerTitle(constant)).foregroundStyle(.secondary)
                .accessibilityIdentifier("templates.constant.\(pointer)")
        } else {
            switch schema["type"].stringValue ?? value.schemaType {
            case "boolean": Toggle(title, isOn: Binding(get: { value.boolValue ?? false }, set: { store.set(path, .bool($0)) })).toggleStyle(.switch).labelsHidden()
            case "integer": integerInput(title, presentation: presentation).accessibilityIdentifier("templates.value.\(pointer)")
            case "number": TextField(title, value: Binding(get: { if case .decimal(let n) = value { return n }; return Double(value.integerValue ?? 0) }, set: { store.set(path, .decimal($0)) }), format: .number).textFieldStyle(.roundedBorder).accessibilityIdentifier("templates.value.\(pointer)")
            case "object":
                if isEditorRoot || displaysObjectInline(for: presentation) {
                    if TemplateEditorGuidance.automaticTLS(path, schema: store.schema, value: store.effective) {
                        Text(L10n.text("此位置的 TLS 由协议自动启用。"))
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    objectFields(presentation: presentation)
                }
                else if TemplateSchema.isObjectResolver(path) {
                    TemplateCollectionFields(store: store, definition: presentation.editingDefinition, path: path, depth: depth, isSingleObject: true)
                } else if path.first == "dns", title == "predefined" {
                    TemplateMapFields(store: store, definition: schema["additionalProperties"], path: path, depth: depth)
                }
                else {
                    HStack {
                        Text(L10n.text("共 {0} 个字段", String(value.objectValue?.count ?? 0))).foregroundStyle(.secondary)
                        Spacer()
                        Button(L10n.text("编辑…"), systemImage: "pencil") { editingObject = true }
                            .accessibilityIdentifier("templates.object.edit.\(pointer)")
                    }
                }
            case "array":
                if store.selected?.format == "mihomo", path == ["rules"] || (path.first == "sub-rules" && path.count == 2) {
                    MihomoTemplateRules(store: store, path: path)
                        .accessibilityIdentifier("templates.collection.\(pointer)")
                } else { arrayFields(schema: schema, value: value) }
            default:
                if presentation.referenceKind == "network_namespace" {
                    HStack {
                        TextField(title, text: Binding(get: { value.stringValue ?? "" }, set: { store.setString(path, $0, definition: definition) })).textFieldStyle(.roundedBorder)
                        Menu(L10n.text("选择命名空间引用")) { ForEach(store.referenceNames("network_namespace"), id: \.self) { name in Button(name) { store.set(path, .string(name)) } } }
                    }
                } else if let reference = presentation.referenceKind {
                    referencePicker(title, kind: reference, presentation: presentation)
                } else {
                    stringInput(title, presentation: presentation)
                }
            }
        }
        if schema["x-unverified"].boolValue != true, !isEditorRoot, presentation.scalarListItemDefinition == nil, !["object", "array"].contains(schema["type"].stringValue ?? value.schemaType) {
            HStack {
                if value == .null { Text("null").foregroundStyle(.secondary) }
                if let nativeDefault = schema.objectValue?["default"], nativeDefault.objectValue == nil, nativeDefault.arrayValue == nil {
                    Text(L10n.text("默认：{0}", nativeDefault.stringValue ?? nativeDefault.scalarText)).foregroundStyle(.secondary)
                } else { Text(L10n.text("未设置时由内核采用默认值")).foregroundStyle(.secondary) }
            }.font(.caption)
        }
    }
    @ViewBuilder private func objectFields(presentation: SubscriptionTemplateStore.TemplateFieldPresentation) -> some View {
        let schema = presentation.schema
        let properties = schema["properties"].objectValue ?? [:]
        let value = presentation.value
        let memberList = members(for: presentation)
        let dialerKeys = TemplateFieldLayout.dialerFields(in: memberList)
        ForEach(memberList.filter { !dialerKeys.contains($0) }, id: \.self) { key in
            if path.isEmpty && key == "proxies" {
                LabeledContent(key, value: L10n.text("动态注入，只读"))
            } else if let field = properties[key] {
                AnyView(SubscriptionTemplateFields(store: store, definition: field, path: path + [key], depth: depth + 1)).padding(.vertical, 4)
            }
        }
        if !dialerKeys.isEmpty {
            TemplateDialerSettings(store: store, path: path, title: schema["title"].stringValue ?? title) {
                ForEach(dialerKeys, id: \.self) { key in
                    if let field = properties[key] {
                        AnyView(SubscriptionTemplateFields(store: store, definition: field, path: path + [key], depth: depth + 1)).padding(.vertical, 4)
                    }
                }
            }
        }
        let hidden = TemplateEditorGuidance.hiddenConfiguredKeys(schema, path: path, schema: store.schema, value: store.effective)
        if !hidden.isEmpty {
            DisclosureGroup(L10n.text("当前条件下隐藏的已有配置")) {
                Text(hidden.joined(separator: "、")).font(.caption).textSelection(.enabled)
                Text(L10n.text("这些字段的值已保留。更改相关开关或模式后可继续编辑，也可在源码中查看。"))
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
        if showsObjectDetails(schema: schema) {
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
    @ViewBuilder private func arrayFields(schema: JSONValue, value: JSONValue) -> some View {
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
    private func variantIndex(_ variants: [JSONValue], schema: JSONValue, value: JSONValue) -> Int {
        variants.firstIndex { candidate in store.schema.resolve(candidate, value: value) == schema } ?? 0
    }
}

private struct FieldResetButton: View {
    let action: () -> Void
    @State private var isHovered = false
    var body: some View {
        Button(action: action) {
            Image(systemName: "arrow.counterclockwise")
                .font(.caption)
                .foregroundStyle(isHovered ? Color.primary : Color.secondary.opacity(0.65))
        }
        .buttonStyle(.borderless)
        .onHover { isHovered = $0 }
        .help(L10n.text("恢复默认"))
        .accessibilityLabel(L10n.text("恢复默认"))
    }
}

struct TemplateFieldLabel: View {
    @Bindable var store: SubscriptionTemplateStore
    let path: [String]
    let title: String
    var value: JSONValue? = nil
    var showsResetAction = false
    var onReset: (() -> Void)? = nil
    var presentation: SubscriptionTemplateStore.TemplateFieldPresentation? = nil
    @State private var showingHelp = false
    private var pointer: String { SubscriptionTemplateStore.pointer(path) }
    private var platforms: [String] { presentation?.platforms ?? store.supportedPlatforms(path, value: value ?? store.effective.at(path)) }
    private var guidance: String? { presentation != nil ? presentation?.guidance : TemplateEditorGuidance.help(path, schema: store.schema, value: store.effective) }
    private var isRequiredField: Bool { presentation?.isRequired ?? store.schema.isRequired(path, value: store.effective) }
    private var isExplicitlySet: Bool { presentation?.isExplicitlySet ?? (store.effective.at(path) != nil) }
    private var isAllowed: Bool { presentation?.allowed ?? !platforms.isEmpty }
    private var fieldIssues: [JSONValue] { store.diagnosticsByPointer[pointer] ?? [] }
    var body: some View {
        HStack(spacing: 6) {
            Text(title).textSelection(.enabled)
            if isExplicitlySet {
                Circle()
                    .fill(Color.accentColor)
                    .frame(width: 5, height: 5)
                    .help(L10n.text("已显式配置；右键或点击重置可恢复默认"))
                    .accessibilityHidden(true)
            }
            if isRequiredField {
                Text(verbatim: "*").foregroundStyle(.red)
                    .accessibilityLabel(L10n.text("必填"))
                    .accessibilityIdentifier("templates.required.\(pointer)")
            }
            if !fieldIssues.isEmpty {
                let hasError = fieldIssues.contains { ($0["severity"].stringValue ?? "warning") == "error" }
                let summary = fieldIssues.compactMap { $0["message"].stringValue }.joined(separator: "\n")
                Image(systemName: hasError ? "xmark.circle.fill" : "exclamationmark.triangle.fill")
                    .font(.caption)
                    .foregroundStyle(hasError ? Color.red : Color.orange)
                    .help(summary)
            }
            if let guidance {
                Button(L10n.text("字段说明"), systemImage: "info.circle") { showingHelp.toggle() }
                    .labelStyle(.iconOnly).buttonStyle(.borderless)
                    .help(L10n.text("字段说明"))
                    .accessibilityLabel(Text(title + " " + L10n.text("字段说明")))
                    .accessibilityIdentifier("templates.help.\(pointer)")
                    .popover(isPresented: $showingHelp) {
                        VStack(alignment: .leading, spacing: 10) {
                            Text(title).font(.headline)
                            Text(guidance).textSelection(.enabled)
                            if let url = TemplateEditorGuidance.documentURL(path, schema: store.schema, value: store.effective) {
                                Link(L10n.text("查看配置文档"), destination: url)
                            }
                            if presentation?.isSingbox ?? TemplateEditorGuidance.isSingbox(store.schema) {
                                Text(L10n.text("表单依据 sing-box 1.14.2；在线文档可能包含更新版本的字段。"))
                                    .font(.caption).foregroundStyle(.secondary)
                            }
                        }.padding(16).frame(width: 320, alignment: .leading)
                    }
            }
            if showsResetAction, isExplicitlySet, isAllowed, let onReset {
                FieldResetButton(action: onReset)
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
            .accessibilityIdentifier("templates.platformTag.\(pointer)")
    }
}
