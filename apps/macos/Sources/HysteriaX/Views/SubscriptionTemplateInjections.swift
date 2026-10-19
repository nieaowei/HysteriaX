import SwiftUI

struct SubscriptionTemplateInjections: View {
    @Bindable var store: SubscriptionTemplateStore
    @State private var expanded = false
    private var rules: [JSONValue] { store.document["injections"].arrayValue ?? [] }
    var body: some View {
        DisclosureGroup(L10n.text("动态节点注入"), isExpanded: Binding(get: { expanded || store.focusedPath.hasPrefix("/injections/") }, set: { expanded = $0 })) {
            Text(L10n.text("仅选择当前用户已授权且符合订阅资格的节点。节点连接参数为只读动态内容。"))
                .font(.caption).foregroundStyle(.secondary)
            ForEach(rules.indices, id: \.self) { index in
                GroupBox {
                    VStack(alignment: .leading) {
                        Picker(L10n.text("目标代理组"), selection: Binding(get: { rules[index]["group"].stringValue ?? "" }, set: { update(index, "group", .string($0)) })) {
                            Text(L10n.text("选择代理组")).tag("")
                            ForEach(targetGroups, id: \.self) { Text($0).tag($0) }
                            if let current = rules[index]["group"].stringValue, !current.isEmpty, !targetGroups.contains(current) { Text(current).tag(current) }
                        }
                        field(L10n.text("节点 ID（每行一个）"), "ids", index, array: true)
                        field(L10n.text("名称包含"), "contains", index)
                        field(L10n.text("名称前缀"), "prefix", index)
                        field(L10n.text("排除名称（每行一个）"), "exclude", index, array: true)
                        field(L10n.text("优选名称前缀"), "preferred_prefix", index)
                        Toggle(L10n.text("名称降序"), isOn: Binding(get: { rules[index]["descending"].boolValue ?? false }, set: { update(index, "descending", .bool($0)) }))
                        Picker(L10n.text("空节点集合"), selection: Binding(get: { rules[index]["on_empty"].stringValue ?? "direct" }, set: { update(index, "on_empty", .string($0)) })) {
                            Text(L10n.text("直连兜底")).tag("direct")
                            Text(L10n.text("返回错误")).tag("error")
                        }
                        Button(L10n.text("删除注入规则"), role: .destructive) { var next = rules; next.remove(at: index); store.edit { $0["injections"] = .array(next) } }
                    }
                }
            }
            Button(L10n.text("添加注入规则")) {
                store.edit { $0["injections"] = .array(rules + [.object(["group": .string(""), "ids": .array([]), "contains": .string(""), "prefix": .string(""), "exclude": .array([]), "descending": .bool(false), "preferred_prefix": .string(""), "on_empty": .string("direct")])]) }
            }
        }
    }
    private var targetGroups: [String] {
        if store.selected?.format == "mihomo" { return store.effective["proxy-groups"].arrayValue?.compactMap { $0["name"].stringValue } ?? [] }
        return (store.effective["outbounds"].arrayValue ?? []).filter { ["selector", "urltest"].contains($0["type"].stringValue ?? "") }.compactMap { $0["tag"].stringValue }
    }
    private func field(_ title: String, _ key: String, _ index: Int, array: Bool = false) -> some View {
        TextField(title, text: Binding(get: {
            array ? (rules[index][key].arrayValue?.compactMap(\.stringValue).joined(separator: "\n") ?? "") : (rules[index][key].stringValue ?? "")
        }, set: { update(index, key, array ? .array($0.components(separatedBy: .newlines).filter { !$0.isEmpty }.map(JSONValue.string)) : .string($0)) }), axis: .vertical)
        .textFieldStyle(.roundedBorder)
    }
    private func update(_ index: Int, _ key: String, _ value: JSONValue) {
        store.edit { $0.set(["injections", String(index), key], value) }
    }
}
