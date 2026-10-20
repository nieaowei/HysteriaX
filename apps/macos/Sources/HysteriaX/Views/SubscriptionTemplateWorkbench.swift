import SwiftUI
import UniformTypeIdentifiers

struct SubscriptionTemplateWorkbench: View {
    @Bindable var store: ManagementStore
    @Bindable var model: SubscriptionTemplateStore
    @State private var refactoring = false
    @State private var refactorKind = "group"
    @State private var oldReference = ""
    @State private var newReference = ""
    @State private var newName = ""
    @State private var newFormat = "mihomo"
    @State private var creating = false
    @State private var copying = false
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
    @State private var renaming = false
    @State private var rename = ""
    @State private var confirmingPublish = false
    @State private var conversionSource = "common"
    var body: some View {
        NavigationSplitView(columnVisibility: .constant(.all)) {
            SubscriptionTemplateSidebar(store: store, model: model)
                .navigationSplitViewColumnWidth(min: 140, ideal: 160, max: 190)
        } content: {
            editorContent
                .navigationSplitViewColumnWidth(min: 360, ideal: 640)
                .toolbar { editorToolbar }
        } detail: {
            inspector()
                .navigationSplitViewColumnWidth(min: 280, ideal: 320, max: 440)
        }
        .navigationSplitViewStyle(.balanced)
        .navigationTitle(model.selected?.name ?? L10n.text("订阅模板"))
        .navigationSubtitle(model.dirty ? L10n.text("未保存") : L10n.text("草稿已保存"))
        .toolbar(removing: .sidebarToggle)
        .sheet(isPresented: $confirmingPublish) {
            VStack(alignment: .leading, spacing: 16) {
                Text(L10n.text("发布检查")).font(.title2)
                ScrollView { Text(model.publishDiff.pretty).font(.system(.caption, design: .monospaced)).textSelection(.enabled) }
                HStack { Button(L10n.text("取消")) { confirmingPublish = false }; Spacer(); Button(L10n.text("保存并发布")) { confirmingPublish = false; model.run { try await model.publish(store.requireConnectedAPI()) } } }
            }.padding(24).frame(width: 640, height: 520)
        }
        .disabled(model.busy)
        .overlay(alignment: .topTrailing) { if model.busy { ProgressView().padding() } }
        .accessibilityIdentifier("templates.workbench")
        .onChange(of: model.requestedAction) { _, action in handleAction(action) }
        .sheet(isPresented: $refactoring) { refactorSheet }
        .sheet(isPresented: $creating) { createSheet }
        .sheet(isPresented: $importing, onDismiss: { model.pendingImport = nil; importReview = nil }) { importSheet }
        .sheet(isPresented: $archiving) { archiveSheet }
        .alert(L10n.text("重命名模板"), isPresented: $renaming) {
            TextField(L10n.text("模板名称"), text: $rename)
            Button(L10n.text("保存")) { model.run { try await model.update(store.requireConnectedAPI(), name: rename) } }
            Button(L10n.text("取消"), role: .cancel) {}
        }
        .fileImporter(isPresented: $pickingFile, allowedContentTypes: [.json, .yaml, .plainText]) { result in
            do { let url = try result.get(); let access = url.startAccessingSecurityScopedResource(); defer { if access { url.stopAccessingSecurityScopedResource() } }; importText = try String(contentsOf: url, encoding: .utf8) } catch { model.error = error.localizedDescription }
        }
        .fileExporter(isPresented: $exporting, document: exportFile, contentType: exportType, defaultFilename: exportName) { result in if case .failure(let error) = result { model.error = error.localizedDescription } }
    }
    @ViewBuilder private var editorContent: some View {
        if model.selected != nil {
            VStack(alignment: .leading, spacing: 0) {
                if model.suspended || model.error != nil || model.remoteConflict != nil {
                    VStack(alignment: .leading, spacing: 12) {
                        if model.suspended { Text(L10n.text("当前服务已切换；本地内容已保留，远端操作已暂停。")).foregroundStyle(.orange) }
                        if let error = model.error { Text(error).foregroundStyle(.red).textSelection(.enabled).accessibilityIdentifier("templates.error") }
                        if let remote = model.remoteConflict {
                            DisclosureGroup(L10n.text("服务端版本与本地修改（本地内容已保留）")) {
                                HStack(alignment: .top) { Text(model.document.pretty); Text(remote.pretty) }.font(.system(.caption, design: .monospaced)).textSelection(.enabled)
                                Button(L10n.text("保留本地修改，采用最新修订号")) { model.adoptRemoteRevision() }
                            }
                        }
                    }.padding()
                }
                if model.needsPlatformConversion {
                    platformConversion
                } else {
                    SubscriptionTemplateModules(store: store, model: model)
                }
            }.frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            ContentUnavailableView(L10n.text("订阅模板"), systemImage: "doc.text", description: Text(L10n.text("选择模板，或新建独立的全局配置。")))
        }
    }
    private var platformConversion: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(L10n.text("转换为单份配置")).font(.title2)
            Text(L10n.text("旧模板包含平台覆盖。请选择保留的配置，确认下方差异后转换；保存前可以撤销。"))
                .foregroundStyle(.secondary)
            Picker(L10n.text("保留配置"), selection: $conversionSource) {
                Text(L10n.text("通用配置")).tag("common")
                ForEach(TemplatePlatform.all.filter { model.document["platforms"][$0].objectValue != nil }, id: \.self) {
                    Text(TemplatePlatform.title($0)).tag($0)
                }
            }
            ScrollView {
                Text(SubscriptionTemplateStore.difference(model.document, model.singleConfiguration(from: conversionSource)).pretty)
                    .font(.system(.caption, design: .monospaced)).textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            Button(L10n.text("采用这份配置")) { model.convertToSingleConfiguration(from: conversionSource) }
                .buttonStyle(.borderedProminent)
        }.padding(20)
        .accessibilityIdentifier("templates.platformConversion")
    }
    @ToolbarContentBuilder private var editorToolbar: some ToolbarContent {
        ToolbarItemGroup(placement: .automatic) {
            Button {
                model.run { try await model.switchMode(store.requireConnectedAPI()) }
            } label: {
                Label(model.sourceMode ? L10n.text("切换到可视化") : L10n.text("切换到源码"), systemImage: model.sourceMode ? "slider.horizontal.3" : "chevron.left.forwardslash.chevron.right")
            }.labelStyle(.iconOnly).help(model.sourceMode ? L10n.text("切换到可视化") : L10n.text("切换到源码")).disabled(model.busy || model.needsPlatformConversion).accessibilityIdentifier("templates.sourceMode")
        }
        ToolbarItemGroup(placement: .primaryAction) {
            Button { model.undo() } label: { Image(systemName: "arrow.uturn.backward") }.help(L10n.text("撤销")).disabled(model.undoStack.isEmpty || model.sourceMode || model.busy).accessibilityIdentifier("templates.undo")
            Button { model.redo() } label: { Image(systemName: "arrow.uturn.forward") }.help(L10n.text("重做")).disabled(model.redoStack.isEmpty || model.sourceMode || model.busy).accessibilityIdentifier("templates.redo")
            Button(L10n.text("校验"), systemImage: "checkmark.shield") { model.run { try await model.validate(store.requireConnectedAPI()) } }.labelStyle(.iconOnly).help(L10n.text("校验")).disabled(model.suspended || model.busy || model.needsPlatformConversion)
            Button(L10n.text("保存草稿"), systemImage: "square.and.arrow.down") { model.run { try await model.save(store.requireConnectedAPI()) } }.labelStyle(.iconOnly).help(L10n.text("保存草稿")).keyboardShortcut("s", modifiers: .command).disabled(model.suspended || model.busy || model.needsPlatformConversion).accessibilityIdentifier("templates.save")
            Button(L10n.text("发布"), systemImage: "paperplane") { model.run { try await model.preparePublish(store.requireConnectedAPI()); confirmingPublish = true } }.labelStyle(.iconOnly).help(L10n.text("发布")).disabled(model.suspended || model.busy || model.needsPlatformConversion).accessibilityIdentifier("templates.publish")
            if let selected = model.selected { editorMenu(selected).disabled(model.busy) }
        }
    }
    private func inspector() -> some View {
        SubscriptionTemplateInspector(store: store, model: model, export: { source, name in
            exportFile = TemplateTextFile(source); exportName = name
            exportType = name.hasSuffix(".yaml") ? .yaml : .json; exporting = true
        })
    }
    private func editorMenu(_ selected: SubscriptionTemplate) -> some View {
        Menu {
            Button(L10n.text("复制模板")) { copying = true; newName = selected.name; newFormat = selected.format; creating = true }
            Button(L10n.text("重命名组或规则集引用…")) { oldReference = ""; newReference = ""; refactoring = true }
            Button(L10n.text("重命名模板")) { rename = selected.name; renaming = true }
            Button(L10n.text("设为默认模板")) { model.run { try await model.update(store.requireConnectedAPI(), makeDefault: true) } }.disabled(selected.publishedVersion == nil)
            Button(L10n.text("导入配置或模板…")) { model.run { try await model.syncSource(store.requireConnectedAPI()); importText = ""; importing = true } }
            Button(L10n.text("导出完整模板…")) {
                model.run {
                    let api = try store.requireConnectedAPI(); try await model.save(api)
                    let package = try await api.get(APIEndpoints.exportSubscriptionTemplate(id: selected.id))
                    try model.requireSession(api)
                    exportFile = TemplateTextFile(JSONValue.object(package).pretty); exportName = "\(selected.name).json"; exportType = .json; exporting = true
                }
            }
            Button(L10n.text("归档模板…"), role: .destructive) { replacementID = ""; archiving = true }
        } label: { Image(systemName: "ellipsis.circle") }
    }
    private var createSheet: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(copying ? L10n.text("复制模板") : L10n.text("新建模板")).font(.title2)
            TextField(L10n.text("模板名称"), text: $newName)
            Picker(L10n.text("配置格式"), selection: $newFormat) { Text("Mihomo").tag("mihomo"); Text("sing-box").tag("singbox") }.disabled(copying)
            HStack { Button(L10n.text("取消")) { creating = false }; Spacer(); Button(L10n.text("创建")) { creating = false; model.run { let api = try store.requireConnectedAPI(); try await model.syncSource(api); let created = try await api.post(APIEndpoints.createSubscriptionTemplate, body: CreateSubscriptionTemplateRequest(expectedRevision: 0, name: newName, format: newFormat, document: copying ? model.document.objectValue : nil)); try await model.loadList(api); store.templateWindows.open(created, store: store); NotificationCenter.default.post(name: .templateListChanged, object: nil) } }.disabled(newName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty).keyboardShortcut(.defaultAction) }
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
                            try model.requireSession(api)
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
            Text(L10n.text("同步更新整份配置与节点注入中的引用。"))
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
            HStack { Button(L10n.text("取消")) { archiving = false }; Spacer(); Button(L10n.text("归档"), role: .destructive) { archiving = false; model.run { let api = try store.requireConnectedAPI(); if model.dirty { try await model.save(api) }; try await model.update(api, archive: true, replacement: model.templates.first { $0.id == replacementID }) } }.disabled(model.selected?.isDefault == true && replacementID.isEmpty) }
        }.padding(24).frame(width: 460)
    }
    private func handleAction(_ action: String?) {
        guard let action, let selected = model.selected else { return }
        model.requestedAction = nil
        switch action {
        case "copy": copying = true; newName = selected.name; newFormat = selected.format; creating = true
        case "rename": rename = selected.name; renaming = true
        case "default": model.run { try await model.update(store.requireConnectedAPI(), makeDefault: true) }
        case "export": model.run {
            let api = try store.requireConnectedAPI(); try await model.save(api)
            let package = try await api.get(APIEndpoints.exportSubscriptionTemplate(id: selected.id))
                    try model.requireSession(api)
            exportFile = TemplateTextFile(JSONValue.object(package).pretty); exportName = "\(selected.name).json"; exportType = .json; exporting = true
        }
        case "archive": replacementID = ""; archiving = true
        default: break
        }
    }
}
