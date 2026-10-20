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
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            List(selection: Binding<Int?>(get: { selectedIndex }, set: { if let value = $0 { selectedIndex = value } })) {
                ForEach(rules.indices, id: \.self) { index in
                    HStack {
                        Text("\(index + 1) · \(rules[index].stringValue ?? "")").lineLimit(2)
                        Spacer()
                        Button(L10n.text("编辑"), systemImage: "pencil") { editingIndex = index; selectedIndex = index }
                            .labelStyle(.iconOnly).buttonStyle(.borderless)
                            .accessibilityIdentifier("templates.rule.edit.\(SubscriptionTemplateStore.pointer(path)).\(index)")
                    }.tag(index)
                }
            }.frame(height: min(280, max(70, CGFloat(rules.count) * 40)))
            HStack {
                Button(L10n.text("添加规则"), systemImage: "plus") {
                    let index = rules.count
                    store.set(path, .array(rules + [.string("MATCH,DIRECT")]))
                    selectedIndex = index; editingIndex = index
                }
                Spacer()
                Button(L10n.text("上移"), systemImage: "arrow.up") { move(selectedIndex, -1) }.labelStyle(.iconOnly).disabled(selectedIndex == 0 || rules.isEmpty)
                Button(L10n.text("下移"), systemImage: "arrow.down") { move(selectedIndex, 1) }.labelStyle(.iconOnly).disabled(selectedIndex + 1 >= rules.count)
                Button(L10n.text("删除规则"), systemImage: "trash", role: .destructive) {
                    store.deleteItem(path: path, index: selectedIndex); selectedIndex = min(selectedIndex, max(0, rules.count - 1))
                }.labelStyle(.iconOnly).disabled(rules.isEmpty)
            }
        }
        .sheet(isPresented: Binding(get: { editingIndex != nil }, set: { if !$0 { editingIndex = nil } })) {
            if let index = editingIndex, rules.indices.contains(index) {
                VStack(spacing: 0) {
                    HStack { Text(L10n.text("编辑规则")).font(.title2.bold()); Spacer() }.padding(20)
                    Form { Section(L10n.text("规则")) { ruleFields(index) } }.formStyle(.grouped)
                    HStack {
                        Text(L10n.text("修改会应用到本地草稿")).font(.caption).foregroundStyle(.secondary)
                        Spacer()
                        Button(L10n.text("完成")) { editingIndex = nil }.keyboardShortcut(.cancelAction)
                    }.padding(16)
                }.frame(width: 800, height: 560).accessibilityIdentifier("templates.complexEditor")
            }
        }
        .onAppear { focus() }
        .onChange(of: store.focusGeneration) { _, _ in focus() }
        .onChange(of: rules.count) { _, _ in
            if let index = editingIndex, !rules.indices.contains(index) { editingIndex = nil }
        }
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
