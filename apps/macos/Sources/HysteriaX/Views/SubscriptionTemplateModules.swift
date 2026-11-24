import SwiftUI

struct SubscriptionTemplateSidebar: View {
    @Bindable var store: ManagementStore
    @Bindable var model: SubscriptionTemplateStore
    @State private var fieldSearch = ""

    private var trimmedSearch: String {
        fieldSearch.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var searchResults: [SubscriptionTemplateStore.TemplateSearchableField] {
        guard !trimmedSearch.isEmpty else { return [] }
        let query = trimmedSearch.lowercased()
        return model.searchableFields.filter {
            $0.keywords.contains(query) || $0.pointer.localizedCaseInsensitiveContains(trimmedSearch)
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 6) {
                Image(systemName: "magnifyingglass")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
                TextField(L10n.text("搜索配置项…"), text: $fieldSearch)
                    .textFieldStyle(.plain)
                    .font(.callout)
                    .accessibilityIdentifier("templates.fieldSearch")
                if !fieldSearch.isEmpty {
                    Button {
                        fieldSearch = ""
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    .buttonStyle(.plain)
                    .help(L10n.text("清除搜索"))
                    .accessibilityLabel(L10n.text("清除搜索"))
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .background(.quaternary.opacity(0.45), in: RoundedRectangle(cornerRadius: 7, style: .continuous))
            .padding(.horizontal, 10)
            .padding(.top, 8)
            .padding(.bottom, 6)

            if trimmedSearch.isEmpty {
                List(selection: Binding<String?>(get: { model.module }, set: { next in
                    guard let next else { return }
                    if model.sourceMode {
                        model.run {
                            try await model.syncSource(store.requireConnectedAPI())
                            model.sourceMode = false
                            model.module = next
                        }
                    } else {
                        model.module = next
                    }
                })) {
                    ForEach(TemplateModule.all, id: \.self) { module in
                        HStack(spacing: 8) {
                            Image(systemName: TemplateModule.symbol(module))
                                .font(.callout)
                                .foregroundStyle(.secondary)
                                .frame(width: 16, alignment: .center)
                                .accessibilityHidden(true)
                            Text(L10n.text(module))
                                .lineLimit(1)
                            Spacer(minLength: 4)
                            moduleBadge(for: module)
                        }
                        .tag(module)
                        .accessibilityIdentifier("templates.module.\(module)")
                    }
                }
                .listStyle(.sidebar)
            } else if searchResults.isEmpty {
                VStack(spacing: 6) {
                    Image(systemName: "magnifyingglass")
                        .font(.title3)
                        .foregroundStyle(.secondary)
                    Text(L10n.text("未找到匹配的配置项"))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .padding()
            } else {
                List(searchResults) { field in
                    Button {
                        jumpToField(field)
                    } label: {
                        VStack(alignment: .leading, spacing: 3) {
                            HStack(spacing: 6) {
                                Text(field.title)
                                    .font(.callout)
                                    .lineLimit(1)
                                if model.effective.at(field.path) != nil {
                                    Circle()
                                        .fill(Color.accentColor)
                                        .frame(width: 5, height: 5)
                                        .accessibilityHidden(true)
                                }
                                Spacer(minLength: 0)
                            }
                            HStack(spacing: 4) {
                                Text("\(L10n.text(field.module)) · \(L10n.text(field.section))")
                                    .lineLimit(1)
                                Spacer(minLength: 4)
                                Text(field.pointer)
                                    .font(.caption2.monospaced())
                                    .foregroundStyle(.tertiary)
                                    .lineLimit(1)
                            }
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .padding(.vertical, 2)
                }
                .listStyle(.sidebar)
            }

            if let selected = model.selected {
                Divider()
                VStack(alignment: .leading, spacing: 6) {
                    HStack(spacing: 6) {
                        StatusBadge(
                            selected.format == "singbox" ? "sing-box" : "Mihomo",
                            tone: .neutral,
                            size: .compact,
                            showDot: false
                        )
                        StatusBadge(
                            selected.publishedVersion.map { "v\($0)" } ?? L10n.text("未发布"),
                            tone: selected.publishedVersion != nil ? .positive : .neutral,
                            size: .compact,
                            showDot: false
                        )
                        if selected.isDefault {
                            StatusBadge(L10n.text("默认"), tone: .accent, size: .compact, showDot: false)
                        }
                    }
                    if let kernel = model.catalog["kernel_version"].stringValue, !kernel.isEmpty {
                        Text(L10n.text("内核版本：{0}", kernel))
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                            .monospacedDigit()
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("templates.modules")
    }

    @ViewBuilder
    private func moduleBadge(for module: String) -> some View {
        let diag = model.moduleDiagnostics[module]
        if let errors = diag?.errors, errors > 0 {
            SidebarCountBadge(count: errors, tone: .danger)
        } else if let warnings = diag?.warnings, warnings > 0 {
            SidebarCountBadge(count: warnings, tone: .warning)
        } else if module == "节点注入" {
            let count = (model.document["injections"].arrayValue ?? []).count
            if count > 0 {
                SidebarCountBadge(count: count, tone: .neutral)
            }
        } else if module == "版本历史", !model.history.isEmpty {
            SidebarCountBadge(count: model.history.count, tone: .neutral)
        }
    }

    private func jumpToField(_ field: SubscriptionTemplateStore.TemplateSearchableField) {
        if model.sourceMode {
            model.run {
                try await model.syncSource(store.requireConnectedAPI())
                model.sourceMode = false
                model.focusedPath = field.pointer
            }
        } else {
            model.focusedPath = field.pointer
        }
    }
}

struct SubscriptionTemplateModules: View {
    @Bindable var store: ManagementStore
    @Bindable var model: SubscriptionTemplateStore
    var toast: ToastCenter? = nil
    @State private var rollbackVersion: Int?
    @State private var confirmRollback = false
    private var properties: [String: JSONValue] { model.rootProperties }
    @ViewBuilder private func fieldEditor(_ field: SubscriptionTemplateStore.TemplateModuleField, showsHeader: Bool) -> some View {
        if field.path == ["proxies"] { Text(L10n.text("动态节点连接参数只读")) }
        else {
            SubscriptionTemplateFields(
                store: model, definition: field.definition, path: field.path,
                showsFieldHeader: showsHeader,
                expandsObjectInline: field.path == ["experimental", "cache_file"] || field.path == ["experimental", "clash_api"]
            )
        }
    }
    @ViewBuilder private var configurationSections: some View {
        let layout = model.moduleLayout(for: model.module)
        ForEach(layout.sections) { section in
            if let path = section.dialerPath {
                Section {
                    TemplateDialerSettings(store: model, path: path, title: L10n.text(section.title)) {
                        ForEach(section.fields) { field in fieldEditor(field, showsHeader: true) }
                    }
                }.id("section:" + section.id)
            } else {
            Section {
                ForEach(section.fields) { field in
                    fieldEditor(field, showsHeader: section.fields.count > 1 || !field.isComplex)
                }
            } header: {
                if section.fields.count == 1, let field = section.fields.first, field.isComplex {
                    TemplateFieldLabel(
                        store: model,
                        path: field.path,
                        title: L10n.text(section.title),
                        value: model.effective.at(field.path),
                        presentation: model.fieldPresentation(path: field.path, definition: field.definition)
                    )
                } else {
                    Text(L10n.text(section.title))
                }
            }.id("section:" + section.id)
            }
        }
        if !layout.unknownNestedFields.isEmpty {
            Section(L10n.text("未验证字段")) {
                ForEach(layout.unknownNestedFields, id: \.self) { Text($0).textSelection(.enabled).foregroundStyle(.secondary) }
                Text(L10n.text("未验证字段：保留原值，可通过源码编辑。"))
            }
        }
    }
    var body: some View {
        VStack(spacing: 0) {
            if model.sourceMode {
                TemplateSourceEditor(model: model, toast: toast)
            } else {
                ScrollViewReader { reader in
                    Form {
                        if model.module == "节点注入" {
                            Section(L10n.text("节点注入")) { SubscriptionTemplateInjections(store: model) }
                        } else if model.module == "版本历史" {
                            historySections
                        } else {
                            configurationSections
                            if model.module == "高级配置" {
                                Section(L10n.text("未知字段")) {
                                    ForEach((model.effective.objectValue ?? [:]).keys.filter { properties[$0] == nil }.sorted(), id: \.self) { Text("/" + $0).foregroundStyle(.orange).id("/" + $0) }
                                    Text(L10n.text("未知字段保留原值，请通过源码编辑。"))
                                }
                            }
                        }
                    }
                    .formStyle(.grouped)
                    .frame(minWidth: 260)
                    .disabled(model.busy)
                    .onChange(of: model.focusGeneration) { _, _ in
                        let path = model.focusedPath
                        let root = SubscriptionTemplateStore.path(path).first ?? ""
                        let targetModule = root == "injections" ? "节点注入" : TemplateModule.destination(root)
                        let switched = model.module != targetModule
                        model.module = targetModule
                        if switched {
                            DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) {
                                withAnimation(.easeInOut(duration: 0.2)) {
                                    reader.scrollTo(path, anchor: .center)
                                }
                            }
                        } else {
                            DispatchQueue.main.async {
                                withAnimation(.easeInOut(duration: 0.2)) {
                                    reader.scrollTo(path, anchor: .center)
                                }
                            }
                        }
                    }
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("templates.editor")
        .confirmationDialog(L10n.text("回滚将发布新版本并替换当前本地修改。"), isPresented: $confirmRollback) {
            Button(L10n.text("回滚并发布"), role: .destructive) {
                model.run {
                    if let rollbackVersion {
                        try await model.rollback(store.requireConnectedAPI(), version: rollbackVersion)
                        toast?.show(L10n.text("已回滚至 v{0} 并发布新版本", String(rollbackVersion)), tone: .positive)
                    }
                }
            }
            Button(L10n.text("取消"), role: .cancel) {}
        }
    }

    @ViewBuilder
    private var historySections: some View {
        Section(L10n.text("发布历史")) {
            if model.history.isEmpty {
                ContentUnavailableView(
                    L10n.text("暂无发布记录"),
                    systemImage: "clock.arrow.circlepath",
                    description: Text(L10n.text("点击工具栏中的发布按钮创建首个正式版本。"))
                )
                .frame(maxWidth: .infinity)
                .padding(.vertical, 16)
            } else {
                ForEach(Array(model.history.enumerated()), id: \.offset) { _, entry in
                    let version = entry["version"].integerValue ?? 0
                    let createdAt = entry["created_at"].stringValue ?? ""
                    let isCurrent = model.selected?.publishedVersion == version
                    let isComparing = model.comparisonVersion == version && model.comparison != nil
                    HStack(spacing: 10) {
                        Text("v\(version)")
                            .font(.body.weight(.semibold))
                            .monospacedDigit()
                        if isCurrent {
                            StatusBadge(L10n.text("当前发布"), tone: .positive, size: .compact, showDot: false)
                        }
                        Text(DateDisplayText.local(createdAt))
                            .font(.callout)
                            .foregroundStyle(.secondary)
                            .monospacedDigit()
                        Spacer()
                        Button(isComparing ? L10n.text("对比中") : L10n.text("对比")) {
                            model.run { try await model.compare(store.requireConnectedAPI(), version: version) }
                        }
                        .disabled(model.busy)
                        Button(L10n.text("回滚")) {
                            rollbackVersion = version
                            confirmRollback = true
                        }
                        .disabled(model.busy)
                        .accessibilityIdentifier("templates.rollback.\(version)")
                    }
                    .padding(.vertical, 2)
                    .interactiveRowHover()
                }
            }
        }
        if let historical = model.comparison {
            Section {
                TemplateDiffView(
                    diff: SubscriptionTemplateStore.difference(historical, model.document),
                    emptyMessage: L10n.text("当前草稿与 v{0} 完全一致。", String(model.comparisonVersion ?? 0)),
                    toast: toast
                )
            } header: {
                HStack {
                    Text(L10n.text("版本对比：v{0} → 当前草稿", String(model.comparisonVersion ?? 0)))
                    Spacer()
                    Button(L10n.text("关闭对比")) {
                        model.comparison = nil
                        model.comparisonVersion = nil
                    }
                    .buttonStyle(.borderless)
                    .font(.caption)
                }
            }
        }
    }
}

struct SubscriptionTemplateDiagnosticsBar: View {
    @Bindable var store: ManagementStore
    @Bindable var model: SubscriptionTemplateStore
    @Binding var isExpanded: Bool

    private var sortedIssues: [(severity: String, issue: JSONValue)] {
        let errors = model.diagnostics.filter { ($0["severity"].stringValue ?? "warning") == "error" }.map { ("error", $0) }
        let warnings = model.diagnostics.filter { ($0["severity"].stringValue ?? "warning") != "error" }.map { ("warning", $0) }
        return errors + warnings
    }

    var body: some View {
        VStack(spacing: 0) {
            Divider()
            HStack(spacing: 10) {
                Button {
                    withAnimation(.snappy(duration: 0.2)) {
                        isExpanded.toggle()
                    }
                } label: {
                    HStack(spacing: 8) {
                        Image(systemName: isExpanded ? "chevron.down" : "chevron.right")
                            .font(.caption2.weight(.semibold))
                            .foregroundStyle(.secondary)
                            .frame(width: 12)
                        if model.errorCount > 0 {
                            StatusBadge(L10n.text("{0} 错误", String(model.errorCount)), tone: .danger, size: .compact)
                        }
                        if model.warningCount > 0 {
                            StatusBadge(L10n.text("{0} 警告", String(model.warningCount)), tone: .warning, size: .compact)
                        }
                        if model.errorCount == 0 && model.warningCount == 0 {
                            StatusBadge(
                                model.hasValidated ? L10n.text("校验通过") : L10n.text("未校验"),
                                tone: model.hasValidated ? .positive : .neutral,
                                size: .compact
                            )
                        }
                        Text(L10n.text("诊断 · {0} 错误 · {1} 警告", String(model.errorCount), String(model.warningCount)))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)

                Spacer()

                if !model.hasValidated && !model.diagnostics.isEmpty {
                    Text(L10n.text("草稿已修改，请重新校验"))
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }

                Button(L10n.text("立即校验")) {
                    model.run {
                        try await model.validate(store.requireConnectedAPI())
                        if model.errorCount > 0 || model.warningCount > 0 {
                            isExpanded = true
                        }
                    }
                }
                .buttonStyle(.borderless)
                .font(.caption)
                .disabled(model.suspended || model.busy)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 8)

            if isExpanded {
                Divider()
                let issues = sortedIssues
                if issues.isEmpty {
                    HStack(spacing: 8) {
                        Image(systemName: model.hasValidated ? "checkmark.circle.fill" : "info.circle")
                            .foregroundStyle(model.hasValidated ? Color.green : Color.secondary)
                        Text(model.hasValidated
                             ? L10n.text("当前配置无诊断错误或警告。")
                             : L10n.text("尚未运行校验；点击「立即校验」检查配置。"))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Spacer()
                    }
                    .padding(.horizontal, 14)
                    .padding(.vertical, 12)
                } else {
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 4) {
                            ForEach(Array(issues.enumerated()), id: \.offset) { _, entry in
                                let issue = entry.issue
                                let isError = entry.severity == "error"
                                let layer = issue["layer"].stringValue ?? "common"
                                let layerTitle = layer == "common" ? L10n.text("通用") : TemplatePlatform.title(layer)
                                let pathText = issue["path"].stringValue ?? ""
                                let message = issue["message"].stringValue ?? ""
                                Button {
                                    model.run { try await model.focus(issue, api: store.requireConnectedAPI()) }
                                } label: {
                                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                                        Image(systemName: isError ? "xmark.circle.fill" : "exclamationmark.triangle.fill")
                                            .font(.caption)
                                            .foregroundStyle(isError ? Color.red : Color.orange)
                                        StatusBadge(layerTitle, tone: .neutral, size: .compact, showDot: false)
                                        Text(pathText.isEmpty ? "/" : pathText)
                                            .font(.system(.caption, design: .monospaced))
                                            .foregroundStyle(.secondary)
                                        Text(message)
                                            .font(.caption)
                                            .foregroundStyle(.primary)
                                            .multilineTextAlignment(.leading)
                                            .frame(maxWidth: .infinity, alignment: .leading)
                                    }
                                    .padding(.horizontal, 8)
                                    .padding(.vertical, 5)
                                    .contentShape(Rectangle())
                                }
                                .buttonStyle(.plain)
                                .interactiveRowHover(cornerRadius: 6)
                            }
                        }
                        .padding(.horizontal, 10)
                        .padding(.vertical, 8)
                    }
                    .frame(minHeight: 110, maxHeight: 200)
                }
            }
        }
        .background(.bar)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("templates.diagnostics")
    }
}
