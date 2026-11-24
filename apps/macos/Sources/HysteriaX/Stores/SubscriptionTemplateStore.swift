import Foundation
import Observation

@MainActor @Observable
final class SubscriptionTemplateStore {
    var connectedAPI: APIClient?
    @ObservationIgnored var sessionAvailable: ((APIClient) -> Bool)?
    var closed = false
    var suspended: Bool { if let api = connectedAPI { return sessionAvailable?(api) == false }; return false }
    var module = "基础配置"
    var expandedPaths: Set<String> = []
    var collectionSelections: [String: String] = [:]
    var injectionsExpanded = true
    var injectionSelection = 0
    var requestedAction: String?
    var publishDiff: JSONValue = .object([:])
    var documentGeneration = UUID()
    var saving = false
    var serviceName = ""
    var templates: [SubscriptionTemplate] = []
    var selected: SubscriptionTemplate?
    var document: JSONValue = .object([:]) {
        didSet {
            if document != oldValue {
                documentGeneration = UUID()
                isDocumentDirty = (document != baseline)
                hasValidated = false
            }
        }
    }
    var baseline: JSONValue = .object([:]) {
        didSet {
            if baseline != oldValue {
                isDocumentDirty = (document != baseline)
            }
        }
    }
    var catalog: JSONValue = .object([:]) {
        didSet { rebuildCatalogCaches() }
    }
    var source = "" { didSet { if source != oldValue { documentGeneration = UUID() } } }
    var sourceBaseline = ""
    var sourceMode = false
    var diagnostics: [JSONValue] = [] {
        didSet { rebuildDiagnosticSummaries() }
    }
    var history: [JSONValue] = []
    var error: String?
    struct PendingDeletion {
        let before: JSONValue
        let after: JSONValue
        let presenterPath: [String]
        let names: [String]
        let changes: [String]
    }
    var pendingDeletion: PendingDeletion?
    var remoteConflict: JSONValue?
    var conflictTemplate: SubscriptionTemplate?
    var comparison: JSONValue?
    var comparisonVersion: Int?
    var busy = false
    var undoStack: [JSONValue] = []
    var redoStack: [JSONValue] = []
    var pendingImport: [String: JSONValue]?
    var focusGeneration = UUID()
    var focusedPath = "" { didSet { focusGeneration = UUID() } }
    private(set) var isDocumentDirty = false
    var dirty: Bool { isDocumentDirty || (sourceMode && source != sourceBaseline) }
    var editorWindowTitle: String {
        let status = dirty ? L10n.text("未保存") : L10n.text("草稿已保存")
        return "\(selected?.name ?? "") · \(status) · \(selected?.format ?? "") · \(serviceName)"
    }
    private(set) var cachedSchema = TemplateSchema(root: .null)
    var schema: TemplateSchema { cachedSchema }
    private(set) var rootProperties: [String: JSONValue] = [:]
    private struct CompiledPlatformRule {
        let pattern: [String]
        let requiredValue: JSONValue?
        let allowed: Set<String>
    }
    @ObservationIgnored private var compiledPlatformRules: [CompiledPlatformRule] = []
    @ObservationIgnored private var pathRulesCache: [[String]: [CompiledPlatformRule]] = [:]
    struct TemplateSearchableField: Identifiable, Hashable {
        let path: [String]
        let pointer: String
        let title: String
        let module: String
        let section: String
        let keywords: String
        var id: String { pointer }
    }
    private(set) var searchableFields: [TemplateSearchableField] = []
    struct ModuleDiagnosticSummary: Equatable {
        var errors = 0
        var warnings = 0
    }
    private(set) var errorCount = 0
    private(set) var warningCount = 0
    private(set) var moduleDiagnostics: [String: ModuleDiagnosticSummary] = [:]
    private(set) var diagnosticsByPointer: [String: [JSONValue]] = [:]
    var hasValidated = false
    struct TemplateModuleField: Identifiable {
        let path: [String]
        let definition: JSONValue
        let isComplex: Bool
        var id: String { SubscriptionTemplateStore.pointer(path) }
    }
    struct TemplateModuleSection: Identifiable {
        let title: String
        let fields: [TemplateModuleField]
        var dialerPath: [String]? = nil
        var id: String { dialerPath.map { "dialer:" + SubscriptionTemplateStore.pointer($0) } ?? title }
    }
    struct TemplateModuleLayoutSnapshot {
        let sections: [TemplateModuleSection]
        let unknownNestedFields: [String]
    }
    struct TemplateFieldPresentation {
        let sourceDefinition: JSONValue
        let editingDefinition: JSONValue
        let value: JSONValue
        let schema: JSONValue
        let variants: [JSONValue]
        let scalarListItemDefinition: JSONValue?
        let enumChoices: [JSONValue]?
        let enumOptions: TemplateEnumPickerOptions?
        let isRequired: Bool
        let guidance: String?
        let helpText: String
        let hasPreviewValue: Bool
        let platforms: [String]
        let allowed: Bool
        let isExplicitlySet: Bool
        let referenceKind: String?
        let isSingbox: Bool
    }
    @ObservationIgnored private var renderCacheGeneration = UUID()
    @ObservationIgnored private var renderCacheLanguage = ""
    @ObservationIgnored private var moduleLayoutCache: [String: TemplateModuleLayoutSnapshot] = [:]
    @ObservationIgnored private var fieldPresentationCache: [[String]: TemplateFieldPresentation] = [:]
    var common: JSONValue { document["common"] }
    var effective: JSONValue { common }
    nonisolated static func path(_ pointer: String) -> [String] { pointer.dropFirst().components(separatedBy: "/").map { $0.replacingOccurrences(of: "~1", with: "/").replacingOccurrences(of: "~0", with: "~") } }
    nonisolated static func pointer(_ path: [String]) -> String { "/" + path.map { $0.replacingOccurrences(of: "~", with: "~0").replacingOccurrences(of: "/", with: "~1") }.joined(separator: "/") }
    func edit(_ body: (inout JSONValue) -> Void) {
        let before = document; var next = document; body(&next); document = next
        if document != before { undoStack.append(before); if undoStack.count > 100 { undoStack.removeFirst() }; redoStack.removeAll() }
    }
    func undo() { if let previous = undoStack.popLast() { redoStack.append(document); document = previous } }
    func redo() { if let next = redoStack.popLast() { undoStack.append(document); document = next } }
    struct EditorCheckpoint {
        let document: JSONValue
        let undoStack: [JSONValue]
        let redoStack: [JSONValue]
        let error: String?
    }
    func editorCheckpoint() -> EditorCheckpoint {
        EditorCheckpoint(document: document, undoStack: undoStack, redoStack: redoStack, error: error)
    }
    func restoreEditorCheckpoint(_ checkpoint: EditorCheckpoint) {
        pendingDeletion = nil
        document = checkpoint.document
        undoStack = checkpoint.undoStack
        redoStack = checkpoint.redoStack
        error = checkpoint.error
    }
    func state(_ path: [String]) -> String { common.at(path) == nil ? "inherit" : "set" }
    func setState(_ state: String, path: [String], schema: JSONValue) {
        if state == "set" { set(path, common.at(path) ?? editorValue(schema, path: path)); return }
        for index in path.indices where common.at(Array(path.prefix(index)))?.arrayValue != nil {
            let parent = Array(path.prefix(index)); var array = common.at(parent) ?? .array([])
            array.set(Array(path.dropFirst(index)), nil); set(parent, array); return
        }
        edit { $0.set(["common"] + path, nil) }
    }
    private func portableInitial(_ value: JSONValue, path: [String]) -> JSONValue {
        if let map = value.objectValue {
            return .object(map.reduce(into: [:]) { output, item in
                let nextPath = path + [item.key]
                if allowed(nextPath, value: item.value) { output[item.key] = portableInitial(item.value, path: nextPath) }
            })
        }
        if let array = value.arrayValue { return .array(array.enumerated().map { portableInitial($0.element, path: path + [String($0.offset)]) }) }
        return value
    }
    func initialValue(_ definition: JSONValue, path: [String]) -> JSONValue { portableInitial(schema.initial(definition), path: path) }
    func editorValue(_ definition: JSONValue, path: [String]) -> JSONValue {
        if let value = effective.at(path) {
            if TemplateSchema.isObjectResolver(path), schema.resolve(definition)["type"].stringValue == "object",
               let server = value.stringValue { return .object(["server": .string(server)]) }
            return value
        }
        let parent = Array(path.dropLast())
        if path.last == "server", TemplateSchema.isObjectResolver(parent), let server = effective.at(parent)?.stringValue { return .string(server) }
        if let value = TemplateEditorGuidance.defaultValue(path, schema: schema, value: effective) { return value }
        return initialValue(definition, path: path)
    }
    func restoreDefault(path: [String], definition: JSONValue) {
        if let value = schema.resolve(definition, value: effective.at(path) ?? .null).objectValue?["default"] { set(path, portableInitial(value, path: path)) }
        else { setState("inherit", path: path, schema: definition) }
    }
    func set(_ path: [String], _ value: JSONValue) {
        for index in path.indices.dropLast() {
            let parent = Array(path.prefix(index + 1))
            if TemplateSchema.isObjectResolver(parent), let server = effective.at(parent)?.stringValue {
                var object: JSONValue = .object(["server": .string(server)])
                object.set(Array(path.dropFirst(index + 1)), value)
                set(parent, object)
                return
            }
        }
        let referenceIndex = TemplateReferenceIndex(format: selected?.format ?? catalog["format"].stringValue ?? "mihomo", schema: schema)
        if let collection = referenceIndex.collectionPaths.first(where: { path.starts(with: $0) && path.count > $0.count + 1 }),
           ["name", "tag"].contains(path[collection.count + 1]) {
            let owner = Array(path.prefix(collection.count + 1))
            let oldNames = Set(referenceIndex.identities(at: owner, in: effective))
            var changed = effective; changed.set(path, value)
            let newNames = Set(referenceIndex.identities(at: owner, in: changed))
            let removed = oldNames.subtracting(newNames), added = newNames.subtracting(oldNames)
            if removed.count == 1, added.count == 1, let old = removed.first, let new = added.first {
                renameReference(old, new: new.name, index: referenceIndex)
                return
            }
            for old in removed {
                let next = Self.applyingConfiguration(changed, to: document)
                guard permitsDeletion(candidate: next, names: [old.name], kind: old.kind) else { return }
            }
        }
        edit { $0.set(["common"] + path, value) }
    }
    func setString(_ path: [String], _ text: String, definition: JSONValue) {
        if text.isEmpty, TemplateEditorGuidance.isSingbox(schema) {
            setState("inherit", path: path, schema: definition)
        } else {
            set(path, .string(text))
        }
    }
    func supportedPlatforms(_ path: [String], value: JSONValue? = nil) -> [String] {
        let matchingRules: [CompiledPlatformRule]
        if let cached = pathRulesCache[path] {
            matchingRules = cached
        } else {
            func matches(_ pattern: ArraySlice<String>, _ path: ArraySlice<String>) -> Bool {
                guard let first = pattern.first else { return path.isEmpty }
                if first == "**" { return matches(pattern.dropFirst(), path) || (!path.isEmpty && matches(pattern, path.dropFirst())) }
                return !path.isEmpty && (first == "*" || first == path.first) && matches(pattern.dropFirst(), path.dropFirst())
            }
            let filtered = compiledPlatformRules.filter { matches($0.pattern[...], path[...]) }
            if pathRulesCache.count > 4096 { pathRulesCache.removeAll(keepingCapacity: true) }
            pathRulesCache[path] = filtered
            matchingRules = filtered
        }
        if matchingRules.isEmpty { return TemplatePlatform.all }
        var platforms = TemplatePlatform.all
        for rule in matchingRules {
            if let required = rule.requiredValue, value != required { continue }
            platforms = platforms.filter { rule.allowed.contains($0) }
        }
        return platforms
    }
    func allowed(_ path: [String], value: JSONValue? = nil) -> Bool {
        !supportedPlatforms(path, value: value).isEmpty
    }
    static func requireSchema2(_ document: JSONValue) throws {
        let keys = Set(document.objectValue?.keys.map { $0 } ?? [])
        guard document["schema_version"].integerValue == 2,
              document["common"].objectValue != nil,
              keys.isSubset(of: ["schema_version", "common", "injections", "platforms"]),
              document.objectValue?["platforms"] == nil || document["platforms"].objectValue?.isEmpty == true else {
            throw APIClientError.server(L10n.text("此模板文档格式不受支持，仅支持 schema 2。"))
        }
    }
    func requireSession(_ api: APIClient) throws {
        if closed || sessionAvailable?(api) == false { throw CancellationError() }
        if let connectedAPI, connectedAPI !== api { throw APIClientError.server(L10n.text("服务连接已变化，本地修改已保留；请刷新模板列表后再提交。")) }
    }
    func reconnect(_ api: APIClient) async throws {
        guard sessionAvailable?(api) != false, !closed, let selected else { throw CancellationError() }
        let remote = try await api.get(APIEndpoints.getSubscriptionTemplateDraft(id: selected.id))
        guard sessionAvailable?(api) != false, !closed else { throw CancellationError() }
        try Self.requireSchema2(.object(remote.document))
        connectedAPI = api
        if remote.template.revision != selected.revision {
            remoteConflict = .object(remote.document); conflictTemplate = remote.template
            error = "服务端修订已变化，本地内容已保留；请处理冲突。"
        }
    }
    func loadList(_ api: APIClient) async throws {
        if sessionAvailable != nil { try requireSession(api) }
        connectedAPI = api
        let items = try await api.collectPages { page in APIEndpoints.listSubscriptionTemplates(page: page, pageSize: 200) }
        try requireSession(api)
        templates = items
    }
    func open(_ template: SubscriptionTemplate, api: APIClient) async throws {
        try requireSession(api)
        connectedAPI = api
        let draft = try await api.get(APIEndpoints.getSubscriptionTemplateDraft(id: template.id))
        try requireSession(api)
        let fields = try await api.get(APIEndpoints.subscriptionTemplateFields(format: template.format))
        try requireSession(api)
        let nextDocument = JSONValue.object(draft.document)
        try Self.requireSchema2(nextDocument)
        selected = draft.template; document = nextDocument; baseline = document; catalog = .object(fields)
        undoStack = []; redoStack = []; diagnostics = []; hasValidated = false; comparison = nil; comparisonVersion = nil; remoteConflict = nil; sourceMode = false
        try await loadHistory(api); try await loadList(api)
    }
    func syncSource(_ api: APIClient) async throws {
        try requireSession(api)
        guard let selected else { return }
        try Self.requireSchema2(document)
        if sourceMode {
            let buffer = source
            let response = try await api.post(APIEndpoints.subscriptionTemplateSource, body: SubscriptionTemplateSourceRequest(format: selected.format, source: buffer))
            try requireSession(api)
            guard buffer == source else { throw CancellationError() }
            if let config = response["config"] { edit { doc in doc.set(["common"], config) } }
        }
    }
    func switchMode(_ api: APIClient) async throws {
        try requireSession(api)
        guard let selected else { return }
        if sourceMode { try await syncSource(api); sourceMode = false }
        else {
            let generation = documentGeneration
            let response = try await api.post(APIEndpoints.subscriptionTemplateSource, body: SubscriptionTemplateSourceRequest(format: selected.format, config: common.objectValue ?? [:]))
            try requireSession(api)
            guard generation == documentGeneration else { throw CancellationError() }
            source = response["source"]?.stringValue ?? ""; sourceBaseline = source; sourceMode = true
        }
    }
    func save(_ api: APIClient) async throws {
        try requireSession(api)
        guard !saving else { throw APIClientError.server("草稿正在保存。") }
        saving = true; defer { saving = false }
        try await syncSource(api)
        guard remoteConflict == nil else { throw APIClientError.server("请先处理服务端修订冲突。") }
        guard let selected else { return }
        do {
            let savedDocument = document; let savedSource = source
            let saved = try await api.put(APIEndpoints.saveSubscriptionTemplateDraft(id: selected.id), body: SaveSubscriptionTemplateDraftRequest(expectedRevision: selected.revision, document: document.objectValue ?? [:]))
            try requireSession(api)
            self.selected = saved; baseline = savedDocument; sourceBaseline = savedSource; remoteConflict = nil
            NotificationCenter.default.post(name: Notification.Name("templateListChanged"), object: nil)
        } catch {
            // Fetch a comparison copy without overwriting the user's document or revision.
            try requireSession(api)
            if let remote = try? await api.get(APIEndpoints.getSubscriptionTemplateDraft(id: selected.id)), remote.template.revision != selected.revision { try requireSession(api); remoteConflict = .object(remote.document); conflictTemplate = remote.template }
            throw error
        }
    }
    func validate(_ api: APIClient) async throws {
        try requireSession(api)
        try await syncSource(api); guard let selected else { return }
        let generation = documentGeneration
        let response = try await api.post(APIEndpoints.validateSubscriptionTemplate(id: selected.id), body: ValidateSubscriptionTemplateRequest(document: document.objectValue))
        try requireSession(api)
        guard generation == documentGeneration else { throw CancellationError() }
        diagnostics = response["diagnostics"]?.arrayValue ?? []
        hasValidated = true
    }
    func preparePublish(_ api: APIClient) async throws {
        try requireSession(api)
        guard remoteConflict == nil else { throw APIClientError.server("请先处理服务端修订冲突。") }
        try await validate(api)
        if diagnostics.contains(where: { $0["severity"].stringValue == "error" }) { throw APIClientError.server("配置未通过校验，请查看诊断。") }
        if let version = selected?.publishedVersion { try await compare(api, version: version) } else { comparison = .object([:]); comparisonVersion = nil }
        publishDiff = Self.difference(comparison ?? .object([:]), document)
    }
    func publish(_ api: APIClient) async throws {
        try requireSession(api)
        guard remoteConflict == nil else { throw APIClientError.server("请先处理服务端修订冲突。") }
        try await validate(api)
        if diagnostics.contains(where: { $0["severity"].stringValue == "error" }) { throw APIClientError.server("配置未通过校验，请查看诊断。") }
        if dirty { try await save(api) }; guard let selected else { return }
        let published = try await api.post(APIEndpoints.publishSubscriptionTemplate(id: selected.id), body: PublishSubscriptionTemplateRequest(expectedRevision: selected.revision))
        try requireSession(api); self.selected = published
        NotificationCenter.default.post(name: Notification.Name("templateListChanged"), object: nil)
        try await loadHistory(api); try await loadList(api)
    }
    func loadHistory(_ api: APIClient) async throws {
        try requireSession(api)
        guard let selected else { return }; history = []; var page = 1
        while true {
            let response = try await api.get(APIEndpoints.listSubscriptionTemplateVersions(id: selected.id, page: page, pageSize: 200))
            try requireSession(api)
            history += response["items"]?.arrayValue ?? []
            if history.count >= response["total"]?.integerValue ?? 0 { break }; page += 1
        }
    }
    func create(_ api: APIClient, name: String, format: String, copy: Bool) async throws {
        try requireSession(api)
        let template = try await api.post(APIEndpoints.createSubscriptionTemplate, body: CreateSubscriptionTemplateRequest(expectedRevision: 0, name: name, format: format, document: copy ? document.objectValue : nil))
        try await loadList(api); try await open(template, api: api)
    }
    func update(_ api: APIClient, name: String? = nil, makeDefault: Bool = false, archive: Bool = false, replacement: SubscriptionTemplate? = nil) async throws {
        try requireSession(api)
        guard let selected else { return }
        let updated = try await api.patch(APIEndpoints.updateSubscriptionTemplate(id: selected.id), body: UpdateSubscriptionTemplateRequest(expectedRevision: selected.revision, name: name, isDefault: makeDefault ? true : nil, archived: archive ? true : nil, replacementId: replacement?.id, replacementRevision: replacement?.revision))
        try requireSession(api); self.selected = updated
        NotificationCenter.default.post(name: Notification.Name("templateListChanged"), object: nil)
        try await loadList(api)
        if archive { self.selected = nil }
    }
    func rollback(_ api: APIClient, version: Int) async throws {
        try requireSession(api)
        guard let selected else { return }
        let next = try await api.post(APIEndpoints.rollbackSubscriptionTemplate(id: selected.id), body: RollbackSubscriptionTemplateRequest(expectedRevision: selected.revision, version: version))
        try await open(next, api: api); try await loadList(api)
        NotificationCenter.default.post(name: Notification.Name("templateListChanged"), object: nil)
    }
    func analyzeImport(_ api: APIClient, source: String) async throws {
        try requireSession(api)
        guard let selected else { return }
        let imported = try await api.post(APIEndpoints.importSubscriptionTemplate, body: ImportSubscriptionTemplateRequest(format: selected.format, source: source))
        try requireSession(api); pendingImport = imported
    }
    func acceptImport() {
        guard let next = pendingImport?["document"] else { return }
        do {
            try Self.requireSchema2(next)
            edit { $0 = next }; diagnostics = pendingImport?["diagnostics"]?.arrayValue ?? []; hasValidated = true
            pendingImport = nil; sourceMode = false
        } catch { self.error = error.localizedDescription }
    }
    func refactor(_ api: APIClient, kind: String, old: String, new: String) async throws {
        try requireSession(api)
        try await syncSource(api); guard let selected else { return }
        let response = try await api.post(APIEndpoints.refactorSubscriptionTemplate(id: selected.id), body: RefactorSubscriptionTemplateRequest(expectedRevision: selected.revision, document: document.objectValue ?? [:], kind: kind, oldName: old, newName: new))
        try requireSession(api)
        if let next = response["document"] { try Self.requireSchema2(next); edit { $0 = next }; diagnostics = response["diagnostics"]?.arrayValue ?? []; hasValidated = true; sourceMode = false }
    }
    private func candidate(_ action: (SubscriptionTemplateStore) -> Void) -> JSONValue {
        let editor = SubscriptionTemplateStore(); editor.document = document
        action(editor); return editor.document
    }
    private static func applyingConfiguration(_ config: JSONValue, to document: JSONValue) -> JSONValue {
        TemplateReferenceIndex.applying(config, to: document)
    }
    private func renameReference(_ identity: TemplateReferenceIndex.Identity, new: String, index: TemplateReferenceIndex) {
        guard !new.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, !new.hasPrefix("hx:"),
              !(index.format == "mihomo" && new.contains(",")),
              !index.exists(new, kind: identity.kind, in: common) else {
            error = L10n.text("名称无效、已存在或使用了保留前缀。"); return
        }
        var next = document
        next = Self.applyingConfiguration(index.renamed(common, identity: identity, new: new), to: next)
        if identity.kind == "outbound" {
            var injections = next["injections"].arrayValue ?? []
            for i in injections.indices where injections[i]["group"] == .string(identity.name) { injections[i]["group"] = .string(new) }
            if next.objectValue?["injections"] != nil { next["injections"] = .array(injections) }
        }
        pendingDeletion = nil; error = nil
        edit { $0 = next }
    }
    private func deleteOrPreview(_ candidate: JSONValue, identities: [TemplateReferenceIndex.Identity], presenterPath: [String]) {
        guard !identities.isEmpty else { pendingDeletion = nil; error = nil; edit { $0 = candidate }; return }
        let index = TemplateReferenceIndex(format: selected?.format ?? catalog["format"].stringValue ?? "mihomo", schema: schema)
        var next = candidate
        var changes: Set<String> = []
        var removed = Set(identities)
        do {
            let config = TemplateReferenceIndex.effective(next)
            let injectedGroups = Set((next["injections"].arrayValue ?? []).compactMap { $0["group"].stringValue })
            let result = index.cascading(config, removing: removed, injectedGroups: injectedGroups)
            removed.formUnion(result.removed)
            next = Self.applyingConfiguration(result.config, to: next)
            let original = TemplateReferenceIndex.effective(document)
            for path in result.paths {
                let address = TemplateReferenceIndex.originalPath(path, current: config, original: original)
                let item = original.at(address) ?? .null
                let name = item["name"].stringValue ?? item["tag"].stringValue
                changes.insert("common:" + Self.pointer(address) + (name.map { " · " + $0 } ?? ""))
            }
        }
        var injections = next["injections"].arrayValue ?? []
        for i in injections.indices.reversed() {
            guard let group = injections[i]["group"].stringValue,
                  removed.contains(.init(kind: "outbound", name: group)),
                  !index.exists(group, kind: "outbound", in: next["common"]) else { continue }
            changes.insert("/injections/\(i)"); injections.remove(at: i)
        }
        if next.objectValue?["injections"] != nil { next["injections"] = .array(injections) }
        error = nil
        if changes.isEmpty { pendingDeletion = nil; edit { $0 = candidate }; return }
        pendingDeletion = PendingDeletion(before: document, after: next, presenterPath: presenterPath,
            names: identities.map(\.name), changes: changes.sorted())
    }
    func confirmCascadeDeletion() {
        guard let pending = pendingDeletion else { return }
        pendingDeletion = nil
        guard document == pending.before else { error = L10n.text("配置已变化，请重新发起级联删除。"); return }
        error = nil; edit { $0 = pending.after }
    }
    private func permitsDeletion(candidate: JSONValue, names: [String], kind: String?) -> Bool {
        guard let kind, !names.isEmpty else { return true }
        let index = TemplateReferenceIndex(format: selected?.format ?? catalog["format"].stringValue ?? "mihomo", schema: schema)
        let references = index.blockingPaths(after: candidate, before: document, names: names, kind: kind)
        if !references.isEmpty { error = L10n.text("对象仍被引用，请先处理以下路径：") + "\n" + references.joined(separator: "\n"); return false }
        error = nil; return true
    }
    func deleteField(parent: [String], key: String) {
        let path = parent + [key]
        guard effective.at(path) != nil else { return }
        let kind = parent == ["rule-providers"] ? "rule_set" : (parent == ["sub-rules"] ? "sub_rule" : nil)
        let operation: (SubscriptionTemplateStore) -> Void = { editor in editor.setState("inherit", path: path, schema: .object([:])) }
        guard let kind else { operation(self); return }
        deleteOrPreview(candidate(operation), identities: [.init(kind: kind, name: key)], presenterPath: parent)
    }
    func deleteItem(path: [String], index: Int) {
        var values = effective.at(path)?.arrayValue ?? []; guard values.indices.contains(index) else { return }
        values.remove(at: index)
        let referenceIndex = TemplateReferenceIndex(format: selected?.format ?? catalog["format"].stringValue ?? "mihomo", schema: schema)
        let identities = referenceIndex.identities(at: path + [String(index)], in: effective)
        let next = candidate { $0.set(path, .array(values)) }
        deleteOrPreview(next, identities: identities, presenterPath: path)
    }
    func adoptRemoteRevision() {
        if let conflictTemplate, let remoteConflict { selected = conflictTemplate; baseline = remoteConflict }
        self.remoteConflict = nil; self.conflictTemplate = nil
    }
    func compare(_ api: APIClient, version: Int) async throws {
        try requireSession(api)
        guard let selected else { return }
        let historical = try await api.get(APIEndpoints.getSubscriptionTemplateVersion(id: selected.id, version: String(version)))
        try requireSession(api)
        try Self.requireSchema2(.object(historical.document))
        comparison = .object(historical.document)
        comparisonVersion = version
    }
    @discardableResult
    func duplicateItem(path: [String], index: Int) -> Int? {
        var values = effective.at(path)?.arrayValue ?? []
        guard values.indices.contains(index) else { return nil }
        var clone = values[index]
        if var object = clone.objectValue {
            let nameKey: String? = object["name"]?.stringValue != nil ? "name" : (object["tag"]?.stringValue != nil ? "tag" : nil)
            if let key = nameKey, let base = object[key]?.stringValue, !base.isEmpty {
                let existing = Set(values.compactMap { $0[key].stringValue })
                var candidate = "\(base)-copy"
                var suffix = 2
                while existing.contains(candidate) {
                    candidate = "\(base)-copy-\(suffix)"
                    suffix += 1
                }
                object[key] = .string(candidate)
                clone = .object(object)
            }
        }
        let nextIndex = index + 1
        values.insert(clone, at: nextIndex)
        set(path, .array(values))
        return nextIndex
    }
    @discardableResult
    func duplicateInjection(at index: Int) -> Int? {
        var rules = document["injections"].arrayValue ?? []
        guard rules.indices.contains(index) else { return nil }
        let nextIndex = index + 1
        rules.insert(rules[index], at: nextIndex)
        edit { $0["injections"] = .array(rules) }
        return nextIndex
    }
    func refactorCandidates(kind: String) -> [String] {
        let format = selected?.format ?? catalog["format"].stringValue ?? "mihomo"
        if kind == "rule_set" { return referenceNames("rule_set") }
        if format == "mihomo" {
            var seen: Set<String> = []
            return (effective["proxy-groups"].arrayValue?.compactMap { $0["name"].stringValue } ?? [])
                .filter { !$0.isEmpty && seen.insert($0).inserted }
        }
        return referenceNames("outbound")
    }
    private func ensureRenderCacheCurrent() {
        let generation = documentGeneration
        let language = L10n.language
        let _ = AppLanguage.shared.selection
        if renderCacheGeneration != generation || renderCacheLanguage != language {
            moduleLayoutCache.removeAll(keepingCapacity: true)
            fieldPresentationCache.removeAll(keepingCapacity: true)
            renderCacheGeneration = generation
            renderCacheLanguage = language
        }
    }

