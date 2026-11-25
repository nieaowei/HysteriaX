import SwiftUI

struct TemplateCollectionFields: View {
    @Bindable var store: SubscriptionTemplateStore
    let definition: JSONValue
    let path: [String]
    let depth: Int
    var isSingleObject = false
    @State private var editing: Item?
    @State private var filterText = ""
    @State private var rowClickTracker = RowDoubleClickTracker<Int>()
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
    private var trimmedFilter: String {
        filterText.trimmingCharacters(in: .whitespacesAndNewlines)
    }
    private var visibleEntries: [(index: Int, value: JSONValue)] {
        let all = Array(items.enumerated()).map { (index: $0.offset, value: $0.element) }
        guard !isSingleObject, items.count > 8, !trimmedFilter.isEmpty else { return all }
        return all.filter { entry in
            title(entry.value, index: entry.index).localizedCaseInsensitiveContains(trimmedFilter)
                || summary(entry.value).localizedCaseInsensitiveContains(trimmedFilter)
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if !isSingleObject && items.count > 8 {
                HStack(spacing: 6) {
                    Image(systemName: "magnifyingglass")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    TextField(L10n.text("筛选条目…"), text: $filterText)
                        .textFieldStyle(.plain)
                        .font(.callout)
                    if !filterText.isEmpty {
                        Button {
                            filterText = ""
                        } label: {
                            Image(systemName: "xmark.circle.fill")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.horizontal, 8)
                .padding(.vertical, 5)
                .background(.quaternary.opacity(0.35), in: RoundedRectangle(cornerRadius: 6))
            }
            if items.isEmpty {
                Text(L10n.text("暂无项目，点击添加开始配置。"))
                    .foregroundStyle(.secondary).frame(maxWidth: .infinity, minHeight: 60)
            } else if visibleEntries.isEmpty {
                Text(L10n.text("没有匹配的条目"))
                    .foregroundStyle(.secondary).frame(maxWidth: .infinity, minHeight: 48)
            } else {
                let entries = visibleEntries
                VStack(spacing: 0) {
                    // SwiftUI may refresh a removed row after the store has changed.
                    // Keep its value with the row instead of subscripting the live collection.
                    ForEach(entries, id: \.index) { entry in
                        let index = entry.index
                        let value = entry.value
                        HStack(spacing: 0) {
                            Button {
                                selected = index
                                if rowClickTracker.handleClick(on: index) {
                                    open(index)
                                }
                            } label: {
                                HStack {
                                    Text(String(index + 1)).monospacedDigit().foregroundStyle(.secondary).frame(width: 28)
                                    VStack(alignment: .leading, spacing: 3) {
                                        Text(title(value, index: index)).lineLimit(1)
                                        Text(summary(value)).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                                    }
                                    Spacer()
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.vertical, 10)
                                .padding(.leading, 10)
                                .padding(.trailing, 8)
                                .contentShape(Rectangle())
                            }.buttonStyle(.plain)
                                .accessibilityIdentifier("templates.item.select.\(SubscriptionTemplateStore.pointer(path)).\(index)")
                            Button(L10n.text("编辑"), systemImage: "pencil") { open(index) }
                                .labelStyle(.iconOnly).buttonStyle(.borderless)
                                .padding(.trailing, 10)
                                .accessibilityIdentifier("templates.item.edit.\(SubscriptionTemplateStore.pointer(path)).\(index)")
                        }
                        .contentShape(Rectangle())
                        .background(selected == index ? Color.accentColor.opacity(0.12) : Color.clear, in: RoundedRectangle(cornerRadius: 8))
                        .interactiveRowHover(cornerRadius: 8)
                        .onTapGesture {
                            selected = index
                            if rowClickTracker.handleClick(on: index) {
                                open(index)
                            }
                        }
                        .contextMenu {
                            Button(L10n.text("编辑")) { open(index) }
                            if !isSingleObject {
                                Button(L10n.text("复制项目")) {
                                    if let nextIndex = store.duplicateItem(path: path, index: index) {
                                        selected = nextIndex
                                    }
                                }
                                Button(L10n.text("上移")) {
                                    selected = index
                                    move(-1)
                                }
                                .disabled(index == 0 || !trimmedFilter.isEmpty)
                                Button(L10n.text("下移")) {
                                    selected = index
                                    move(1)
                                }
                                .disabled(index + 1 >= items.count || !trimmedFilter.isEmpty)
                            }
                            Divider()
                            Button(L10n.text("删除项目"), role: .destructive) {
                                deleteAt(index)
                            }
                        }
                        if index != entries.last?.index { Divider().padding(.horizontal, 10) }
                    }
                }
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
                    Button(L10n.text("复制项目"), systemImage: "doc.on.doc") {
                        guard let index = selected, let nextIndex = store.duplicateItem(path: path, index: index) else { return }
                        selected = nextIndex
                    }
                    .labelStyle(.iconOnly)
                    .help(L10n.text("复制项目"))
                    .disabled(selected == nil)
                    .accessibilityIdentifier("templates.item.duplicate.\(SubscriptionTemplateStore.pointer(path))")

                    Button(L10n.text("上移"), systemImage: "arrow.up") { move(-1) }
                        .labelStyle(.iconOnly)
                        .disabled(selected == nil || selected == 0 || !trimmedFilter.isEmpty)
                    Button(L10n.text("下移"), systemImage: "arrow.down") { move(1) }
                        .labelStyle(.iconOnly)
                        .disabled(selected == nil || selected == items.count - 1 || !trimmedFilter.isEmpty)
                }
                Button(L10n.text("删除项目"), systemImage: "trash", role: .destructive) {
                    guard let index = selected else { return }
                    deleteAt(index)
                }.labelStyle(.iconOnly).disabled(selected == nil)
            }
        }
        .sheet(item: $editing) { item in
            if items.indices.contains(item.index) {
                TemplateStructuredFieldEditor(store: store, definition: definition, path: isSingleObject ? path : path + [String(item.index)], depth: depth + 1, title: title(items[item.index], index: item.index))
            }
        }
        .modifier(TemplateCascadeDeletionPresenter(store: store, path: path))
        .onChange(of: items.count) { _, count in
            if count <= 8 && !filterText.isEmpty { filterText = "" }
            if let item = editing, !items.indices.contains(item.index) { editing = nil }
        }
        .onAppear { focus() }
        .onChange(of: store.focusGeneration) { _, _ in focus() }
    }
    private func deleteAt(_ index: Int) {
        let count = items.count
        if isSingleObject, let key = path.last { store.deleteField(parent: Array(path.dropLast()), key: key) }
        else { store.deleteItem(path: path, index: index) }
        if items.count < count { selected = items.isEmpty ? nil : min(index, items.count - 1) }
    }
    private func title(_ value: JSONValue, index: Int) -> String {
        if isSingleObject { return value["server"].stringValue.flatMap { $0.isEmpty ? nil : $0 } ?? path.last ?? "" }
        return value["name"].stringValue ?? value["tag"].stringValue ?? value["type"].stringValue ?? (value.objectValue == nil && value.arrayValue == nil ? value.pretty : nil) ?? L10n.text("项目 {0}", String(index + 1))
    }
    private func summary(_ value: JSONValue) -> String {
        if TemplateEditorGuidance.isSingbox(store.schema), let summary = TemplateEditorGuidance.summary(value, path: path) { return summary }
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

struct TemplateCascadeDeletionPresenter: ViewModifier {
    @Bindable var store: SubscriptionTemplateStore
    let path: [String]
    func body(content: Content) -> some View {
        content.sheet(isPresented: Binding(get: { store.pendingDeletion?.presenterPath == path }, set: { if !$0 { store.pendingDeletion = nil } })) {
            if let pending = store.pendingDeletion {
                VStack(alignment: .leading, spacing: 16) {
                    Text(L10n.text("级联删除预览")).font(.title2.bold())
                    Text(pending.names.joined(separator: "、")).font(.headline).textSelection(.enabled)
                    Text(L10n.text("将删除所选对象及依赖规则，并处理下列引用。整次操作可以一次撤销。"))
                    ScrollView {
                        VStack(alignment: .leading, spacing: 10) {
                            ForEach(pending.changes, id: \.self) { Text($0).font(.system(.caption, design: .monospaced)).textSelection(.enabled) }
                            DisclosureGroup(L10n.text("查看完整变更")) {
                                TemplateDiffView(diff: SubscriptionTemplateStore.difference(pending.before, pending.after))
                                    .padding(.top, 6)
                            }
                        }.frame(maxWidth: .infinity, alignment: .leading)
                    }
                    HStack {
                        Button(L10n.text("取消")) { store.pendingDeletion = nil }.keyboardShortcut(.cancelAction)
                        Spacer()
                        Button(L10n.text("级联删除"), role: .destructive) { store.confirmCascadeDeletion() }
                            .buttonStyle(.borderedProminent)
                            .keyboardShortcut(.defaultAction)
                            .accessibilityIdentifier("templates.cascade.confirm")
                    }
                }.padding(24).frame(width: 680, height: 520).disabled(store.busy)
                .accessibilityIdentifier("templates.cascade.preview")
            }
        }
    }
}
