import SwiftUI
import UniformTypeIdentifiers

struct SubscriptionTemplateWorkbench: View {
    @Bindable var store: ManagementStore
    @Bindable var model: SubscriptionTemplateStore
    @State private var toast = ToastCenter()
    @State private var diagnosticsExpanded = false
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

    private var windowSubtitle: String {
        let draftStatus = model.dirty ? L10n.text("未保存") : L10n.text("草稿已保存")
        let versionStatus = model.selected?.publishedVersion.map { "v\($0)" } ?? L10n.text("未发布")
        return "\(draftStatus) · \(versionStatus)"
    }

    var body: some View {
        NavigationSplitView(columnVisibility: .constant(.all)) {
            SubscriptionTemplateSidebar(store: store, model: model)
                .navigationSplitViewColumnWidth(min: 185, ideal: 215, max: 260)
        } detail: {
            editorContent
                .navigationSplitViewColumnWidth(min: 480, ideal: 760)
                .toolbar { editorToolbar }
        }
        .navigationSplitViewStyle(.balanced)
        .navigationTitle(model.selected?.name ?? L10n.text("订阅模板"))
        .navigationSubtitle(windowSubtitle)
        .toolbar(removing: .sidebarToggle)
        .toastOverlay(toast)
        .accessibilityIdentifier("templates.workbench")
        .onChange(of: model.requestedAction) { _, action in handleAction(action) }
        .onChange(of: model.errorCount) { _, count in
            if count > 0 { diagnosticsExpanded = true }
        }
        .sheet(isPresented: $confirmingPublish) { publishSheet }
        .sheet(isPresented: $refactoring) { refactorSheet }
        .sheet(isPresented: $creating) { createSheet }
        .sheet(isPresented: $importing, onDismiss: { model.pendingImport = nil; importReview = nil }) { importSheet }
        .sheet(isPresented: $archiving) { archiveSheet }
        .alert(L10n.text("重命名模板"), isPresented: $renaming) {
            TextField(L10n.text("模板名称"), text: $rename)
            Button(L10n.text("保存")) {
                model.run {
                    try await model.update(store.requireConnectedAPI(), name: rename)
                    toast.show(L10n.text("模板已重命名"), tone: .positive)
                }
            }
            Button(L10n.text("取消"), role: .cancel) {}
        }
        .fileImporter(isPresented: $pickingFile, allowedContentTypes: [.json, .yaml, .plainText]) { result in
            do {
                let url = try result.get()
                let access = url.startAccessingSecurityScopedResource()
                defer { if access { url.stopAccessingSecurityScopedResource() } }
                importText = try String(contentsOf: url, encoding: .utf8)
            } catch {
                model.error = error.localizedDescription
            }
        }
        .fileExporter(isPresented: $exporting, document: exportFile, contentType: exportType, defaultFilename: exportName) { result in
            if case .failure(let error) = result {
                model.error = error.localizedDescription
            } else if case .success = result {
                toast.show(L10n.text("模板已导出"), detail: exportName, tone: .positive)
            }
        }
    }

