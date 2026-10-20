import SwiftUI

struct SubscriptionTemplateInjections: View {
    @Bindable var store: SubscriptionTemplateStore
    @State private var editingIndex: Int?
    private var selectedIndex: Int {
        get { store.injectionSelection }
        nonmutating set { store.injectionSelection = newValue }
    }
    private var rules: [JSONValue] { store.document["injections"].arrayValue ?? [] }
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(L10n.text("仅选择当前用户已授权且符合订阅资格的节点。节点连接参数为只读动态内容。"))
                .font(.caption).foregroundStyle(.secondary)
            VStack(spacing: 0) {
                ForEach(Array(rules.enumerated()), id: \.offset) { index, rule in
                    HStack {
                        Button { selectedIndex = index } label: {
                            HStack {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text("\(index + 1) · \(rule["group"].stringValue ?? "")")
                                    let summary = [rule["contains"].stringValue, rule["prefix"].stringValue].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " · ")
                                    if !summary.isEmpty { Text(summary).font(.caption).foregroundStyle(.secondary) }
                                }
                                Spacer()
                            }.contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityAddTraits(selectedIndex == index ? .isSelected : [])
                        .accessibilityIdentifier("templates.injection.row.\(index)")
                        Button(L10n.text("编辑"), systemImage: "pencil") { selectedIndex = index; editingIndex = index }.labelStyle(.iconOnly).buttonStyle(.borderless)
                    }
                    .padding(10)
                    .background(selectedIndex == index ? Color.primary.opacity(0.08) : Color.clear, in: RoundedRectangle(cornerRadius: 6))
                    if index < rules.count - 1 { Divider().padding(.horizontal, 10) }
                }
            }
            HStack {
                Button(L10n.text("添加注入规则"), systemImage: "plus") {
                    let index = rules.count
                    store.edit { $0["injections"] = .array(rules + [.object(["group": .string(""), "ids": .array([]), "contains": .string(""), "prefix": .string(""), "exclude": .array([]), "descending": .bool(false), "preferred_prefix": .string(""), "on_empty": .string("direct")])]) }
                    selectedIndex = index; editingIndex = index
                }
                Spacer()
                Button(L10n.text("上移"), systemImage: "arrow.up") { move(selectedIndex, -1) }.labelStyle(.iconOnly).disabled(rules.isEmpty || selectedIndex == 0)
                Button(L10n.text("下移"), systemImage: "arrow.down") { move(selectedIndex, 1) }.labelStyle(.iconOnly).disabled(selectedIndex + 1 >= rules.count)
                Button(L10n.text("删除注入规则"), systemImage: "trash", role: .destructive) {
                    guard rules.indices.contains(selectedIndex) else { return }
                    var next = rules; next.remove(at: selectedIndex)
                    store.edit { $0["injections"] = .array(next) }; selectedIndex = max(0, min(selectedIndex, next.count - 1))
                }.labelStyle(.iconOnly).disabled(rules.isEmpty)
            }
        }
        .sheet(isPresented: Binding(get: { editingIndex != nil }, set: { if !$0 { editingIndex = nil } })) {
            if let index = editingIndex, rules.indices.contains(index) {
                VStack(spacing: 0) {
                    HStack { Text(L10n.text("动态节点注入")).font(.title2.bold()); Spacer() }.padding(20)
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
                    }.formStyle(.grouped)
                    HStack {
                        Text(L10n.text("修改会应用到本地草稿")).font(.caption).foregroundStyle(.secondary)
                        Spacer()
                        Button(L10n.text("完成")) { editingIndex = nil }.keyboardShortcut(.cancelAction)
                    }.padding(16)
                }.frame(width: 800, height: 640).accessibilityIdentifier("templates.complexEditor")
            }
        }
        .onAppear { focus() }
        .onChange(of: store.focusGeneration) { _, _ in focus() }
        .onChange(of: rules.count) { _, _ in
            if let index = editingIndex, !rules.indices.contains(index) { editingIndex = nil }
        }
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
