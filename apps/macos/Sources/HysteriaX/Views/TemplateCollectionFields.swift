import SwiftUI

struct TemplateCollectionFields: View {
    @Bindable var store: SubscriptionTemplateStore
    let definition: JSONValue
    let path: [String]
    let depth: Int
    var isSingleObject = false
    @State private var editing: Item?
    private struct Item: Identifiable { let index: Int; var id: Int { index } }
    private var items: [JSONValue] {
        if isSingleObject {
            guard let value = store.effective.at(path), value != .null else { return [] }
            return [store.editorValue(definition, path: path)]
        }
        return store.effective.at(path)?.arrayValue ?? []
    }
    private var selected: Int? {
        get {
            let saved = store.collectionSelections[SubscriptionTemplateStore.pointer(path)].flatMap(Int.init)
            return saved.flatMap { items.indices.contains($0) ? $0 : nil }
        }
        nonmutating set { store.collectionSelections[SubscriptionTemplateStore.pointer(path)] = newValue.map(String.init) }
    }
    private var variants: [JSONValue] { store.schema.variants(definition) }
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if items.isEmpty {
                Text(L10n.text("暂无项目，点击添加开始配置。"))
                    .foregroundStyle(.secondary).frame(maxWidth: .infinity, minHeight: 60)
            } else {
                List(selection: Binding(get: { selected }, set: { selected = $0 })) {
                    // SwiftUI may refresh a removed row after the store has changed.
                    // Keep its value with the row instead of subscripting the live collection.
                    ForEach(Array(items.enumerated()), id: \.offset) { index, value in
                        HStack {
                            Text(String(index + 1)).monospacedDigit().foregroundStyle(.secondary).frame(width: 28)
                            VStack(alignment: .leading, spacing: 3) {
                                Text(title(value, index: index)).lineLimit(1)
                                Text(summary(value)).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                            }
                            Spacer()
                            Button(L10n.text("编辑"), systemImage: "pencil") { open(index) }
                                .labelStyle(.iconOnly).buttonStyle(.borderless)
                                .accessibilityIdentifier("templates.item.edit.\(SubscriptionTemplateStore.pointer(path)).\(index)")
                        }
                        .tag(index)
                        .onTapGesture(count: 2) { open(index) }
                    }
                }.frame(height: min(280, max(80, CGFloat(items.count) * 54)))
            }
            HStack {
                if isSingleObject && !items.isEmpty {
                    EmptyView()
                } else if variants.count > 1 {
                    Menu {
                        ForEach(Array(variants.enumerated()), id: \.offset) { index, variant in
                            Button(store.schema.variantTitle(variant, index: index)) { add(variant) }
                        }
                    } label: { Label(L10n.text("添加项目"), systemImage: "plus") }
                    .accessibilityIdentifier("templates.item.add.\(SubscriptionTemplateStore.pointer(path))")
                } else {
                    Button(L10n.text("添加项目"), systemImage: "plus") { add(definition) }
                        .accessibilityIdentifier("templates.item.add.\(SubscriptionTemplateStore.pointer(path))")
                }
                Spacer()
                Text(L10n.text("共 {0} 项", String(items.count))).font(.caption).foregroundStyle(.secondary)
                if !isSingleObject {
                    Button(L10n.text("上移"), systemImage: "arrow.up") { move(-1) }.labelStyle(.iconOnly).disabled(selected == nil || selected == 0)
                    Button(L10n.text("下移"), systemImage: "arrow.down") { move(1) }.labelStyle(.iconOnly).disabled(selected == nil || selected == items.count - 1)
                }
                Button(L10n.text("删除项目"), systemImage: "trash", role: .destructive) {
                    guard let index = selected else { return }
                    let count = items.count
                    if isSingleObject, let key = path.last { store.deleteField(parent: Array(path.dropLast()), key: key) }
                    else { store.deleteItem(path: path, index: index) }
                    if items.count < count { selected = items.isEmpty ? nil : min(index, items.count - 1) }
                }.labelStyle(.iconOnly).disabled(selected == nil)
            }
        }
        .sheet(item: $editing) { item in
            if items.indices.contains(item.index) {
                TemplateStructuredFieldEditor(store: store, definition: definition, path: isSingleObject ? path : path + [String(item.index)], depth: depth + 1, title: title(items[item.index], index: item.index))
            }
        }
        .onChange(of: items.count) { _, _ in
            if let item = editing, !items.indices.contains(item.index) { editing = nil }
        }
        .onAppear { focus() }
        .onChange(of: store.focusGeneration) { _, _ in focus() }
    }
    private func title(_ value: JSONValue, index: Int) -> String {
        if isSingleObject { return value["server"].stringValue.flatMap { $0.isEmpty ? nil : $0 } ?? path.last ?? "" }
        return value["name"].stringValue ?? value["tag"].stringValue ?? value["type"].stringValue ?? (value.objectValue == nil && value.arrayValue == nil ? value.pretty : nil) ?? L10n.text("项目 {0}", String(index + 1))
    }
    private func summary(_ value: JSONValue) -> String {
        if path == ["http_clients"] {
            let engine = value["engine"].stringValue ?? "go"
            let version = TemplateHTTPClientLayout.isApple(value) ? L10n.text("系统管理版本") : TemplateHTTPClientLayout.versionTitle(TemplateHTTPClientLayout.version(value))
            return [engine, version, value["detour"].stringValue].compactMap { $0 }.joined(separator: " · ")
        }
        let keys = isSingleObject ? ["strategy", "timeout", "client_subnet"] : ["type", "server", "server_port", "address", "url", "action", "outbound"]
        let summary = keys.compactMap { key in
            let item = value[key]
            return item.stringValue ?? item.integerValue.map(String.init)
        }.joined(separator: " · ")
        if isSingleObject {
            return L10n.text("共 {0} 个字段", String(value.objectValue?.count ?? 0)) + (summary.isEmpty ? "" : " · " + summary)
        }
        return summary
    }
    private func open(_ index: Int) {
        guard items.indices.contains(index) else { return }
        selected = index; editing = Item(index: index)
    }
    private func add(_ schema: JSONValue) {
        if isSingleObject {
            guard items.isEmpty else { return }
            store.set(path, store.initialValue(schema, path: path))
            if !items.isEmpty { open(0) }
            return
        }
        let index = items.count
        store.set(path, .array(items + [store.initialValue(schema, path: path + [String(index)])]))
        if items.indices.contains(index) { open(index) }
    }
    private func move(_ offset: Int) {
        guard let index = selected, items.indices.contains(index + offset) else { return }
        var next = items; next.swapAt(index, index + offset)
        store.set(path, .array(next)); selected = index + offset
    }
    private func focus() {
        let target = SubscriptionTemplateStore.path(store.focusedPath)
        if isSingleObject, target.starts(with: path), target.count > path.count, !items.isEmpty {
            open(0)
            return
        }
        guard target.starts(with: path), target.count > path.count, let index = Int(target[path.count]), items.indices.contains(index) else { return }
        open(index)
    }
}
