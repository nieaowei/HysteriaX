import SwiftUI

struct MihomoTemplateRules: View {
    @Bindable var store: SubscriptionTemplateStore
    let path: [String]
    private var selectedIndex: Int {
        get { store.collectionSelections[SubscriptionTemplateStore.pointer(path)].flatMap(Int.init) ?? 0 }
        nonmutating set { store.collectionSelections[SubscriptionTemplateStore.pointer(path)] = String(newValue) }
    }
    private var rules: [JSONValue] { store.effective.at(path)?.arrayValue ?? [] }
    private var kinds: [String] { store.catalog["schema"]["x-rule-types"].arrayValue?.compactMap(\.stringValue) ?? ["MATCH"] }
    @State private var editingIndex: Int?
    @State private var filterText = ""
    @State private var rowClickTracker = RowDoubleClickTracker<Int>()

    private var trimmedFilter: String {
        filterText.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var visibleEntries: [(index: Int, source: String)] {
        let all = rules.enumerated().map { (index: $0.offset, source: $0.element.stringValue ?? "") }
        guard rules.count > 8, !trimmedFilter.isEmpty else { return all }
        return all.filter { $0.source.localizedCaseInsensitiveContains(trimmedFilter) }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if rules.count > 8 {
                HStack(spacing: 6) {
                    Image(systemName: "magnifyingglass")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    TextField(L10n.text("筛选规则…"), text: $filterText)
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

            if rules.isEmpty {
                Text(L10n.text("暂无规则，点击添加开始配置。"))
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, minHeight: 60)
            } else if visibleEntries.isEmpty {
                Text(L10n.text("没有匹配的规则"))
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, minHeight: 48)
            } else {
                let entries = visibleEntries
                VStack(spacing: 0) {
                    ForEach(entries, id: \.index) { entry in
                        let index = entry.index
                        let rule = MihomoTemplateRule(entry.source)
                        HStack(spacing: 0) {
                            Button {
                                selectedIndex = index
                                if rowClickTracker.handleClick(on: index) {
                                    editingIndex = index
                                }
                            } label: {
                                HStack(spacing: 8) {
                                    Text(String(index + 1))
                                        .monospacedDigit()
                                        .foregroundStyle(.secondary)
                                        .frame(width: 28, alignment: .trailing)
                                    StatusBadge(rule.kind, tone: .neutral, size: .compact, showDot: false)
                                    if rule.kind != "MATCH" && !rule.payload.isEmpty {
                                        Text(rule.payload)
                                            .font(.system(.callout, design: .monospaced))
                                            .lineLimit(1)
                                            .truncationMode(.middle)
                                    }
                                    Image(systemName: "arrow.right")
                                        .font(.caption2)
                                        .foregroundStyle(.secondary)
                                    StatusBadge(rule.target, tone: targetTone(rule.target), size: .compact, showDot: false)
                                    ForEach(rule.options, id: \.self) { option in
                                        Text(option)
                                            .font(.caption2)
                                            .foregroundStyle(.secondary)
                                            .padding(.horizontal, 5)
                                            .padding(.vertical, 1)
                                            .background(.quaternary, in: Capsule())
                                    }
                                    Spacer(minLength: 0)
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.vertical, 10)
                                .padding(.leading, 10)
                                .padding(.trailing, 8)
                                .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .accessibilityAddTraits(selectedIndex == index ? .isSelected : [])
                            Button(L10n.text("编辑"), systemImage: "pencil") { editingIndex = index; selectedIndex = index }
                                .labelStyle(.iconOnly)
                                .buttonStyle(.borderless)
                                .padding(.trailing, 10)
                                .accessibilityIdentifier("templates.rule.edit.\(SubscriptionTemplateStore.pointer(path)).\(index)")
                        }
                        .contentShape(Rectangle())
                        .background(selectedIndex == index ? Color.accentColor.opacity(0.12) : Color.clear, in: RoundedRectangle(cornerRadius: 8))
                        .interactiveRowHover(cornerRadius: 8)
                        .onTapGesture {
                            selectedIndex = index
                            if rowClickTracker.handleClick(on: index) {
                                editingIndex = index
                            }
                        }
                        .contextMenu {
                            Button(L10n.text("编辑")) { selectedIndex = index; editingIndex = index }
                            Button(L10n.text("复制规则")) {
                                if let next = store.duplicateItem(path: path, index: index) {
                                    selectedIndex = next
                                }
                            }
                            Button(L10n.text("上移")) { move(index, -1) }
                                .disabled(index == 0 || !trimmedFilter.isEmpty)
                            Button(L10n.text("下移")) { move(index, 1) }
                                .disabled(index + 1 >= rules.count || !trimmedFilter.isEmpty)
                            Divider()
                            Button(L10n.text("删除规则"), role: .destructive) { deleteRule(at: index) }
                        }
                        if index != entries.last?.index { Divider().padding(.horizontal, 10) }
                    }
                }
            }

            HStack {
                Button(L10n.text("添加规则"), systemImage: "plus") {
                    let index = rules.count
                    store.set(path, .array(rules + [.string("MATCH,DIRECT")]))
                    selectedIndex = index; editingIndex = index
                }
                Spacer()
                Text(L10n.text("共 {0} 项", String(rules.count))).font(.caption).foregroundStyle(.secondary)
                Button(L10n.text("复制规则"), systemImage: "doc.on.doc") {
                    if let next = store.duplicateItem(path: path, index: selectedIndex) {
                        selectedIndex = next
                    }
                }
                .labelStyle(.iconOnly)
                .help(L10n.text("复制规则"))
                .disabled(!rules.indices.contains(selectedIndex))
                Button(L10n.text("上移"), systemImage: "arrow.up") { move(selectedIndex, -1) }
                    .labelStyle(.iconOnly)
                    .disabled(selectedIndex == 0 || rules.isEmpty || !trimmedFilter.isEmpty)
                Button(L10n.text("下移"), systemImage: "arrow.down") { move(selectedIndex, 1) }
                    .labelStyle(.iconOnly)
                    .disabled(selectedIndex + 1 >= rules.count || !trimmedFilter.isEmpty)
                Button(L10n.text("删除规则"), systemImage: "trash", role: .destructive) {
                    deleteRule(at: selectedIndex)
                }
                .labelStyle(.iconOnly)
                .disabled(!rules.indices.contains(selectedIndex))
            }
        }
        .sheet(isPresented: Binding(get: { editingIndex != nil }, set: { if !$0 { editingIndex = nil } })) {
            if let index = editingIndex, rules.indices.contains(index) {
                VStack(spacing: 0) {
                    DetailHeaderBar {
                        HStack {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(L10n.text("编辑规则")).font(.title2.bold())
                                Text("#\(index + 1)").font(.caption.monospaced()).foregroundStyle(.secondary)
                            }
                            Spacer()
                            Button(L10n.text("撤销"), systemImage: "arrow.uturn.backward") { store.undo() }
                                .labelStyle(.iconOnly)
                                .help(L10n.text("撤销"))
                                .disabled(store.undoStack.isEmpty)
                            Button(L10n.text("重做"), systemImage: "arrow.uturn.forward") { store.redo() }
                                .labelStyle(.iconOnly)
                                .help(L10n.text("重做"))
                                .disabled(store.redoStack.isEmpty)
                        }
                    }
                    Form { Section(L10n.text("规则")) { ruleFields(index) } }.formStyle(.grouped).scrollContentBackground(.hidden)
                    Divider()
                    TemplateEditorFooter(store: store) { editingIndex = nil }.id(index)
                }.frame(width: 800, height: 560).accessibilityIdentifier("templates.complexEditor")
            }
        }
        .onAppear { focus() }
        .onChange(of: store.focusGeneration) { _, _ in focus() }
        .onChange(of: rules.count) { _, count in
            if count <= 8 && !filterText.isEmpty { filterText = "" }
            if let index = editingIndex, !rules.indices.contains(index) { editingIndex = nil }
        }
    }