    @ViewBuilder
    private var editorContent: some View {
        if model.selected != nil {
            VStack(alignment: .leading, spacing: 0) {
                alertBanners
                SubscriptionTemplateModules(store: store, model: model, toast: toast)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                SubscriptionTemplateDiagnosticsBar(store: store, model: model, isExpanded: $diagnosticsExpanded)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            ContentUnavailableView(
                L10n.text("订阅模板"),
                systemImage: "doc.text",
                description: Text(L10n.text("选择模板，或新建独立的全局配置。"))
            )
        }
    }

    @ViewBuilder
    private var alertBanners: some View {
        if model.suspended || model.error != nil || model.remoteConflict != nil {
            VStack(alignment: .leading, spacing: 10) {
                if model.suspended {
                    HStack(spacing: 8) {
                        Image(systemName: "exclamationmark.triangle.fill")
                            .foregroundStyle(.orange)
                        Text(L10n.text("当前服务已切换；本地内容已保留，远端操作已暂停。"))
                            .foregroundStyle(.orange)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .padding(10)
                    .background(.orange.opacity(0.1), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
                }
                if let error = model.error {
                    HStack(alignment: .top, spacing: 8) {
                        Image(systemName: "xmark.octagon.fill")
                            .foregroundStyle(.red)
                        Text(error)
                            .foregroundStyle(.red)
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .accessibilityIdentifier("templates.error")
                        Button {
                            model.error = nil
                        } label: {
                            Image(systemName: "xmark")
                                .font(.caption.weight(.semibold))
                                .foregroundStyle(.secondary)
                        }
                        .buttonStyle(.plain)
                        .help(L10n.text("关闭提示"))
                    }
                    .padding(10)
                    .background(.red.opacity(0.08), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
                }
                if let remote = model.remoteConflict {
                    VStack(alignment: .leading, spacing: 10) {
                        HStack(spacing: 8) {
                            Image(systemName: "arrow.triangle.2.circlepath.circle.fill")
                                .foregroundStyle(.orange)
                            Text(L10n.text("服务端版本与本地修改冲突，本地内容已保留。"))
                                .font(.callout.weight(.medium))
                            Spacer()
                            Button(L10n.text("保留本地修改，采用最新修订号")) {
                                model.adoptRemoteRevision()
                                toast.show(L10n.text("已采用最新修订号并保留本地修改"), tone: .positive)
                            }
                            .buttonStyle(.borderedProminent)
                            .controlSize(.small)
                        }
                        DisclosureGroup(L10n.text("服务端版本与本地修改（本地内容已保留）")) {
                            ScrollView {
                                TemplateDiffView(
                                    diff: SubscriptionTemplateStore.difference(remote, model.document),
                                    emptyMessage: L10n.text("本地修改与服务端最新版本内容一致。"),
                                    toast: toast
                                )
                                .padding(.top, 6)
                            }
                            .frame(maxHeight: 220)
                        }
                        .font(.caption)
                    }
                    .padding(10)
                    .background(.orange.opacity(0.08), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            Divider()
        }
    }

    @ToolbarContentBuilder
    private var editorToolbar: some ToolbarContent {
        ToolbarItemGroup(placement: .automatic) {
            Button {
                model.run { try await model.switchMode(store.requireConnectedAPI()) }
            } label: {
                Label(
                    model.sourceMode ? L10n.text("切换到可视化") : L10n.text("切换到源码"),
                    systemImage: model.sourceMode ? "slider.horizontal.3" : "chevron.left.forwardslash.chevron.right"
                )
            }
            .labelStyle(.iconOnly)
            .help(model.sourceMode ? L10n.text("切换到可视化") : L10n.text("切换到源码"))
            .disabled(model.busy)
            .accessibilityIdentifier("templates.sourceMode")

            if model.busy {
                ProgressView()
                    .controlSize(.small)
                    .padding(.horizontal, 4)
            }
        }
        ToolbarItemGroup(placement: .primaryAction) {
            Button { model.undo() } label: { Image(systemName: "arrow.uturn.backward") }
                .help(L10n.text("撤销"))
                .disabled(model.undoStack.isEmpty || model.sourceMode || model.busy)
                .accessibilityIdentifier("templates.undo")
            Button { model.redo() } label: { Image(systemName: "arrow.uturn.forward") }
                .help(L10n.text("重做"))
                .disabled(model.redoStack.isEmpty || model.sourceMode || model.busy)
                .accessibilityIdentifier("templates.redo")
            Button(L10n.text("校验"), systemImage: "checkmark.shield") {
                model.run {
                    try await model.validate(store.requireConnectedAPI())
                    if model.errorCount > 0 {
                        diagnosticsExpanded = true
                    } else if model.warningCount > 0 {
                        diagnosticsExpanded = true
                        toast.show(L10n.text("校验完成，发现 {0} 条警告", String(model.warningCount)), tone: .warning)
                    } else {
                        toast.show(L10n.text("校验通过，配置有效"), tone: .positive)
                    }
                }
            }
            .labelStyle(.iconOnly)
            .help(L10n.text("校验"))
            .disabled(model.suspended || model.busy)
            .accessibilityIdentifier("templates.validate")

            Button(L10n.text("保存草稿"), systemImage: "square.and.arrow.down") {
                model.run {
                    try await model.save(store.requireConnectedAPI())
                    toast.show(L10n.text("草稿已保存"), tone: .positive)
                }
            }
            .labelStyle(.iconOnly)
            .help(L10n.text("保存草稿"))
            .keyboardShortcut("s", modifiers: .command)
            .disabled(model.suspended || model.busy)
            .accessibilityIdentifier("templates.save")

            Button(L10n.text("发布"), systemImage: "paperplane") {
                model.run {
                    try await model.preparePublish(store.requireConnectedAPI())
                    confirmingPublish = true
                }
            }
            .labelStyle(.iconOnly)
            .buttonStyle(.borderedProminent)
            .help(L10n.text("发布"))
            .disabled(model.suspended || model.busy)
            .accessibilityIdentifier("templates.publish")

            if let selected = model.selected {
                editorMenu(selected).disabled(model.busy)
            }
        }
    }

    private func editorMenu(_ selected: SubscriptionTemplate) -> some View {
        Menu {
            Button(L10n.text("复制模板")) { copying = true; newName = selected.name; newFormat = selected.format; creating = true }
            Button(L10n.text("重命名组或规则集引用…")) { oldReference = ""; newReference = ""; refactoring = true }
            Button(L10n.text("重命名模板")) { rename = selected.name; renaming = true }
            Button(L10n.text("设为默认模板")) {
                model.run {
                    try await model.update(store.requireConnectedAPI(), makeDefault: true)
                    toast.show(L10n.text("已设为默认模板"), tone: .positive)
                }
            }
            .disabled(selected.publishedVersion == nil)
            Button(L10n.text("导入配置或模板…")) {
                model.run {
                    try await model.syncSource(store.requireConnectedAPI())
                    importText = ""
                    importing = true
                }
            }
            Button(L10n.text("导出完整模板…")) {
                model.run {
                    let api = try store.requireConnectedAPI()
                    try await model.save(api)
                    let package = try await api.get(APIEndpoints.exportSubscriptionTemplate(id: selected.id))
                    try model.requireSession(api)
                    exportFile = TemplateTextFile(JSONValue.object(package).pretty)
                    exportName = "\(selected.name).json"
                    exportType = .json
                    exporting = true
                }
            }
            Button(L10n.text("归档模板…"), role: .destructive) { replacementID = ""; archiving = true }
        } label: { Image(systemName: "ellipsis.circle") }
    }

    private var publishSheet: some View {
        let nextVersion = (model.selected?.publishedVersion ?? 0) + 1
        return VStack(alignment: .leading, spacing: 16) {
            HStack(spacing: 10) {
                Text(L10n.text("发布检查")).font(.title2.bold())
                StatusBadge("v\(nextVersion)", tone: .accent, size: .compact, showDot: false)
                    .monospacedDigit()
                Spacer()
            }
            if model.warningCount > 0 {
                HStack(spacing: 8) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange)
                    Text(L10n.text("校验发现 {0} 条警告，发布前请确认不影响目标平台。", String(model.warningCount)))
                        .font(.caption)
                }
                .padding(10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(.orange.opacity(0.1), in: RoundedRectangle(cornerRadius: 8))
            }
            ScrollView {
                TemplateDiffView(
                    diff: model.publishDiff,
                    emptyMessage: L10n.text("与上次发布版本相比无配置变更。"),
                    toast: toast
                )
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            Divider()
            HStack {
                Button(L10n.text("取消")) { confirmingPublish = false }
                    .keyboardShortcut(.cancelAction)
                Spacer()
                Button(L10n.text("保存并发布")) {
                    confirmingPublish = false
                    model.run {
                        try await model.publish(store.requireConnectedAPI())
                        let version = model.selected?.publishedVersion ?? nextVersion
                        toast.show(L10n.text("已发布 v{0}", String(version)), tone: .positive)
                    }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
            }
        }
        .padding(24)
        .frame(width: 680, height: 540)
    }

    private var createSheet: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(copying ? L10n.text("复制模板") : L10n.text("新建模板")).font(.title2.bold())
            TextField(L10n.text("模板名称"), text: $newName)
            Picker(L10n.text("配置格式"), selection: $newFormat) {
                Text("Mihomo").tag("mihomo")
                Text("sing-box").tag("singbox")
            }
            .disabled(copying)
            HStack {
                Button(L10n.text("取消")) { creating = false }
                    .keyboardShortcut(.cancelAction)
                Spacer()
                Button(L10n.text("创建")) {
                    creating = false
                    model.run {
                        let api = try store.requireConnectedAPI()
                        try await model.syncSource(api)
                        let created = try await api.post(
                            APIEndpoints.createSubscriptionTemplate,
                            body: CreateSubscriptionTemplateRequest(
                                expectedRevision: 0,
                                name: newName,
                                format: newFormat,
                                document: copying ? model.document.objectValue : nil
                            )
                        )
                        try await model.loadList(api)
                        store.templateWindows.open(created, store: store)
                        NotificationCenter.default.post(name: .templateListChanged, object: nil)
                    }
                }
                .buttonStyle(.borderedProminent)
                .disabled(newName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                .keyboardShortcut(.defaultAction)
            }
        }
        .padding(24)
        .frame(width: 400)
    }

    private var importSheet: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(L10n.text("导入配置或模板…")).font(.title2.bold())
            if let analysis = model.pendingImport {
                HSplitView {
                    ScrollView {
                        importAnalysisSummary(analysis)
                            .padding(.trailing, 8)
                    }
                    .frame(minWidth: 320)
                    if let review = importReview {
                        ScrollView {
                            VStack(alignment: .leading, spacing: 12) {
                                Text(L10n.text("确认前可调整各组的节点选择规则；这些规则不会绑定到某个用户。"))
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                SubscriptionTemplateInjections(store: review)
                            }
                            .padding(.leading, 20)
                            .padding(.trailing, 8)
                            .padding(.vertical, 8)
                        }
                        .frame(minWidth: 360)
                    }
                }
            } else {
                HStack {
                    Text(L10n.text("粘贴配置文本或从本地文件导入"))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Spacer()
                    Button(L10n.text("选择文件…"), systemImage: "doc") { pickingFile = true }
                }
                TextEditor(text: $importText)
                    .font(.system(.body, design: .monospaced))
                    .overlay(
                        RoundedRectangle(cornerRadius: 6)
                            .strokeBorder(.primary.opacity(0.12), lineWidth: 0.5)
                    )
                    .accessibilityIdentifier("templates.import.source")
            }
            if let error = model.error {
                Text(error).foregroundStyle(.red).textSelection(.enabled)
            }
            Divider()
            HStack {
                Button(L10n.text("取消")) {
                    model.pendingImport = nil
                    importReview = nil
                    importing = false
                }
                .keyboardShortcut(.cancelAction)
                .disabled(model.busy)
                Spacer()
                if model.pendingImport != nil {
                    Button(L10n.text("确认映射并写入本地草稿")) {
                        model.run {
                            let api = try store.requireConnectedAPI()
                            if let review = importReview {
                                try await review.validate(api)
                                try model.requireSession(api)
                                model.pendingImport?["document"] = review.document
                                model.pendingImport?["diagnostics"] = .array(review.diagnostics)
                            }
                            model.acceptImport()
                            importing = false
                            toast.show(L10n.text("已写入本地草稿"), tone: .positive)
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
                    .disabled(model.busy)
                } else {
                    Button(L10n.text("分析导入")) {
                        model.run {
                            let api = try store.requireConnectedAPI()
                            try await model.analyzeImport(api, source: importText)
                            let review = SubscriptionTemplateStore()
                            review.connectedAPI = api
                            review.selected = model.selected
                            review.catalog = model.catalog
                            review.document = model.pendingImport?["document"] ?? .object([:])
                            review.baseline = review.document
                            review.focusedPath = "/injections/0"
                            importReview = review
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
                    .disabled(importText.isEmpty || model.busy)
                    .accessibilityIdentifier("templates.import.analyze")
                }
            }
        }
        .padding(24)
        .frame(width: model.pendingImport == nil ? 680 : 980, height: 600)
        .interactiveDismissDisabled(model.busy)
    }

    @ViewBuilder
    private func importAnalysisSummary(_ analysis: [String: JSONValue]) -> some View {
        let removedNodes = analysis["removed_nodes"]?.arrayValue ?? []
        let mappings = analysis["reference_mapping"]?.arrayValue ?? []
        let removedProviders = analysis["removed_providers"]?.arrayValue ?? []
        let diagnostics = analysis["diagnostics"]?.arrayValue ?? []
        VStack(alignment: .leading, spacing: 12) {
            Text(L10n.text("导入分析：将移除手工节点，并将引用替换为授权动态节点。"))
                .font(.callout)
            HStack(spacing: 8) {
                StatusBadge(L10n.text("移除节点 {0}", String(removedNodes.count)), tone: .neutral, size: .compact, showDot: false)
                StatusBadge(L10n.text("映射组 {0}", String(mappings.count)), tone: .active, size: .compact, showDot: false)
                if !removedProviders.isEmpty {
                    StatusBadge(L10n.text("移除提供者 {0}", String(removedProviders.count)), tone: .warning, size: .compact, showDot: false)
                }
                if !diagnostics.isEmpty {
                    StatusBadge(L10n.text("诊断 {0}", String(diagnostics.count)), tone: .warning, size: .compact, showDot: false)
                }
            }
            if !mappings.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    Text(L10n.text("引用映射摘要"))
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.secondary)
                    ForEach(Array(mappings.enumerated()), id: \.offset) { _, entry in
                        let group = entry["group"].stringValue ?? ""
                        let refs = (entry["removed_references"].arrayValue ?? []).compactMap(\.stringValue)
                        let providerRefs = (entry["removed_provider_references"].arrayValue ?? []).compactMap(\.stringValue)
                        VStack(alignment: .leading, spacing: 3) {
                            Text(group.isEmpty ? L10n.text("未命名组") : group)
                                .font(.caption.weight(.medium))
                            if !(refs + providerRefs).isEmpty {
                                Text((refs + providerRefs).joined(separator: ", "))
                                    .font(.caption2.monospaced())
                                    .foregroundStyle(.secondary)
                            }
                        }
                        .padding(8)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(.quaternary.opacity(0.25), in: RoundedRectangle(cornerRadius: 6))
                    }
                }
            }
            DisclosureGroup(L10n.text("查看完整分析 JSON")) {
                Text(JSONValue.object(analysis.filter { $0.key != "document" }).pretty)
                    .font(.system(.caption, design: .monospaced))
                    .textSelection(.enabled)
            }
            .font(.caption)
        }
    }

    private var refactorSheet: some View {
        let candidates = model.refactorCandidates(kind: refactorKind)
        return VStack(alignment: .leading, spacing: 16) {
            Text(L10n.text("重命名组或规则集引用…")).font(.title2.bold())
            Picker(L10n.text("对象类型"), selection: $refactorKind) {
                Text(L10n.text("代理组")).tag("group")
                Text(L10n.text("规则集")).tag("rule_set")
            }
            HStack(spacing: 8) {
                TextField(L10n.text("原名称"), text: $oldReference)
                if !candidates.isEmpty {
                    Menu {
                        ForEach(candidates, id: \.self) { candidate in
                            Button(candidate) { oldReference = candidate }
                        }
                    } label: {
                        Label(L10n.text("选择已有项"), systemImage: "chevron.down.circle")
                    }
                    .fixedSize()
                }
            }
            TextField(L10n.text("新名称"), text: $newReference)
            Text(L10n.text("同步更新整份配置与节点注入中的引用。"))
                .font(.caption)
                .foregroundStyle(.secondary)
            if let error = model.error {
                Text(error).foregroundStyle(.red).textSelection(.enabled)
            }
            HStack {
                Button(L10n.text("取消")) { refactoring = false }
                    .keyboardShortcut(.cancelAction)
                Spacer()
                Button(L10n.text("重命名")) {
                    model.run {
                        try await model.refactor(store.requireConnectedAPI(), kind: refactorKind, old: oldReference, new: newReference)
                        refactoring = false
                        toast.show(L10n.text("已重命名引用"), detail: "\(oldReference) → \(newReference)", tone: .positive)
                    }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(oldReference.isEmpty || newReference.isEmpty || model.busy)
            }
        }
        .padding(24)
        .frame(width: 480)
    }

    private var archiveSheet: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(L10n.text("归档模板…")).font(.title2.bold())
            if model.selected?.isDefault == true {
                Picker(L10n.text("同格式替代默认模板"), selection: $replacementID) {
                    Text(L10n.text("选择模板")).tag("")
                    ForEach(model.templates.filter { $0.id != model.selected?.id && $0.format == model.selected?.format && $0.publishedVersion != nil }) {
                        Text($0.name).tag($0.id)
                    }
                }
            }
            Text(L10n.text("归档后，此模板的显式订阅地址返回 404。"))
                .font(.caption)
                .foregroundStyle(.secondary)
            HStack {
                Button(L10n.text("取消")) { archiving = false }
                    .keyboardShortcut(.cancelAction)
                Spacer()
                Button(L10n.text("归档"), role: .destructive) {
                    archiving = false
                    model.run {
                        let api = try store.requireConnectedAPI()
                        if model.dirty { try await model.save(api) }
                        try await model.update(api, archive: true, replacement: model.templates.first { $0.id == replacementID })
                    }
                }
                .buttonStyle(.borderedProminent)
                .disabled(model.selected?.isDefault == true && replacementID.isEmpty)
            }
        }
        .padding(24)
        .frame(width: 460)
    }

    private func handleAction(_ action: String?) {
        guard let action, let selected = model.selected else { return }
        model.requestedAction = nil
        switch action {
        case "copy":
            copying = true; newName = selected.name; newFormat = selected.format; creating = true
        case "rename":
            rename = selected.name; renaming = true
        case "default":
            model.run {
                try await model.update(store.requireConnectedAPI(), makeDefault: true)
                toast.show(L10n.text("已设为默认模板"), tone: .positive)
            }
        case "export":
            model.run {
                let api = try store.requireConnectedAPI()
                try await model.save(api)
                let package = try await api.get(APIEndpoints.exportSubscriptionTemplate(id: selected.id))
                try model.requireSession(api)
                exportFile = TemplateTextFile(JSONValue.object(package).pretty)
                exportName = "\(selected.name).json"
                exportType = .json
                exporting = true
            }
        case "archive":
            replacementID = ""; archiving = true
        default:
            break
        }
    }
}
