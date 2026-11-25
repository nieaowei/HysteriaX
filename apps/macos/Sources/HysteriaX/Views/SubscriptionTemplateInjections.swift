import SwiftUI

struct SubscriptionTemplateInjections: View {
    @Bindable var store: SubscriptionTemplateStore
    @State private var editingIndex: Int?
    @State private var rowClickTracker = RowDoubleClickTracker<Int>()
    private var selectedIndex: Int {
        get { store.injectionSelection }
        nonmutating set { store.injectionSelection = newValue }
    }
    private var rules: [JSONValue] { store.document["injections"].arrayValue ?? [] }
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(L10n.text("仅选择当前用户已授权且符合订阅资格的节点。节点连接参数为只读动态内容。"))
                .font(.caption).foregroundStyle(.secondary)
            if rules.isEmpty {
                Text(L10n.text("暂无节点注入规则，点击下方按钮添加。"))
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, minHeight: 60)
            } else {
                VStack(spacing: 0) {
                    ForEach(Array(rules.enumerated()), id: \.offset) { index, rule in
                        let group = rule["group"].stringValue ?? ""
                        let chips = conditionChips(rule)
                        HStack(spacing: 0) {
                            Button {
                                selectedIndex = index
                                if rowClickTracker.handleClick(on: index) {
                                    editingIndex = index
                                }
                            } label: {
                                HStack(alignment: .center, spacing: 8) {
                                    Text(String(index + 1))
                                        .monospacedDigit()
                                        .foregroundStyle(.secondary)
                                        .frame(width: 24, alignment: .trailing)
                                    VStack(alignment: .leading, spacing: 4) {
                                        if group.isEmpty {
                                            Text(L10n.text("未选择目标代理组"))
                                                .foregroundStyle(.orange)
                                        } else {
                                            Text(group)
                                                .fontWeight(.medium)
                                        }
                                        if !chips.isEmpty {
                                            HStack(spacing: 6) {
                                                ForEach(chips, id: \.self) { chip in
                                                    Text(chip)
                                                        .font(.caption2)
                                                        .foregroundStyle(.secondary)
                                                        .padding(.horizontal, 6)
                                                        .padding(.vertical, 2)
                                                        .background(.quaternary, in: Capsule())
                                                }
                                            }
                                        }
                                    }
                                    Spacer()
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.vertical, 10)
                                .padding(.leading, 10)
                                .padding(.trailing, 8)
                                .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .accessibilityAddTraits(selectedIndex == index ? .isSelected : [])
                            .accessibilityIdentifier("templates.injection.row.\(index)")
                            Button(L10n.text("编辑"), systemImage: "pencil") { selectedIndex = index; editingIndex = index }
                                .labelStyle(.iconOnly)
                                .buttonStyle(.borderless)
                                .padding(.trailing, 10)
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
                            Button(L10n.text("复制注入规则")) {
                                if let next = store.duplicateInjection(at: index) {
                                    selectedIndex = next
                                }
                            }
                            Button(L10n.text("上移")) { move(index, -1) }.disabled(index == 0)
                            Button(L10n.text("下移")) { move(index, 1) }.disabled(index + 1 >= rules.count)
                            Divider()
                            Button(L10n.text("删除注入规则"), role: .destructive) { deleteRule(at: index) }
                        }
                        if index < rules.count - 1 { Divider().padding(.horizontal, 10) }
                    }
                }
            }
            HStack {
                Button(L10n.text("添加注入规则"), systemImage: "plus") {
                    let index = rules.count
                    store.edit { $0["injections"] = .array(rules + [.object(["group": .string(""), "ids": .array([]), "contains": .string(""), "prefix": .string(""), "exclude": .array([]), "descending": .bool(false), "preferred_prefix": .string(""), "on_empty": .string("direct")])]) }
                    selectedIndex = index; editingIndex = index
                }
                Spacer()
                Text(L10n.text("共 {0} 项", String(rules.count))).font(.caption).foregroundStyle(.secondary)
                Button(L10n.text("复制注入规则"), systemImage: "doc.on.doc") {
                    if let next = store.duplicateInjection(at: selectedIndex) {
                        selectedIndex = next
                    }
                }
                .labelStyle(.iconOnly)
                .help(L10n.text("复制注入规则"))
                .disabled(!rules.indices.contains(selectedIndex))
                Button(L10n.text("上移"), systemImage: "arrow.up") { move(selectedIndex, -1) }.labelStyle(.iconOnly).disabled(rules.isEmpty || selectedIndex == 0)
                Button(L10n.text("下移"), systemImage: "arrow.down") { move(selectedIndex, 1) }.labelStyle(.iconOnly).disabled(selectedIndex + 1 >= rules.count)
                Button(L10n.text("删除注入规则"), systemImage: "trash", role: .destructive) {
                    deleteRule(at: selectedIndex)
                }.labelStyle(.iconOnly).disabled(!rules.indices.contains(selectedIndex))
            }
        }
        .sheet(isPresented: Binding(get: { editingIndex != nil }, set: { if !$0 { editingIndex = nil } })) {
            if let index = editingIndex, rules.indices.contains(index) {
                VStack(spacing: 0) {
                    DetailHeaderBar {
                        HStack {
                            Text(L10n.text("动态节点注入")).font(.title2.bold())
                            Spacer()
                        }
                    }
                    Form {
                        Section(L10n.text("基础信息")) {
                            Picker(L10n.text("目标代理组"), selection: Binding(get: { entry(index, "group").stringValue ?? "" }, set: { update(index, "group", .string($0)) })) {
                                Text(L10n.text("选择代理组")).tag("")
                                ForEach(targetGroups, id: \.self) { Text(TemplateSchema.pickerTitle(.string($0))).tag($0) }
                                if let current = entry(index, "group").stringValue, !current.isEmpty, !targetGroups.contains(current) { Text(TemplateSchema.pickerTitle(.string(current))).tag(current) }
                            }
                        }
                        Section(L10n.text("节点筛选")) {
                            field(L10n.text("节点 ID（每行一个）"), "ids", index, array: true)
                            field(L10n.text("名称包含"), "contains", index)
                            field(L10n.text("名称前缀"), "prefix", index)
                            field(L10n.text("排除名称（每行一个）"), "exclude", index, array: true)
                        }
                        Section(L10n.text("排序与兜底")) {
                            field(L10n.text("优选名称前缀"), "preferred_prefix", index)
                            Toggle(L10n.text("名称降序"), isOn: Binding(get: { entry(index, "descending").boolValue ?? false }, set: { update(index, "descending", .bool($0)) }))
                            let onEmpty = entry(index, "on_empty").stringValue ?? "direct"
                            Picker(L10n.text("空节点集合"), selection: Binding(get: { onEmpty }, set: { update(index, "on_empty", .string($0)) })) {
                                Text(L10n.text("直连兜底")).tag("direct")
                                Text(L10n.text("返回错误")).tag("error")
                                if !["direct", "error"].contains(onEmpty) { Text(TemplateSchema.pickerTitle(.string(onEmpty))).tag(onEmpty) }
                            }
                        }
                    }.formStyle(.grouped).scrollContentBackground(.hidden)
                    Divider()
                    TemplateEditorFooter(store: store) { editingIndex = nil }.id(index)
                }.frame(width: 800, height: 640).accessibilityIdentifier("templates.complexEditor")
            }
        }
        .onAppear { focus() }
        .onChange(of: store.focusGeneration) { _, _ in focus() }
        .onChange(of: rules.count) { _, _ in
            if let index = editingIndex, !rules.indices.contains(index) { editingIndex = nil }
        }
    }
    private func conditionChips(_ rule: JSONValue) -> [String] {
        var chips: [String] = []
        let idsCount = rule["ids"].arrayValue?.count ?? 0
        if idsCount > 0 { chips.append("ID ×\(idsCount)") }
        if let contains = rule["contains"].stringValue, !contains.isEmpty { chips.append(L10n.text("包含: {0}", contains)) }
        if let prefix = rule["prefix"].stringValue, !prefix.isEmpty { chips.append(L10n.text("前缀: {0}", prefix)) }
        let excludeCount = rule["exclude"].arrayValue?.count ?? 0
        if excludeCount > 0 { chips.append(L10n.text("排除 ×{0}", String(excludeCount))) }
        if let preferred = rule["preferred_prefix"].stringValue, !preferred.isEmpty { chips.append(L10n.text("优选: {0}", preferred)) }
        if rule["descending"].boolValue == true { chips.append(L10n.text("降序")) }
        if rule["on_empty"].stringValue == "error" { chips.append(L10n.text("空集合报错")) }
        return chips
    }
    private func deleteRule(at index: Int) {
        guard rules.indices.contains(index) else { return }
        var next = rules; next.remove(at: index)
        store.edit { $0["injections"] = .array(next) }
        selectedIndex = max(0, min(index, next.count - 1))
    }
    private func focus() {
        let target = SubscriptionTemplateStore.path(store.focusedPath)
        if target.first == "injections", target.count > 1, let index = Int(target[1]), rules.indices.contains(index) { selectedIndex = index; editingIndex = index }
    }
    private func move(_ index: Int, _ offset: Int) {
        var next = rules; let target = index + offset
        guard next.indices.contains(index), next.indices.contains(target) else { return }
        next.swapAt(index, target); store.edit { $0["injections"] = .array(next) }; selectedIndex = target
    }
    private var targetGroups: [String] {
        let names: [String]
        if store.selected?.format == "mihomo" { names = store.effective["proxy-groups"].arrayValue?.compactMap { $0["name"].stringValue } ?? [] }
        else { names = (store.effective["outbounds"].arrayValue ?? []).filter { ["selector", "urltest"].contains($0["type"].stringValue ?? "") }.compactMap { $0["tag"].stringValue } }
        var seen: Set<String> = []
        return names.filter { !$0.isEmpty && seen.insert($0).inserted }
    }
    @ViewBuilder private func field(_ title: String, _ key: String, _ index: Int, array: Bool = false) -> some View {
        if array {
            VStack(alignment: .leading, spacing: 6) {
                Text(title)
                TemplateLineListInput(values: Binding(get: { entry(index, key).arrayValue ?? [] }, set: { update(index, key, .array($0)) }), identifier: "templates.lines./injections/\(index)/\(key)")
            }
        } else {
            TextField(title, text: Binding(get: { entry(index, key).stringValue ?? "" }, set: { update(index, key, .string($0)) }))
                .textFieldStyle(.roundedBorder)
        }
    }
    private func entry(_ index: Int, _ key: String) -> JSONValue { rules.indices.contains(index) ? rules[index][key] : .null }
    private func update(_ index: Int, _ key: String, _ value: JSONValue) {
        guard rules.indices.contains(index) else { return }
        store.edit { $0.set(["injections", String(index), key], value) }
    }
}
