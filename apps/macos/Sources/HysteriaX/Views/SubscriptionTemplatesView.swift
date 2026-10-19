import SwiftUI
import UniformTypeIdentifiers

struct TemplateTextFile: FileDocument {
    static var readableContentTypes: [UTType] { [.plainText, .json, .yaml] }
    var text: String
    init(_ text: String) { self.text = text }
    init(configuration: ReadConfiguration) throws { text = String(data: configuration.file.regularFileContents ?? Data(), encoding: .utf8) ?? "" }
    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper { FileWrapper(regularFileWithContents: Data(text.utf8)) }
}

struct SubscriptionTemplatesView: View {
    @Bindable var store: ManagementStore
    @State private var model: SubscriptionTemplateStore
    init(store: ManagementStore) { self.store = store; _model = State(initialValue: store.subscriptionTemplateEditor()) }
    @State private var refactoring = false
    @State private var refactorKind = "group"
    @State private var oldReference = ""
    @State private var newReference = ""
    @State private var filter = ""
    @State private var newName = ""
    @State private var newFormat = "mihomo"
    @State private var creating = false
    @State private var copying = false
    @State private var pendingSelection: SubscriptionTemplate?
    @State private var confirmDiscard = false
    @State private var importText = ""
    @State private var importing = false
    @State private var importReview: SubscriptionTemplateStore?
    @State private var pickingFile = false
    @State private var exporting = false
    @State private var exportFile = TemplateTextFile("")
    @State private var exportName = "template.json"
    @State private var exportType = UTType.json
    @State private var archiving = false
    @State private var replacementID = ""
    @State private var inspectorVisible = true
    @State private var compactInspectorPresented = false
    @State private var renaming = false
    @State private var rename = ""
    var body: some View {
        GeometryReader { geometry in
            let wide = geometry.size.width >= 1040
            HSplitView {
                directory.frame(minWidth: 160, idealWidth: 200, maxWidth: 240)
                if let selected = model.selected {
                    VStack(alignment: .leading, spacing: 12) {
                        editorHeader(selected, wide: wide)
                        if let error = model.error { Text(error).foregroundStyle(.red).textSelection(.enabled).accessibilityIdentifier("templates.error") }
                        if let remote = model.remoteConflict {
                            DisclosureGroup(L10n.text("服务端版本与本地修改（本地内容已保留）")) {
                                HStack(alignment: .top) { Text(model.document.pretty); Text(remote.pretty) }.font(.system(.caption, design: .monospaced)).textSelection(.enabled)
                                Button(L10n.text("保留本地修改，采用最新修订号")) { model.adoptRemoteRevision() }
                            }
                        }
                        HStack {
                            Picker(L10n.text("编辑目标"), selection: Binding(get: { model.layer }, set: { layer in
                                model.run { let api = try store.requireConnectedAPI(); try await model.syncSource(api); model.sourceMode = false; model.layer = layer }
                            })) {
                                Text(L10n.text("通用配置")).tag("common")
                                ForEach(TemplatePlatform.all, id: \.self) { Text(TemplatePlatform.title($0)).tag($0) }
                            }.accessibilityIdentifier("templates.layer")
                            Button(model.sourceMode ? L10n.text("切换到可视化") : L10n.text("切换到源码")) {
                                model.run { try await model.switchMode(store.requireConnectedAPI()) }
                            }.accessibilityIdentifier("templates.sourceMode")
                        }
                        if model.sourceMode {
                            Text(L10n.text("源码仅编辑当前配置层；平台移除操作与注入规则保存在独立区域。"))
                                .font(.caption).foregroundStyle(.secondary)
                            TextEditor(text: $model.source).font(.system(.body, design: .monospaced))
                                .accessibilityIdentifier("templates.source")
                        } else {
                            TextField(L10n.text("搜索字段"), text: $model.search).textFieldStyle(.roundedBorder).accessibilityIdentifier("templates.fieldSearch")
                            ScrollView {
                                VStack(alignment: .leading, spacing: 16) {
                                    SubscriptionTemplateFields(store: model, definition: model.catalog["schema"], path: [])
                                    SubscriptionTemplateInjections(store: model)
                                    if model.layer != "common" { removals }
                                }.padding(.leading, 28).padding(.trailing, 8).padding(.vertical, 4)
                            }
                        }
                    }.padding().frame(minWidth: 0, maxWidth: .infinity, maxHeight: .infinity)
                    if wide && inspectorVisible {
                        inspector().frame(minWidth: 300, idealWidth: 320, maxWidth: 380)
                    }
                } else {
                    ContentUnavailableView(L10n.text("订阅模板"), systemImage: "doc.text", description: Text(L10n.text("选择模板，或新建独立的全局配置。")))
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            .frame(width: geometry.size.width, height: geometry.size.height)
            .sheet(isPresented: $compactInspectorPresented) {
                VStack(spacing: 0) {
                    HStack {
                        Text(L10n.text("预览与诊断")).font(.headline)
                        Spacer()
                        Button(L10n.text("完成")) { compactInspectorPresented = false }
                    }.padding()
                    Divider()
                    inspector(showsTitle: false)
                }.frame(width: 460, height: 560)
            }
        }
        .disabled(model.busy)
        .overlay(alignment: .topTrailing) { if model.busy { ProgressView().padding() } }
        .accessibilityIdentifier("templates.page")
        .task { model.run { try await model.loadList(store.requireConnectedAPI()) } }
        .onChange(of: store.serviceAddress) { _, _ in importing = false; creating = false; archiving = false; refactoring = false; model = store.subscriptionTemplateEditor(); model.run { try await model.loadList(store.requireConnectedAPI()) } }
        .sheet(isPresented: $refactoring) { refactorSheet }
        .sheet(isPresented: $creating) { createSheet }
        .sheet(isPresented: $importing, onDismiss: { model.pendingImport = nil; importReview = nil }) { importSheet }
        .sheet(isPresented: $archiving) { archiveSheet }
        .alert(L10n.text("重命名模板"), isPresented: $renaming) {
            TextField(L10n.text("模板名称"), text: $rename)
            Button(L10n.text("保存")) { model.run { try await model.update(store.requireConnectedAPI(), name: rename) } }
            Button(L10n.text("取消"), role: .cancel) {}
        }
        .confirmationDialog(L10n.text("存在未保存修改"), isPresented: $confirmDiscard) {
            Button(L10n.text("保存并切换")) { model.run { let api = try store.requireConnectedAPI(); try await model.save(api); if let pendingSelection { try await model.open(pendingSelection, api: api) } } }
            Button(L10n.text("放弃修改并切换"), role: .destructive) { openPending() }
            Button(L10n.text("取消"), role: .cancel) { pendingSelection = nil }
        }
        .fileImporter(isPresented: $pickingFile, allowedContentTypes: [.json, .yaml, .plainText]) { result in
            do { let url = try result.get(); let access = url.startAccessingSecurityScopedResource(); defer { if access { url.stopAccessingSecurityScopedResource() } }; importText = try String(contentsOf: url, encoding: .utf8) } catch { model.error = error.localizedDescription }
        }
        .fileExporter(isPresented: $exporting, document: exportFile, contentType: exportType, defaultFilename: exportName) { result in if case .failure(let error) = result { model.error = error.localizedDescription } }
    }
    private func inspector(showsTitle: Bool = true) -> some View {
        SubscriptionTemplateInspector(store: store, model: model, export: { source, name in
            exportFile = TemplateTextFile(source); exportName = name
            exportType = name.hasSuffix(".yaml") ? .yaml : .json; exporting = true
        }, showsTitle: showsTitle)
    }
    private var directory: some View {
        VStack {
            Picker(L10n.text("配置格式"), selection: $filter) { Text(L10n.text("全部")).tag(""); Text("Mihomo").tag("mihomo"); Text("sing-box").tag("singbox") }
                .labelsHidden().accessibilityLabel(L10n.text("配置格式"))
                .padding(.horizontal)
            List(selection: Binding(get: { model.selected?.id }, set: { id in
                guard let next = model.templates.first(where: { $0.id == id }), next.id != model.selected?.id else { return }
                pendingSelection = next; if model.dirty { confirmDiscard = true } else { openPending() }
            })) {
                ForEach(model.templates.filter { filter.isEmpty || $0.format == filter }) { template in
                    VStack(alignment: .leading) {
                        Text(template.name).lineLimit(1)
                        Text("\(template.format) · \(template.publishedVersion.map { "v\($0)" } ?? L10n.text("未发布"))\(template.isDefault ? " · " + L10n.text("默认") : "")").font(.caption).foregroundStyle(.secondary)
                    }.tag(template.id).accessibilityIdentifier("templates.row.\(template.id)")
                }
            }
            HStack {
                Button { copying = false; newName = ""; creating = true } label: { Image(systemName: "plus") }.help(L10n.text("新建模板"))
                Spacer()
                Button { model.run { try await model.loadList(store.requireConnectedAPI()) } } label: { Image(systemName: "arrow.clockwise") }.help(L10n.text("刷新"))
            }.padding()
            if model.selected == nil, let error = model.error { Text(error).foregroundStyle(.red).padding() }
        }
    }
    private func editorHeader(_ selected: SubscriptionTemplate, wide: Bool) -> some View {
        VStack(alignment: .leading) {
            HStack {
                Text(selected.name).font(.title2.bold()).lineLimit(1)
                Text(model.dirty ? L10n.text("未保存") : L10n.text("草稿已保存")).accessibilityIdentifier("templates.saveState").font(.caption).foregroundStyle(.secondary)
                Spacer()
                Button {
                    if wide { inspectorVisible.toggle() } else { compactInspectorPresented = true }
                } label: { Image(systemName: "sidebar.trailing") }
                .help(L10n.text("预览与诊断"))
                .accessibilityLabel(L10n.text("预览与诊断"))
                .accessibilityIdentifier("templates.inspector.toggle")
                Menu {
                    Button(L10n.text("复制模板")) { copying = true; newName = selected.name; newFormat = selected.format; creating = true }
                    Button(L10n.text("重命名组或规则集引用…")) { oldReference = ""; newReference = ""; refactoring = true }
                    Button(L10n.text("重命名模板")) { rename = selected.name; renaming = true }
                    Button(L10n.text("设为默认模板")) { model.run { try await model.update(store.requireConnectedAPI(), makeDefault: true) } }.disabled(selected.publishedVersion == nil)
                    Button(L10n.text("导入配置或模板…")) { importText = ""; importing = true }
                    Button(L10n.text("导出完整模板…")) {
                        model.run {
                            let api = try store.requireConnectedAPI(); try await model.save(api)
                            let package = try await api.get(APIEndpoints.exportSubscriptionTemplate(id: selected.id))
                            exportFile = TemplateTextFile(JSONValue.object(package).pretty); exportName = "\(selected.name).json"; exportType = .json; exporting = true
                        }
                    }
                    Button(L10n.text("归档模板…"), role: .destructive) { replacementID = ""; archiving = true }
                } label: { Image(systemName: "ellipsis.circle") }
            }
            HStack {
                Button(L10n.text("撤销")) { model.undo() }.keyboardShortcut("z", modifiers: .command).disabled(model.undoStack.isEmpty || model.sourceMode)
                Button(L10n.text("重做")) { model.redo() }.keyboardShortcut("z", modifiers: [.command, .shift]).disabled(model.redoStack.isEmpty || model.sourceMode)
                Spacer()
                Button(L10n.text("校验")) { model.run { try await model.validate(store.requireConnectedAPI()) } }
                Button(L10n.text("保存草稿")) { model.run { try await model.save(store.requireConnectedAPI()) } }.keyboardShortcut("s", modifiers: .command).accessibilityIdentifier("templates.save")
                Button(L10n.text("发布")) { model.run { try await model.publish(store.requireConnectedAPI()) } }.accessibilityIdentifier("templates.publish")
            }
        }
    }
    private var removals: some View {
        DisclosureGroup(L10n.text("平台移除字段")) {
            ForEach(model.removals, id: \.self) { pointer in
                HStack { Text(pointer).font(.system(.caption, design: .monospaced)); Button(L10n.text("恢复继承")) { model.setState("inherit", path: SubscriptionTemplateStore.path(pointer), schema: .object([:])) } }
            }
        }
    }
    private var createSheet: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(copying ? L10n.text("复制模板") : L10n.text("新建模板")).font(.title2)
            TextField(L10n.text("模板名称"), text: $newName)
            Picker(L10n.text("配置格式"), selection: $newFormat) { Text("Mihomo").tag("mihomo"); Text("sing-box").tag("singbox") }.disabled(copying)
            HStack { Button(L10n.text("取消")) { creating = false }; Spacer(); Button(L10n.text("创建")) { creating = false; model.run { let api = try store.requireConnectedAPI(); if model.dirty { try await model.save(api) }; try await model.create(api, name: newName, format: newFormat, copy: copying) } }.disabled(newName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty).keyboardShortcut(.defaultAction) }
        }.padding(24).frame(width: 400)
    }
    private var importSheet: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(L10n.text("导入配置或模板…")).font(.title2)
            if let analysis = model.pendingImport {
                HSplitView {
                ScrollView {
                    VStack(alignment: .leading) {
                        Text(L10n.text("导入分析：将移除手工节点，并将引用替换为授权动态节点。"))
                        Text(JSONValue.object(analysis.filter { $0.key != "document" }).pretty).font(.system(.caption, design: .monospaced)).textSelection(.enabled)
                    }
                }
                if let review = importReview {
                    ScrollView {
                        VStack(alignment: .leading, spacing: 12) {
                            Text(L10n.text("确认前可调整各组的节点选择规则；这些规则不会绑定到某个用户。"))
                            SubscriptionTemplateInjections(store: review)
                        }.padding(.leading, 28).padding(.trailing, 8).padding(.vertical, 8)
                    }.frame(minWidth: 350)
                }
                }
                Button(L10n.text("确认映射并写入本地草稿")) {
                    model.run {
                        let api = try store.requireConnectedAPI()
                        if let review = importReview {
                            try await review.validate(api)
                            model.pendingImport?["document"] = review.document
                            model.pendingImport?["diagnostics"] = .array(review.diagnostics)
                        }
                        model.acceptImport(); importing = false
                    }
                }.disabled(model.busy)
            } else {
                Button(L10n.text("选择文件…")) { pickingFile = true }
                TextEditor(text: $importText).font(.system(.body, design: .monospaced)).accessibilityIdentifier("templates.import.source")
                Button(L10n.text("分析导入")) { model.run {
                    let api = try store.requireConnectedAPI()
                    try await model.analyzeImport(api, source: importText)
                    let review = SubscriptionTemplateStore()
                    review.connectedAPI = api; review.selected = model.selected; review.catalog = model.catalog
                    review.document = model.pendingImport?["document"] ?? .object([:]); review.baseline = review.document; review.focusedPath = "/injections/0"
                    importReview = review
                } }.disabled(importText.isEmpty).accessibilityIdentifier("templates.import.analyze")
            }
            if let error = model.error { Text(error).foregroundStyle(.red) }
            Button(L10n.text("取消")) { model.pendingImport = nil; importReview = nil; importing = false }.disabled(model.busy)
        }.padding(24).frame(width: model.pendingImport == nil ? 680 : 980, height: 600).interactiveDismissDisabled(model.busy)
    }
    private var refactorSheet: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(L10n.text("重命名组或规则集引用…")).font(.title2)
            Picker(L10n.text("对象类型"), selection: $refactorKind) { Text(L10n.text("代理组")).tag("group"); Text(L10n.text("规则集")).tag("rule_set") }
            TextField(L10n.text("原名称"), text: $oldReference)
            TextField(L10n.text("新名称"), text: $newReference)
            Text(L10n.text("同步更新通用配置、所有平台覆盖与节点注入中的引用。"))
            HStack { Button(L10n.text("取消")) { refactoring = false }; Spacer(); Button(L10n.text("重命名")) { model.run { try await model.refactor(store.requireConnectedAPI(), kind: refactorKind, old: oldReference, new: newReference); refactoring = false } }.disabled(oldReference.isEmpty || newReference.isEmpty) }
            if let error = model.error { Text(error).foregroundStyle(.red) }
        }.padding(24).frame(width: 480)
    }
    private var archiveSheet: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(L10n.text("归档模板…")).font(.title2)
            if model.selected?.isDefault == true {
                Picker(L10n.text("同格式替代默认模板"), selection: $replacementID) {
                    Text(L10n.text("选择模板")).tag("")
                    ForEach(model.templates.filter { $0.id != model.selected?.id && $0.format == model.selected?.format && $0.publishedVersion != nil }) { Text($0.name).tag($0.id) }
                }
            }
            Text(L10n.text("归档后，此模板的显式订阅地址返回 404。"))
            HStack { Button(L10n.text("取消")) { archiving = false }; Spacer(); Button(L10n.text("归档"), role: .destructive) { archiving = false; model.run { try await model.update(store.requireConnectedAPI(), archive: true, replacement: model.templates.first { $0.id == replacementID }) } }.disabled(model.selected?.isDefault == true && replacementID.isEmpty) }
        }.padding(24).frame(width: 460)
    }
    private func openPending() { model.run { if let pendingSelection { try await model.open(pendingSelection, api: store.requireConnectedAPI()) } } }
}
