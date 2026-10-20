import SwiftUI

struct TemplateStructuredFieldEditor: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: SubscriptionTemplateStore
    let definition: JSONValue
    let path: [String]
    let depth: Int
    let title: String
    private var value: JSONValue { store.effective.at(path) ?? .null }
    private var schema: JSONValue { store.schema.resolve(definition, value: value) }
    private var variants: [JSONValue] { store.schema.variants(definition) }
    private var properties: [String: JSONValue] { schema["properties"].objectValue ?? [:] }
    private var groups: [String] {
        Set(properties.keys.map { TemplateFieldLayout.objectSection($0, path: path) })
            .sorted { TemplateFieldLayout.objectSectionOrder.firstIndex(of: $0) ?? 99 < TemplateFieldLayout.objectSectionOrder.firstIndex(of: $1) ?? 99 }
    }
    var body: some View {
        let properties = self.properties
        let variants = self.variants
        let dialerKeys = TemplateFieldLayout.dialerFields(in: Array(properties.keys))
        VStack(spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 4) {
                    Text(title).font(.title2.bold())
                    Text(SubscriptionTemplateStore.pointer(path)).font(.caption.monospaced()).foregroundStyle(.secondary).textSelection(.enabled)
                }
                Spacer()
                Button(L10n.text("撤销"), systemImage: "arrow.uturn.backward") { store.undo() }
                    .labelStyle(.iconOnly).disabled(store.undoStack.isEmpty)
                Button(L10n.text("重做"), systemImage: "arrow.uturn.forward") { store.redo() }
                    .labelStyle(.iconOnly).disabled(store.redoStack.isEmpty)
            }.padding(20)
            Divider()
            if let error = store.error { Text(error).foregroundStyle(.red).textSelection(.enabled).padding(.horizontal, 20) }
            ScrollViewReader { reader in
            Form {
                if schema["type"].stringValue == "object", !properties.isEmpty, schema["x-unverified"].boolValue != true {
                    if TemplateHTTPClientLayout.matches(schema) {
                        TemplateHTTPClientFields(store: store, definition: schema, path: path, depth: depth)
                    } else {
                    if variants.count > 1 {
                        Section(L10n.text("配置类型")) {
                            Picker(selection: Binding(get: {
                                variants.firstIndex { store.schema.resolve($0, value: value) == schema } ?? 0
                            }, set: { index in
                                guard variants.indices.contains(index) else { return }
                                store.set(path, store.schema.selecting(variants[index], from: definition, value: value))
                            })) {
                                ForEach(Array(variants.enumerated()), id: \.offset) { index, variant in Text(store.schema.variantTitle(variant, index: index)).tag(index) }
                            } label: {
                                TemplateFieldLabel(store: store, path: path + ["type"], title: L10n.text("类型"), value: schema["properties"]["type"]["const"])
                            }.accessibilityIdentifier("templates.variant.\(SubscriptionTemplateStore.pointer(path))")
                        }
                    }
                    ForEach(groups, id: \.self) { group in
                        let ordinaryKeys = keys(in: group).filter { !dialerKeys.contains($0) }
                        if !ordinaryKeys.isEmpty {
                        Section {
                            fields(ordinaryKeys, properties: properties, group: group, variants: variants)
                        } header: {
                            if ordinaryKeys.count == 1, let key = ordinaryKeys.first, let field = properties[key], !showsHeader(for: key, definition: field, in: group) {
                                TemplateFieldLabel(store: store, path: path + [key], title: L10n.text(group))
                            } else {
                                Text(L10n.text(group))
                            }
                        }
                        }
                    }
                    if !dialerKeys.isEmpty {
                        Section {
                            TemplateDialerSettings(store: store, path: path, title: title) {
                                fields(dialerKeys, properties: properties, group: "网络连接", variants: variants)
                            }
                        }
                    }
                    }
                    if schema["additionalProperties"].objectValue != nil {
                        Section(L10n.text("自定义字段")) { TemplateMapFields(store: store, definition: schema["additionalProperties"], path: path, knownKeys: Set(properties.keys), depth: depth) }
                    } else {
                        let unknown = (value.objectValue ?? [:]).keys.filter { properties[$0] == nil }.sorted()
                        if !unknown.isEmpty {
                            Section(L10n.text("未验证字段")) {
                                Text(unknown.joined(separator: ", ")).textSelection(.enabled).foregroundStyle(.secondary)
                                Text(L10n.text("未验证字段：保留原值，可通过源码编辑。"))
                            }
                        }
                    }
                } else {
                    Section(L10n.text("内容")) {
                        SubscriptionTemplateFields(store: store, definition: definition, path: path, depth: depth, isEditorRoot: true)
                    }
                }
            }.formStyle(.grouped).disabled(store.busy || !store.allowed(path, value: value))
            .task(id: store.focusGeneration) {
                if SubscriptionTemplateStore.path(store.focusedPath).starts(with: path) {
                    DispatchQueue.main.async { reader.scrollTo(store.focusedPath, anchor: .center) }
                }
            }
            }
            Divider()
            HStack {
                Text(L10n.text("修改会应用到本地草稿")).font(.caption).foregroundStyle(.secondary)
                Spacer()
                Button(L10n.text("完成")) { dismiss() }.keyboardShortcut(.cancelAction)
            }.padding(16)
        }
        .frame(width: 800, height: 640)
        .accessibilityIdentifier("templates.complexEditor")
    }
    @ViewBuilder private func fields(_ keys: [String], properties: [String: JSONValue], group: String, variants: [JSONValue]) -> some View {
        ForEach(keys, id: \.self) { key in
            if let field = properties[key] {
                if let constant = field.objectValue?["const"] {
                    if variants.count <= 1 || key != "type" {
                        LabeledContent {
                            Text(TemplateSchema.pickerTitle(constant))
                        } label: {
                            TemplateFieldLabel(store: store, path: path + [key], title: key, value: constant)
                        }
                    }
                } else {
                    SubscriptionTemplateFields(store: store, definition: field, path: path + [key], depth: depth + 1, showsFieldHeader: showsHeader(for: key, definition: field, in: group))
                }
            }
        }
    }
    private func keys(in group: String) -> [String] {
        let required = Set(schema["required"].arrayValue?.compactMap(\.stringValue) ?? [])
        return TemplateFieldLayout.orderedObjectFields(properties.keys.filter {
            TemplateFieldLayout.objectSection($0, path: path) == group
        }, required: required)
    }
    private func showsHeader(for key: String, definition: JSONValue, in group: String) -> Bool {
        let resolved = store.schema.resolve(store.schema.editorDefinition(definition, path: path + [key]), value: value[key])
        return keys(in: group).count > 1 || !["object", "array"].contains(resolved["type"].stringValue ?? value[key].schemaType)
    }
}

