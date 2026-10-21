import SwiftUI

struct SubscriptionTemplateSidebar: View {
    @Bindable var store: ManagementStore
    @Bindable var model: SubscriptionTemplateStore

    var body: some View {
        List(selection: Binding<String?>(get: { model.module }, set: { next in
            guard let next else { return }
            if model.sourceMode {
                model.run { try await model.syncSource(store.requireConnectedAPI()); model.sourceMode = false; model.module = next }
            } else { model.module = next }
        })) {
            ForEach(TemplateModule.all, id: \.self) {
                Text(L10n.text($0)).tag($0).accessibilityIdentifier("templates.module.\($0)")
            }
        }
        .listStyle(.sidebar)
        .accessibilityIdentifier("templates.modules")
    }
}

struct SubscriptionTemplateModules: View {
    @Bindable var store: ManagementStore
    @Bindable var model: SubscriptionTemplateStore
    @State private var rollbackVersion: Int?
    @State private var confirmRollback = false
    private var properties: [String: JSONValue] { model.schema.resolve(model.catalog["schema"], value: model.effective)["properties"].objectValue ?? [:] }
    private struct Field: Identifiable {
        let path: [String]
        let definition: JSONValue
        var id: String { SubscriptionTemplateStore.pointer(path) }
    }
    private func resolved(_ field: Field) -> JSONValue {
        model.schema.resolve(model.schema.editorDefinition(field.definition, path: field.path), value: model.effective.at(field.path) ?? .null)
    }
    private func isComplex(_ field: Field) -> Bool {
        let schema = resolved(field)
        return ["object", "array"].contains(schema["type"].stringValue ?? (model.effective.at(field.path) ?? .null).schemaType)
    }
    private func showsRootDetails(_ root: String) -> Bool {
        if root == "log" { return model.effective.at([root, "disabled"])?.boolValue != true }
        if root == "ntp" {
            let children = model.schema.resolve(properties[root] ?? .null, value: model.effective[root])["properties"].objectValue ?? [:]
            let key = children["enabled"] != nil ? "enabled" : "enable"
            let value = model.effective.at([root, key]) ?? model.initialValue(children[key] ?? .null, path: [root, key])
            return value.boolValue == true
        }
        return true
    }
    private var moduleFields: [Field] {
        properties.keys.sorted().filter { TemplateModule.destination($0) == model.module }.flatMap { key -> [Field] in
            let field = Field(path: [key], definition: properties[key]!)
            let schema = resolved(field)
            // Fixed objects expose their fields directly without a parent state row.
            guard TemplateFieldLayout.expandedObjects.contains(key), schema["type"].stringValue == "object",
                  schema["x-unverified"].boolValue != true,
                  let children = schema["properties"].objectValue, !children.isEmpty else { return [field] }
            guard model.allowed([key], value: model.effective.at([key]) ?? .null) else { return [field] }
            let documentedVisible = Set(TemplateEditorGuidance.visibleKeys(schema, path: [key], schema: model.schema, value: model.effective))
            let documentedKeys = TemplateEditorGuidance.ordered(Array(children.keys), path: [key], schema: model.schema, value: model.effective)
            let visibleKeys = documentedKeys.sorted {
                if key == "dns", ($0 == "disable_cache") != ($1 == "disable_cache") {
                    return $0 == "disable_cache"
                }
                if key == "ntp" {
                    let leftSwitch = ["enabled", "enable"].contains($0)
                    let rightSwitch = ["enabled", "enable"].contains($1)
                    if leftSwitch != rightSwitch { return leftSwitch }
                }
                return documentedKeys.firstIndex(of: $0)! < documentedKeys.firstIndex(of: $1)!
            }.filter {
                documentedVisible.contains($0) && (showsRootDetails(key) || (key == "log" && $0 == "disabled") || (key == "ntp" && ["enabled", "enable"].contains($0)))
            }
            return visibleKeys.map { Field(path: [key, $0], definition: children[$0]!) }
        }
    }
    @ViewBuilder private func fieldEditor(_ field: Field, showsHeader: Bool) -> some View {
        if field.path == ["proxies"] { Text(L10n.text("动态节点连接参数只读")) }
        else {
            SubscriptionTemplateFields(
                store: model, definition: field.definition, path: field.path,
                showsFieldHeader: showsHeader,
                expandsObjectInline: field.path == ["experimental", "cache_file"] || field.path == ["experimental", "clash_api"]
            )
        }
    }
    private struct FieldSection: Identifiable {
        let title: String
        let fields: [Field]
        var dialerPath: [String]? = nil
        var id: String { dialerPath.map { "dialer:" + SubscriptionTemplateStore.pointer($0) } ?? title }
    }
    private var fieldSections: [FieldSection] {
        let visibleFields = moduleFields.filter { field in
            if field.path == ["route", "default_interface"], model.effective["route"]["auto_detect_interface"].boolValue == true { return false }
            guard field.path.first == "dns" else { return true }
            let key = field.path.last ?? ""
            if model.effective.at(["dns", "disable_cache"])?.boolValue == true,
               key != "disable_cache",
               TemplateFieldLayout.section(path: field.path, complex: isComplex(field)) == "缓存设置" { return false }
            if key == "optimistic", model.effective.at(["dns", "disable_expire"])?.boolValue == true { return false }
            return true
        }
        let rootFields = Dictionary(grouping: visibleFields.filter { $0.path.count == 2 }) { $0.path[0] }
        let dialerKeys = rootFields.mapValues { TemplateFieldLayout.dialerFields(in: $0.compactMap { $0.path.last }) }
        let groups = Dictionary(grouping: visibleFields) { field in
            if field.path.count == 2, let root = field.path.first, dialerKeys[root]?.contains(field.path[1]) == true { return "dialer:" + root }
            return TemplateFieldLayout.section(path: field.path, complex: isComplex(field))
        }
        func orderingTitle(_ group: String) -> String {
            if group.hasPrefix("dialer:") { return TemplateFieldLayout.section(path: [String(group.dropFirst(7)), "detour"], complex: false) }
            return group
        }
        return groups.keys.sorted { TemplateFieldLayout.precedes(orderingTitle($0), orderingTitle($1), module: model.module) }.map { group in
            if group.hasPrefix("dialer:") {
                let root = String(group.dropFirst(7))
                let keys = dialerKeys[root] ?? []
                let fields = (groups[group] ?? []).sorted { keys.firstIndex(of: $0.path.last ?? "") ?? keys.count < keys.firstIndex(of: $1.path.last ?? "") ?? keys.count }
                return FieldSection(title: TemplateFieldLayout.section(path: [root], complex: true), fields: fields, dialerPath: [root])
            }
            return FieldSection(title: group, fields: groups[group] ?? [])
        }
    }
    private var unknownNestedFields: [String] {
        let roots = Set(moduleFields.filter { $0.path.count > 1 }.compactMap { $0.path.first })
        return roots.sorted().flatMap { root in
            guard showsRootDetails(root) else { return [String]() }
            let known = model.schema.resolve(properties[root]!, value: model.effective[root])["properties"].objectValue ?? [:]
            return (model.effective[root].objectValue ?? [:]).keys.filter { known[$0] == nil }.sorted().map {
                SubscriptionTemplateStore.pointer([root, $0])
            }
        }
    }
    @ViewBuilder private var configurationSections: some View {
        ForEach(fieldSections) { section in
            if let path = section.dialerPath {
                Section {
                    TemplateDialerSettings(store: model, path: path, title: L10n.text(section.title)) {
                        ForEach(section.fields) { field in fieldEditor(field, showsHeader: true) }
                    }
                }.id("section:" + section.id)
            } else {
            Section {
                ForEach(section.fields) { field in
                    fieldEditor(field, showsHeader: section.fields.count > 1 || !isComplex(field))
                }
            } header: {
                if section.fields.count == 1, let field = section.fields.first, isComplex(field) {
                    TemplateFieldLabel(store: model, path: field.path, title: L10n.text(section.title), value: model.effective.at(field.path))
                } else {
                    Text(L10n.text(section.title))
                }
            }.id("section:" + section.id)
            }
        }
        if !unknownNestedFields.isEmpty {
            Section(L10n.text("未验证字段")) {
                ForEach(unknownNestedFields, id: \.self) { Text($0).textSelection(.enabled).foregroundStyle(.secondary) }
                Text(L10n.text("未验证字段：保留原值，可通过源码编辑。"))
            }
        }
    }
    var body: some View {
        VStack(spacing: 0) {
            if model.sourceMode {
                VStack(alignment: .leading, spacing: 12) {
                    Text(L10n.text("源码编辑整份配置；节点注入规则在独立导航中管理。"))
                        .font(.caption).foregroundStyle(.secondary)
                    TextEditor(text: $model.source).font(.system(.body, design: .monospaced)).accessibilityIdentifier("templates.source")
                }.padding(12)
            } else {
                ScrollViewReader { reader in
                    Form {
                        if model.module == "节点注入" {
                            Section(L10n.text("节点注入")) { SubscriptionTemplateInjections(store: model) }
                        } else if model.module == "版本历史" {
                            Section(L10n.text("发布历史")) {
                                ForEach(model.history, id: \.pretty) { entry in
                                    HStack {
                                        Text("v\(entry["version"].integerValue ?? 0)")
                                        Text(entry["created_at"].stringValue ?? "").foregroundStyle(.secondary)
                                        Spacer()
                                        Button(L10n.text("对比")) { model.run { try await model.compare(store.requireConnectedAPI(), version: entry["version"].integerValue ?? 0) } }
                                        Button(L10n.text("回滚")) { rollbackVersion = entry["version"].integerValue; confirmRollback = true }.accessibilityIdentifier("templates.rollback.\(entry["version"].integerValue ?? 0)")
                                    }
                                }
                            }
                            if let historical = model.comparison {
                                Section(L10n.text("结构化差异")) { Text(SubscriptionTemplateStore.difference(historical, model.document).pretty).font(.system(.caption, design: .monospaced)).textSelection(.enabled) }
                            }
                        } else {
                            configurationSections
                            if model.module == "高级配置" {
                                Section(L10n.text("未知字段")) {
                                    ForEach((model.effective.objectValue ?? [:]).keys.filter { properties[$0] == nil }.sorted(), id: \.self) { Text("/" + $0).foregroundStyle(.orange).id("/" + $0) }
                                    Text(L10n.text("未知字段保留原值，请通过源码编辑。"))
                                }
                            }
                        }
                    }.formStyle(.grouped).frame(minWidth: 260)
                    .onChange(of: model.focusGeneration) { _, _ in
                        let path = model.focusedPath
                        let root = SubscriptionTemplateStore.path(path).first ?? ""
                        if root == "injections" { model.module = "节点注入" }
                        else { model.module = TemplateModule.destination(root) }
                        DispatchQueue.main.async { reader.scrollTo(path, anchor: .center) }
                    }
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("templates.editor")
        .confirmationDialog("回滚将发布新版本并替换当前本地修改。", isPresented: $confirmRollback) {
            Button(L10n.text("回滚并发布"), role: .destructive) { model.run { if let rollbackVersion { try await model.rollback(store.requireConnectedAPI(), version: rollbackVersion) } } }
            Button(L10n.text("取消"), role: .cancel) {}
        }
    }
}

struct SubscriptionTemplateDiagnostics: View {
    @Bindable var store: ManagementStore
    @Bindable var model: SubscriptionTemplateStore
    @State private var expanded = false
    var body: some View {
        DisclosureGroup(isExpanded: $expanded) {
            ScrollView {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(["error", "warning"], id: \.self) { severity in
                        ForEach(Array(model.diagnostics.enumerated()).filter { $0.element["severity"].stringValue == severity }, id: \.offset) { _, issue in
                            Button {
                                model.run { try await model.focus(issue, api: store.requireConnectedAPI()) }
                            } label: {
                                Text("\(issue["layer"].stringValue ?? "common")\(issue["path"].stringValue ?? "") · \(issue["message"].stringValue ?? "")").frame(maxWidth: .infinity, alignment: .leading)
                            }.buttonStyle(.link).foregroundStyle(severity == "error" ? .red : .orange)
                        }
                    }
                }
            }.frame(maxHeight: 140)
        } label: {
            Text(L10n.text("诊断 · {0} 错误 · {1} 警告", String(model.diagnostics.filter { $0["severity"].stringValue == "error" }.count), String(model.diagnostics.filter { $0["severity"].stringValue == "warning" }.count)))
        }.font(.caption).accessibilityIdentifier("templates.diagnostics")
    }
}