    func moduleLayout(for module: String) -> TemplateModuleLayoutSnapshot {
        ensureRenderCacheCurrent()
        if let cached = moduleLayoutCache[module] {
            return cached
        }
        let properties = rootProperties
        let currentSchema = schema
        let config = effective
        var resolvedRoots: [String: JSONValue] = [:]

        func resolveRoot(_ key: String) -> JSONValue {
            if let existing = resolvedRoots[key] { return existing }
            let val = config.at([key]) ?? .null
            let def = properties[key] ?? .null
            let resolved = currentSchema.resolve(currentSchema.editorDefinition(def, path: [key]), value: val)
            resolvedRoots[key] = resolved
            return resolved
        }

        func showsRootDetails(_ root: String) -> Bool {
            if root == "log" { return config.at([root, "disabled"])?.boolValue != true }
            if root == "ntp" {
                let children = resolveRoot(root)["properties"].objectValue ?? [:]
                let key = children["enabled"] != nil ? "enabled" : "enable"
                let value = config.at([root, key]) ?? initialValue(children[key] ?? .null, path: [root, key])
                return value.boolValue == true
            }
            return true
        }

        let moduleFields: [TemplateModuleField] = properties.keys.sorted()
            .filter { TemplateModule.destination($0) == module }
            .flatMap { key -> [TemplateModuleField] in
                let rawDef = properties[key]!
                let rootValue = config.at([key]) ?? .null
                let rootSchema = resolveRoot(key)
                let rootComplex = ["object", "array"].contains(rootSchema["type"].stringValue ?? rootValue.schemaType)
                guard TemplateFieldLayout.expandedObjects.contains(key),
                      rootSchema["type"].stringValue == "object",
                      rootSchema["x-unverified"].boolValue != true,
                      let children = rootSchema["properties"].objectValue, !children.isEmpty,
                      allowed([key], value: rootValue) else {
                    return [TemplateModuleField(path: [key], definition: rawDef, isComplex: rootComplex)]
                }
                let documentedVisible = Set(TemplateEditorGuidance.visibleKeys(rootSchema, path: [key], schema: currentSchema, value: config))
                let documentedKeys = TemplateEditorGuidance.ordered(Array(children.keys), path: [key], schema: currentSchema, value: config)
                var documentedOrder: [String: Int] = [:]
                documentedOrder.reserveCapacity(documentedKeys.count)
                for (idx, item) in documentedKeys.enumerated() where documentedOrder[item] == nil {
                    documentedOrder[item] = idx
                }
                let showDetails = showsRootDetails(key)
                let visibleKeys = documentedKeys.sorted {
                    if key == "dns", ($0 == "disable_cache") != ($1 == "disable_cache") {
                        return $0 == "disable_cache"
                    }
                    if key == "ntp" {
                        let leftSwitch = ["enabled", "enable"].contains($0)
                        let rightSwitch = ["enabled", "enable"].contains($1)
                        if leftSwitch != rightSwitch { return leftSwitch }
                    }
                    return (documentedOrder[$0] ?? documentedKeys.count) < (documentedOrder[$1] ?? documentedKeys.count)
                }.filter {
                    documentedVisible.contains($0) && (showDetails || (key == "log" && $0 == "disabled") || (key == "ntp" && ["enabled", "enable"].contains($0)))
                }
                return visibleKeys.map { childKey in
                    let childPath = [key, childKey]
                    let childDef = children[childKey]!
                    let childVal = config.at(childPath) ?? .null
                    let childSchema = currentSchema.resolve(currentSchema.editorDefinition(childDef, path: childPath), value: childVal)
                    let complex = ["object", "array"].contains(childSchema["type"].stringValue ?? childVal.schemaType)
                    return TemplateModuleField(path: childPath, definition: childDef, isComplex: complex)
                }
            }

        let visibleFields = moduleFields.filter { field in
            if field.path == ["route", "default_interface"], config["route"]["auto_detect_interface"].boolValue == true { return false }
            guard field.path.first == "dns" else { return true }
            let key = field.path.last ?? ""
            if config.at(["dns", "disable_cache"])?.boolValue == true,
               key != "disable_cache",
               TemplateFieldLayout.section(path: field.path, complex: field.isComplex) == "缓存设置" { return false }
            if key == "optimistic", config.at(["dns", "disable_expire"])?.boolValue == true { return false }
            return true
        }

        let rootFields = Dictionary(grouping: visibleFields.filter { $0.path.count == 2 }) { $0.path[0] }
        let dialerKeys = rootFields.mapValues { TemplateFieldLayout.dialerFields(in: $0.compactMap { $0.path.last }) }
        let groups = Dictionary(grouping: visibleFields) { field in
            if field.path.count == 2, let root = field.path.first, dialerKeys[root]?.contains(field.path[1]) == true { return "dialer:" + root }
            return TemplateFieldLayout.section(path: field.path, complex: field.isComplex)
        }
        func orderingTitle(_ group: String) -> String {
            if group.hasPrefix("dialer:") { return TemplateFieldLayout.section(path: [String(group.dropFirst(7)), "detour"], complex: false) }
            return group
        }
        let sections: [TemplateModuleSection] = groups.keys
            .sorted { TemplateFieldLayout.precedes(orderingTitle($0), orderingTitle($1), module: module) }
            .map { group in
                if group.hasPrefix("dialer:") {
                    let root = String(group.dropFirst(7))
                    let keys = dialerKeys[root] ?? []
                    var keyOrder: [String: Int] = [:]
                    for (idx, item) in keys.enumerated() where keyOrder[item] == nil { keyOrder[item] = idx }
                    let fields = (groups[group] ?? []).sorted {
                        (keyOrder[$0.path.last ?? ""] ?? keys.count) < (keyOrder[$1.path.last ?? ""] ?? keys.count)
                    }
                    return TemplateModuleSection(title: TemplateFieldLayout.section(path: [root], complex: true), fields: fields, dialerPath: [root])
                }
                return TemplateModuleSection(title: group, fields: groups[group] ?? [])
            }

        let roots = Set(moduleFields.filter { $0.path.count > 1 }.compactMap { $0.path.first })
        let unknownNested = roots.sorted().flatMap { root -> [String] in
            guard showsRootDetails(root) else { return [] }
            let known = resolveRoot(root)["properties"].objectValue ?? [:]
            return (config[root].objectValue ?? [:]).keys.filter { known[$0] == nil }.sorted().map {
                Self.pointer([root, $0])
            }
        }
        let snapshot = TemplateModuleLayoutSnapshot(sections: sections, unknownNestedFields: unknownNested)
        moduleLayoutCache[module] = snapshot
        return snapshot
    }

