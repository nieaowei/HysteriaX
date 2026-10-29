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
    var previewGeneration = UUID()
    var previewUserID = "" { didSet { if oldValue != previewUserID { invalidatePreview() } } }
    var previewPlatform = "" { didSet { if oldValue != previewPlatform { invalidatePreview() } } }
    var previewKernelVersion = "" { didSet { if oldValue != previewKernelVersion { invalidatePreview() } } }
    var previewReveal = false { didSet { if oldValue != previewReveal { invalidatePreview() } } }
    var previewTab = "最终配置"
    var saving = false
    var serviceName = ""
    var previewStale = true
    func invalidatePreview() { previewGeneration = UUID(); previewStale = true; preview = [:] }
    var templates: [SubscriptionTemplate] = []
    var selected: SubscriptionTemplate?
    var document: JSONValue = .object([:]) { didSet { if document != oldValue { documentGeneration = UUID(); invalidatePreview() } } }
    var baseline: JSONValue = .object([:])
    var catalog: JSONValue = .object([:])
    var source = "" { didSet { if source != oldValue { documentGeneration = UUID(); invalidatePreview() } } }
    var sourceBaseline = ""
    var sourceMode = false
    var diagnostics: [JSONValue] = []
    var preview: [String: JSONValue] = [:]
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
    var busy = false
    var undoStack: [JSONValue] = []
    var redoStack: [JSONValue] = []
    var pendingImport: [String: JSONValue]?
    var focusGeneration = UUID()
    var focusedPath = "" { didSet { focusGeneration = UUID() } }
    var dirty: Bool { document != baseline || (sourceMode && source != sourceBaseline) }
    var editorWindowTitle: String {
        let status = dirty ? L10n.text("未保存") : L10n.text("草稿已保存")
        return "\(selected?.name ?? "") · \(status) · \(selected?.format ?? "") · \(serviceName)"
    }
    var schema: TemplateSchema { TemplateSchema(root: catalog["schema"]) }
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
    func supportedPlatforms(_ path: [String], value: JSONValue? = nil) -> [String] {
        var platforms = TemplatePlatform.all
        let rules = catalog["platform_rules"].arrayValue ?? []
        for rule in rules {
            let pattern = Self.path(rule["path"].stringValue ?? "")
            func matches(_ pattern: ArraySlice<String>, _ path: ArraySlice<String>) -> Bool {
                guard let first = pattern.first else { return path.isEmpty }
                if first == "**" { return matches(pattern.dropFirst(), path) || (!path.isEmpty && matches(pattern, path.dropFirst())) }
                return !path.isEmpty && (first == "*" || first == path.first) && matches(pattern.dropFirst(), path.dropFirst())
            }
            guard matches(pattern[...], path[...]) else { continue }
            if let required = rule.objectValue?["value"], value != required { continue }
            let allowed = rule["platforms"].arrayValue?.compactMap(\.stringValue) ?? []
            platforms = platforms.filter { allowed.contains($0) }
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
        undoStack = []; redoStack = []; diagnostics = []; preview = [:]; remoteConflict = nil; sourceMode = false
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
    }
    func preparePublish(_ api: APIClient) async throws {
        try requireSession(api)
        guard remoteConflict == nil else { throw APIClientError.server("请先处理服务端修订冲突。") }
        try await validate(api)
        if diagnostics.contains(where: { $0["severity"].stringValue == "error" }) { throw APIClientError.server("配置未通过校验，请查看诊断。") }
        if let version = selected?.publishedVersion { try await compare(api, version: version) } else { comparison = .object([:]) }
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
    func render(_ api: APIClient, user: String, platform: String, version: String, reveal: Bool) async throws {
        try requireSession(api)
        try await syncSource(api); guard let selected else { return }
        let request = UUID(); previewGeneration = request; let generation = documentGeneration
        let rendered = try await api.post(APIEndpoints.previewSubscriptionTemplate(id: selected.id), body: PreviewSubscriptionTemplateRequest(userId: user, platform: platform, kernelVersion: version.isEmpty ? nil : version, revealSecrets: reveal, document: document.objectValue))
        try requireSession(api)
        guard previewGeneration == request, documentGeneration == generation else { return }
        preview = rendered; previewStale = false
        diagnostics = preview["diagnostics"]?.arrayValue ?? []
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
            edit { $0 = next }; diagnostics = pendingImport?["diagnostics"]?.arrayValue ?? []
            pendingImport = nil; sourceMode = false
        } catch { self.error = error.localizedDescription }
    }
    func refactor(_ api: APIClient, kind: String, old: String, new: String) async throws {
        try requireSession(api)
        try await syncSource(api); guard let selected else { return }
        let response = try await api.post(APIEndpoints.refactorSubscriptionTemplate(id: selected.id), body: RefactorSubscriptionTemplateRequest(expectedRevision: selected.revision, document: document.objectValue ?? [:], kind: kind, oldName: old, newName: new))
        try requireSession(api)
        if let next = response["document"] { try Self.requireSchema2(next); edit { $0 = next }; diagnostics = response["diagnostics"]?.arrayValue ?? []; sourceMode = false }
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
            diagnostics = parsed; error = L10n.text("配置未通过校验，请查看诊断。")
        } else { error = message }
    }
    func run(_ action: @escaping @MainActor () async throws -> Void) {
        guard !busy else { return }; busy = true; error = nil
        Task { defer { busy = false }; do { try await action() } catch { if !self.closed { self.record(error) } } }
    }
}
extension SubscriptionTemplatesPage: APIPage {}