    private func targetTone(_ target: String) -> StatusTone {
        switch target {
        case "DIRECT", "PASS", "COMPATIBLE": .positive
        case "REJECT", "REJECT-DROP": .danger
        default: .active
        }
    }

    private func deleteRule(at index: Int) {
        guard rules.indices.contains(index) else { return }
        store.deleteItem(path: path, index: index)
        selectedIndex = min(index, max(0, rules.count - 1))
    }

    @ViewBuilder private func ruleFields(_ index: Int) -> some View {
        let rule = MihomoTemplateRule(rules[index].stringValue ?? "")
        Picker(L10n.text("规则类型"), selection: binding(index, \.kind)) {
            ForEach(kinds, id: \.self) { Text($0).tag($0) }
            if !kinds.contains(rule.kind) { Text(rule.kind).tag(rule.kind) }
        }.accessibilityIdentifier("templates.rule.kind.\(SubscriptionTemplateStore.pointer(path)).\(index)")
        if rule.kind != "MATCH" {
            if rule.argumentKind == "rule_set" {
                Picker(L10n.text("规则集"), selection: binding(index, \.payload)) { references(store.referenceNames("rule_set"), current: rule.payload) }
            } else {
                TextField(rule.argumentKind == "predicate" ? L10n.text("逻辑匹配表达式") : L10n.text("匹配值"), text: binding(index, \.payload), axis: .vertical).textFieldStyle(.roundedBorder)
            }
        }
        Picker(rule.kind == "SUB-RULE" ? L10n.text("命名子规则") : L10n.text("规则动作"), selection: binding(index, \.target)) {
            references(store.referenceNames(rule.kind == "SUB-RULE" ? "sub_rule" : "outbound"), current: rule.target)
        }.accessibilityIdentifier("templates.rule.target.\(SubscriptionTemplateStore.pointer(path)).\(index)")
        if rule.supportsNoResolve {
            Toggle("no-resolve", isOn: Binding(get: { rule.options.contains("no-resolve") }, set: { enabled in update(index) { rule in rule.options.removeAll { $0 == "no-resolve" }; if enabled { rule.options.append("no-resolve") } } }))
            Toggle("src", isOn: Binding(get: { rule.options.contains("src") }, set: { enabled in update(index) { rule in rule.options.removeAll { $0 == "src" }; if enabled { rule.options.append("src") } } }))
        }
        Text(rule.source).font(.system(.caption, design: .monospaced)).textSelection(.enabled).foregroundStyle(.secondary)
    }
    private func focus() {
        let target = SubscriptionTemplateStore.path(store.focusedPath)
        if target.starts(with: path), target.count > path.count, let index = Int(target[path.count]), rules.indices.contains(index) {
            selectedIndex = index; editingIndex = index
        }
    }
    @ViewBuilder private func references(_ names: [String], current: String) -> some View {
        Text(L10n.text("未设置")).tag("")
        ForEach(names, id: \.self) { Text($0).tag($0) }
        if !current.isEmpty, !names.contains(current) { Text(current).tag(current) }
    }
    private func binding(_ index: Int, _ key: WritableKeyPath<MihomoTemplateRule, String>) -> Binding<String> {
        Binding(get: { MihomoTemplateRule(rules.indices.contains(index) ? rules[index].stringValue ?? "" : "")[keyPath: key] }, set: { value in update(index) { rule in rule[keyPath: key] = value; if key == \.kind && (MihomoTemplateRule.commaPayloadKinds.contains(value) || value == "MATCH") { rule.options = [] } } })
    }
    private func update(_ index: Int, _ edit: (inout MihomoTemplateRule) -> Void) {
        var values = rules; guard values.indices.contains(index) else { return }
        var rule = MihomoTemplateRule(values[index].stringValue ?? ""); edit(&rule); values[index] = .string(rule.source); store.set(path, .array(values))
    }
    private func move(_ index: Int, _ offset: Int) { var values = rules; let next = index + offset; guard values.indices.contains(next) else { return }; values.swapAt(index, next); store.set(path, .array(values)); selectedIndex = next }
}