    func fieldPresentation(path: [String], definition: JSONValue) -> TemplateFieldPresentation {
        ensureRenderCacheCurrent()
        if let cached = fieldPresentationCache[path], cached.sourceDefinition == definition {
            return cached
        }
        let currentSchema = schema
        let config = effective
        let isSingbox = TemplateEditorGuidance.isSingbox(currentSchema)
        let editingDef = currentSchema.editorDefinition(definition, path: path)
        let val = editorValue(editingDef, path: path)
        let resolvedSchema = currentSchema.resolve(editingDef, value: val)
        let variants = currentSchema.variants(editingDef)
        let scalarListDef = currentSchema.scalarListItemDefinition(editingDef)
        let choices = resolvedSchema["enum"].arrayValue ?? resolvedSchema["x-enum-choices"].arrayValue
        let enumOpts = (choices?.isEmpty == false)
            ? TemplateEditorGuidance.enumPickerOptions(resolvedSchema, path: path, schema: currentSchema, value: config)
            : nil
        let required = path.isEmpty ? false : currentSchema.isRequired(path, value: config)
        let guidance = path.isEmpty ? nil : TemplateEditorGuidance.help(path, schema: currentSchema, value: config)
        let rawTitle = path.last ?? ""
        let help = resolvedSchema["description"].stringValue ?? guidance ?? rawTitle
        let explicit = config.at(path)
        let isExplicit = explicit != nil
        let hasPreview = !isSingbox
            || explicit != nil
            || resolvedSchema.objectValue?["default"] != nil
            || TemplateEditorGuidance.defaultValue(path, schema: currentSchema, value: config) != nil
        let supported = supportedPlatforms(path, value: val)
        let refKind = TemplateSchema.referenceKind(resolvedSchema, path: path)
        let presentation = TemplateFieldPresentation(
            sourceDefinition: definition,
            editingDefinition: editingDef,
            value: val,
            schema: resolvedSchema,
            variants: variants,
            scalarListItemDefinition: scalarListDef,
            enumChoices: choices,
            enumOptions: enumOpts,
            isRequired: required,
            guidance: guidance,
            helpText: help,
            hasPreviewValue: hasPreview,
            platforms: supported,
            allowed: !supported.isEmpty,
            isExplicitlySet: isExplicit,
            referenceKind: refKind,
            isSingbox: isSingbox
        )
        fieldPresentationCache[path] = presentation
        return presentation
    }

