import SwiftUI

struct MihomoTemplateRules: View {
    @Bindable var store: SubscriptionTemplateStore
    let path: [String]
    private var rules: [JSONValue] { store.effective.at(path)?.arrayValue ?? [] }
    private var kinds: [String] { store.catalog["schema"]["x-rule-types"].arrayValue?.compactMap(\.stringValue) ?? ["MATCH"] }
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            ForEach(rules.indices, id: \.self) { index in
                let rule = MihomoTemplateRule(rules[index].stringValue ?? "")
                GroupBox {
                    VStack(alignment: .leading, spacing: 8) {
                        HStack {
                            Text("\(index + 1)").monospacedDigit().foregroundStyle(.secondary)
                            Picker(L10n.text("规则类型"), selection: binding(index, \.kind)) {
                                ForEach(kinds, id: \.self) { Text($0).tag($0) }
                                if !kinds.contains(rule.kind) { Text(rule.kind).tag(rule.kind) }
                            }.accessibilityIdentifier("templates.rule.kind.\(SubscriptionTemplateStore.pointer(path)).\(index)")
                            Spacer()
                            Button { move(index, -1) } label: { Image(systemName: "arrow.up") }.disabled(index == 0).accessibilityLabel(L10n.text("上移"))
                            Button { move(index, 1) } label: { Image(systemName: "arrow.down") }.disabled(index + 1 == rules.count).accessibilityLabel(L10n.text("下移"))
                            Button(role: .destructive) { store.deleteItem(path: path, index: index) } label: { Image(systemName: "minus.circle") }.accessibilityLabel(L10n.text("删除规则"))
                        }
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
                }.accessibilityIdentifier("templates.rule.\(SubscriptionTemplateStore.pointer(path)).\(index)")
            }
            Button(L10n.text("添加规则")) { store.set(path, .array(rules + [.string("MATCH,DIRECT")])) }
        }
    }
    @ViewBuilder private func references(_ names: [String], current: String) -> some View {
        Text(L10n.text("未设置")).tag("")
        ForEach(names, id: \.self) { Text($0).tag($0) }
        if !current.isEmpty, !names.contains(current) { Text(current).tag(current) }
    }
    private func binding(_ index: Int, _ key: WritableKeyPath<MihomoTemplateRule, String>) -> Binding<String> {
        Binding(get: { MihomoTemplateRule(rules[index].stringValue ?? "")[keyPath: key] }, set: { value in update(index) { rule in rule[keyPath: key] = value; if key == \.kind && (MihomoTemplateRule.commaPayloadKinds.contains(value) || value == "MATCH") { rule.options = [] } } })
    }
    private func update(_ index: Int, _ edit: (inout MihomoTemplateRule) -> Void) {
        var values = rules; guard values.indices.contains(index) else { return }
        var rule = MihomoTemplateRule(values[index].stringValue ?? ""); edit(&rule); values[index] = .string(rule.source); store.set(path, .array(values))
    }
    private func move(_ index: Int, _ offset: Int) { var values = rules; let next = index + offset; guard values.indices.contains(next) else { return }; values.swapAt(index, next); store.set(path, .array(values)) }
}