struct TemplateDialerSettings<Content: View>: View {
    @Bindable var store: SubscriptionTemplateStore
    let path: [String]
    let title: String
    @ViewBuilder var content: () -> Content
    private var expansionKey: String { SubscriptionTemplateStore.pointer(path) + "/$dialer" }
    var body: some View {
        DisclosureGroup(isExpanded: Binding(get: {
            store.expandedPaths.contains(expansionKey)
        }, set: { expanded in
            if expanded { store.expandedPaths.insert(expansionKey) }
            else { store.expandedPaths.remove(expansionKey) }
        }), content: content, label: { Text(L10n.text("{0} 高级设置", title)) })
        .accessibilityIdentifier("templates.dialer." + SubscriptionTemplateStore.pointer(path))
        .onAppear { revealFocusedField() }
        .onChange(of: store.focusGeneration) { _, _ in revealFocusedField() }
    }
    private func revealFocusedField() {
        let target = SubscriptionTemplateStore.path(store.focusedPath)
        if target.starts(with: path), target.count > path.count, TemplateFieldLayout.dialerFieldOrder.contains(target[path.count]) {
            store.expandedPaths.insert(expansionKey)
        }
    }
}

struct TemplateMapFields: View {
    @Bindable var store: SubscriptionTemplateStore
    let definition: JSONValue
    let path: [String]
    var knownKeys: Set<String> = []
    let depth: Int
    @State private var newKey = ""
    @State private var editingKey: String?
    private var entryDefinition: JSONValue {
        if path.first == "dns", path.last == "predefined" {
            return .object(["type": .string("array"), "items": .object(["type": .string("string")])])
        }
        return definition
    }
    private var keys: [String] { (store.effective.at(path)?.objectValue ?? [:]).keys.filter { !knownKeys.contains($0) }.sorted() }
    private var usesListOverview: Bool { path.first == "dns" && path.last == "predefined" }
    private var selectedKey: String? {
        get { store.collectionSelections[SubscriptionTemplateStore.pointer(path)].flatMap { keys.contains($0) ? $0 : nil } }
        nonmutating set { store.collectionSelections[SubscriptionTemplateStore.pointer(path)] = newValue }
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if usesListOverview {
                if keys.isEmpty {
                    Text(L10n.text("暂无项目，点击添加开始配置。"))
                        .foregroundStyle(.secondary).frame(maxWidth: .infinity, minHeight: 60)
                } else {
                    List(selection: Binding(get: { selectedKey }, set: { selectedKey = $0 })) {
                        ForEach(keys, id: \.self) { key in
                            entryRow(key).tag(key).onTapGesture(count: 2) { open(key) }
                        }
                    }.frame(height: min(280, max(80, CGFloat(keys.count) * 54)))
                }
            } else {
                ForEach(keys, id: \.self) { key in entryRow(key) }
            }
            HStack {
                TextField(L10n.text("新字段名称"), text: $newKey)
                Button(L10n.text("添加字段"), systemImage: "plus") {
                    let key = newKey
                    store.set(path + [key], store.initialValue(entryDefinition, path: path + [key]))
                    if !isSimple(key) { open(key) }
                    newKey = ""
                }.disabled(newKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || store.effective.at(path)?.objectValue?[newKey] != nil || knownKeys.contains(newKey))
                if usesListOverview {
                    Spacer()
                    Text(L10n.text("共 {0} 项", String(keys.count))).font(.caption).foregroundStyle(.secondary)
                    Button(L10n.text("删除项目"), systemImage: "trash", role: .destructive) {
                        guard let key = selectedKey else { return }
                        store.deleteField(parent: path, key: key)
                    }.labelStyle(.iconOnly).disabled(selectedKey == nil)
                }
            }
        }
        .sheet(isPresented: Binding(get: { editingKey != nil }, set: { if !$0 { editingKey = nil } })) {
            if let key = editingKey, keys.contains(key) {
                TemplateStructuredFieldEditor(store: store, definition: entryDefinition, path: path + [key], depth: depth + 1, title: key)
            }
        }
        .onChange(of: keys) { _, next in
            if let key = editingKey, !next.contains(key) { editingKey = nil }
        }
        .onAppear { focus() }
        .onChange(of: store.focusGeneration) { _, _ in focus() }
    }
    private func entryRow(_ key: String) -> some View {
        HStack {
            if isSimple(key) {
                SubscriptionTemplateFields(store: store, definition: entryDefinition, path: path + [key], depth: depth + 1)
            } else {
                VStack(alignment: .leading, spacing: 3) {
                    Text(key).font(.body.monospaced()).textSelection(.enabled)
                    let value = store.effective.at(path + [key]) ?? .null
                    if let items = value.arrayValue {
                        Text(L10n.text("共 {0} 项", String(items.count)) + " · " + items.prefix(3).map { $0.stringValue ?? $0.pretty }.joined(separator: " · "))
                            .font(.caption).foregroundStyle(.secondary).lineLimit(2)
                    } else if let string = value.stringValue {
                        Text(string).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                    }
                }
                Spacer()
                Button(L10n.text("编辑"), systemImage: "pencil") { open(key) }
                    .labelStyle(.iconOnly).buttonStyle(.borderless)
                    .accessibilityIdentifier("templates.map.edit.\(SubscriptionTemplateStore.pointer(path)).\(key)")
            }
            if !usesListOverview {
                Button(L10n.text("删除字段"), systemImage: "trash", role: .destructive) { store.deleteField(parent: path, key: key) }.labelStyle(.iconOnly)
            }
        }
    }
    private func open(_ key: String) { selectedKey = key; editingKey = key }
    private func focus() {
        let target = SubscriptionTemplateStore.path(store.focusedPath)
        if target.starts(with: path), target.count > path.count, keys.contains(target[path.count]), !isSimple(target[path.count]) { open(target[path.count]) }
    }
    private func isSimple(_ key: String) -> Bool {
        let schema = store.schema.resolve(entryDefinition, value: store.effective.at(path + [key]) ?? .null)
        if schema["x-unverified"].boolValue != true, store.schema.scalarListItemDefinition(entryDefinition) != nil { return true }
        return ["string", "integer", "number", "boolean"].contains(schema["type"].stringValue ?? "")
            && schema["x-unverified"].boolValue != true
            && schema.objectValue?["const"] == nil
            && store.schema.variants(entryDefinition).count <= 1
    }
}