    private func rebuildCatalogCaches() {
        let schemaRoot = catalog["schema"]
        let nextSchema = TemplateSchema(root: schemaRoot)
        cachedSchema = nextSchema
        rootProperties = nextSchema.resolve(schemaRoot, value: effective)["properties"].objectValue ?? [:]
        compiledPlatformRules = (catalog["platform_rules"].arrayValue ?? []).map { rule in
            let pattern = Self.path(rule["path"].stringValue ?? "")
            let requiredValue = rule.objectValue?["value"]
            let allowed = Set(rule["platforms"].arrayValue?.compactMap(\.stringValue) ?? [])
            return CompiledPlatformRule(pattern: pattern, requiredValue: requiredValue, allowed: allowed)
        }
        pathRulesCache.removeAll(keepingCapacity: true)
        documentGeneration = UUID()
        rebuildSearchableFields(schema: nextSchema)
    }
    private func rebuildSearchableFields(schema nextSchema: TemplateSchema) {
        var fields: [TemplateSearchableField] = []
        for key in rootProperties.keys.sorted() {
            guard let def = rootProperties[key] else { continue }
            let resolved = nextSchema.resolve(nextSchema.editorDefinition(def, path: [key]), value: .null)
            let module = TemplateModule.destination(key)
            if TemplateFieldLayout.expandedObjects.contains(key),
               resolved["type"].stringValue == "object",
               resolved["x-unverified"].boolValue != true,
               let children = resolved["properties"].objectValue,
               !children.isEmpty {
                for childKey in children.keys.sorted() {
                    guard let childDef = children[childKey] else { continue }
                    let childResolved = nextSchema.resolve(nextSchema.editorDefinition(childDef, path: [key, childKey]), value: .null)
                    let path = [key, childKey]
                    let complex = ["object", "array"].contains(childResolved["type"].stringValue ?? "")
                    let section = TemplateFieldLayout.section(path: path, complex: complex)
                    let title = childResolved["title"].stringValue ?? childKey
                    let desc = childResolved["description"].stringValue ?? ""
                    let keywords = [
                        key, childKey, "\(key).\(childKey)", title,
                        section, L10n.text(section), module, L10n.text(module), desc
                    ].filter { !$0.isEmpty }.joined(separator: " ").lowercased()
                    fields.append(TemplateSearchableField(
                        path: path,
                        pointer: Self.pointer(path),
                        title: title,
                        module: module,
                        section: section,
                        keywords: keywords
                    ))
                }
            } else {
                let path = [key]
                let complex = ["object", "array"].contains(resolved["type"].stringValue ?? "")
                let section = TemplateFieldLayout.section(path: path, complex: complex)
                let title = resolved["title"].stringValue ?? key
                let desc = resolved["description"].stringValue ?? ""
                let keywords = [
                    key, title, section, L10n.text(section), module, L10n.text(module), desc
                ].filter { !$0.isEmpty }.joined(separator: " ").lowercased()
                fields.append(TemplateSearchableField(
                    path: path,
                    pointer: Self.pointer(path),
                    title: title,
                    module: module,
                    section: section,
                    keywords: keywords
                ))
            }
        }
        searchableFields = fields.sorted {
            if $0.module != $1.module {
                let lhs = TemplateModule.all.firstIndex(of: $0.module) ?? TemplateModule.all.count
                let rhs = TemplateModule.all.firstIndex(of: $1.module) ?? TemplateModule.all.count
                if lhs != rhs { return lhs < rhs }
            }
            return $0.pointer < $1.pointer
        }
    }
    private func rebuildDiagnosticSummaries() {
        var errors = 0
        var warnings = 0
        var byModule: [String: ModuleDiagnosticSummary] = [:]
        var byPointer: [String: [JSONValue]] = [:]
        for issue in diagnostics {
            let severity = issue["severity"].stringValue ?? "warning"
            let isError = severity == "error"
            if isError { errors += 1 } else { warnings += 1 }
            let rawPath = issue["path"].stringValue ?? ""
            let normalizedPath: String
            if rawPath == "/common" { normalizedPath = "" }
            else if rawPath.hasPrefix("/common/") { normalizedPath = String(rawPath.dropFirst(7)) }
            else { normalizedPath = rawPath }
            let components = normalizedPath.isEmpty ? [] : Self.path(normalizedPath)
            let module: String
            if components.first == "injections" { module = "节点注入" }
            else if let root = components.first, !root.isEmpty { module = TemplateModule.destination(root) }
            else { module = "高级配置" }
            var summary = byModule[module, default: ModuleDiagnosticSummary()]
            if isError { summary.errors += 1 } else { summary.warnings += 1 }
            byModule[module] = summary
            if !normalizedPath.isEmpty {
                byPointer[normalizedPath, default: []].append(issue)
            }
            if !rawPath.isEmpty, rawPath != normalizedPath {
                byPointer[rawPath, default: []].append(issue)
            }
        }
        errorCount = errors
        warningCount = warnings
        moduleDiagnostics = byModule
        diagnosticsByPointer = byPointer
    }
    func referenceNames(_ kind: String) -> [String] {
        func choices(_ names: [String]) -> [String] {
            var seen: Set<String> = []
            return names.filter { !$0.isEmpty && seen.insert($0).inserted }
        }
        let config = effective
        let paths: [String: [String]] = ["outbound": ["outbounds"], "inbound": ["inbounds"], "dns_server": ["dns", "servers"], "rule_set": ["route", "rule_set"], "network_namespace": ["network_namespaces"], "http_client": ["http_clients"], "certificate_provider": ["certificate_providers"]]
        if selected?.format == "mihomo" {
            if kind == "outbound" { return choices(["DIRECT", "REJECT", "REJECT-DROP", "PASS", "COMPATIBLE"] + (config["proxy-groups"].arrayValue?.compactMap { $0["name"].stringValue } ?? [])) }
            if kind == "rule_set" { return choices((config["rule-providers"].objectValue ?? [:]).keys.sorted()) }
            if kind == "sub_rule" { return choices((config["sub-rules"].objectValue ?? [:]).keys.sorted()) }
        }
        guard let path = paths[kind] else { return [] }
        return choices((config.at(path)?.arrayValue ?? []).flatMap { item in
            item["tag"].stringValue.map { [$0] } ?? item["tag"].arrayValue?.compactMap(\.stringValue) ?? []
        }.sorted())
    }
    func focus(_ diagnostic: JSONValue, api: APIClient) async throws {
        try await syncSource(api)
        focusedPath = diagnostic["path"].stringValue ?? ""
        sourceMode = false
    }
    static func difference(_ before: JSONValue, _ after: JSONValue, path: String = "") -> JSONValue {
        if before == after { return .array([]) }
        if let left = before.objectValue, let right = after.objectValue {
            return .array(Set(left.keys).union(right.keys).sorted().flatMap { key -> [JSONValue] in
                let nextPath = path + pointer([key])
                guard let old = left[key] else { return [.object(["path": .string(nextPath), "operation": .string("add"), "after": right[key]!])] }
                guard let new = right[key] else { return [.object(["path": .string(nextPath), "operation": .string("remove"), "before": old])] }
                return difference(old, new, path: nextPath).arrayValue ?? []
            })
        }
        return .array([.object(["path": .string(path), "operation": .string("replace"), "before": before, "after": after])])
    }
    func record(_ failure: Error) {
        if failure is CancellationError { return }
        let message = failure.localizedDescription
        if let parsed = try? JSONDecoder().decode([JSONValue].self, from: Data(message.utf8)), parsed.allSatisfy({ $0["path"].stringValue != nil }) {
            diagnostics = parsed; hasValidated = true; error = L10n.text("配置未通过校验，请查看诊断。")
        } else { error = message }
    }
    func run(_ action: @escaping @MainActor () async throws -> Void) {
        guard !busy else { return }; busy = true; error = nil
        Task { defer { busy = false }; do { try await action() } catch { if !self.closed { self.record(error) } } }
    }
}
extension SubscriptionTemplatesPage: APIPage {}
