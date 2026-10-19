import Foundation
import Observation

@MainActor @Observable
final class SubscriptionTemplateStore {
    @ObservationIgnored var connectedAPI: APIClient?
    var templates: [SubscriptionTemplate] = []
    var selected: SubscriptionTemplate?
    var document: JSONValue = .object([:])
    var baseline: JSONValue = .object([:])
    var catalog: JSONValue = .object([:])
    var layer = "common"
    var source = ""
    var sourceBaseline = ""
    var sourceMode = false
    var diagnostics: [JSONValue] = []
    var preview: [String: JSONValue] = [:]
    var history: [JSONValue] = []
    var error: String?
    var remoteConflict: JSONValue?
    var conflictTemplate: SubscriptionTemplate?
    var comparison: JSONValue?
    var busy = false
    var undoStack: [JSONValue] = []
    var redoStack: [JSONValue] = []
    var pendingImport: [String: JSONValue]?
    var search = ""
    var focusedPath = ""
    var dirty: Bool { document != baseline || (sourceMode && source != sourceBaseline) }
    var schema: TemplateSchema { TemplateSchema(root: catalog["schema"]) }
    var common: JSONValue { document["common"] }
    var overlay: JSONValue { document["platforms"][layer]["set"] }
    var removals: [String] { document["platforms"][layer]["remove"].arrayValue?.compactMap(\.stringValue) ?? [] }
    var effective: JSONValue { TemplateReferenceIndex.effective(document, platform: layer) }
    static func path(_ pointer: String) -> [String] { pointer.dropFirst().components(separatedBy: "/").map { $0.replacingOccurrences(of: "~1", with: "/").replacingOccurrences(of: "~0", with: "~") } }
    static func pointer(_ path: [String]) -> String { "/" + path.map { $0.replacingOccurrences(of: "~", with: "~0").replacingOccurrences(of: "/", with: "~1") }.joined(separator: "/") }
    func edit(_ body: (inout JSONValue) -> Void) {
        let before = document; var next = document; body(&next); document = next
        if document != before { undoStack.append(before); if undoStack.count > 100 { undoStack.removeFirst() }; redoStack.removeAll(); preview = [:] }
    }
    func undo() { if let previous = undoStack.popLast() { redoStack.append(document); document = previous } }
    func redo() { if let next = redoStack.popLast() { undoStack.append(document); document = next } }
    func state(_ path: [String]) -> String {
        if layer == "common" { return common.at(path) == nil ? "inherit" : "set" }
        let pointer = Self.pointer(path)
        if removals.contains(where: { pointer == $0 || pointer.hasPrefix($0 + "/") }) { return "remove" }
        return overlay.at(path) == nil ? "inherit" : "set"
    }
    func setState(_ state: String, path: [String], schema: JSONValue) {
        if state == "set" { set(path, effective.at(path) ?? initialValue(schema, path: path)); return }
        for index in path.indices where effective.at(Array(path.prefix(index)))?.arrayValue != nil {
            let parent = Array(path.prefix(index)); var array = effective.at(parent) ?? .array([])
            array.set(Array(path.dropFirst(index)), nil); set(parent, array); return
        }
        edit { doc in
            if layer == "common" { doc.set(["common"] + path, nil); return }
            doc.set(["platforms", layer, "set"] + path, nil)
            var removes = removals.filter { $0 != Self.pointer(path) }
            if state == "remove" { removes.append(Self.pointer(path)) }
            doc.set(["platforms", layer, "remove"], .array(removes.map(JSONValue.string)))
        }
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
    func restoreDefault(path: [String], definition: JSONValue) {
        if let value = schema.resolve(definition, value: effective.at(path) ?? .null).objectValue?["default"] { set(path, portableInitial(value, path: path)) }
        else { setState(layer == "common" ? "inherit" : "remove", path: path, schema: definition) }
    }
    func set(_ path: [String], _ value: JSONValue) {
        // Editing an inherited array creates an explicit complete array override.
        var actualPath = path
        var actualValue = value
        if layer != "common" {
            for index in path.indices where effective.at(Array(path.prefix(index)))?.arrayValue != nil {
                actualPath = Array(path.prefix(index)); actualValue = effective.at(actualPath) ?? .array([])
                actualValue.set(Array(path.dropFirst(index)), value); break
            }
        }
        edit { doc in
            if layer == "common" { doc.set(["common"] + actualPath, actualValue) }
            else {
                doc.set(["platforms", layer, "set"] + actualPath, actualValue)
                let pointer = Self.pointer(actualPath)
                doc.set(["platforms", layer, "remove"], .array(removals.filter { $0 != pointer && !$0.hasPrefix(pointer + "/") && !pointer.hasPrefix($0 + "/") }.map(JSONValue.string)))
            }
        }
    }
    func allowed(_ path: [String], value: JSONValue? = nil) -> Bool {
        let platforms = layer == "common" ? TemplatePlatform.all : [layer]
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
            if !platforms.allSatisfy({ allowed.contains($0) }) { return false }
        }
        return true
    }
    private func requireSession(_ api: APIClient) throws {
        if let connectedAPI, connectedAPI !== api { throw APIClientError.server(L10n.text("服务连接已变化，本地修改已保留；请刷新模板列表后再提交。")) }
    }
    func loadList(_ api: APIClient) async throws {
        connectedAPI = api
        templates = try await api.collectPages { page in APIEndpoints.listSubscriptionTemplates(page: page, pageSize: 200) }
    }
    func open(_ template: SubscriptionTemplate, api: APIClient) async throws {
        try requireSession(api)
        let draft = try await api.get(APIEndpoints.getSubscriptionTemplateDraft(id: template.id))
        let fields = try await api.get(APIEndpoints.subscriptionTemplateFields(format: template.format))
        selected = draft.template; document = .object(draft.document); baseline = document; catalog = .object(fields)
        undoStack = []; redoStack = []; diagnostics = []; preview = [:]; remoteConflict = nil; sourceMode = false; layer = "common"
        try await loadHistory(api)
    }
    func syncSource(_ api: APIClient) async throws {
        try requireSession(api)
        guard let selected else { return }
        if sourceMode {
            let response = try await api.post(APIEndpoints.subscriptionTemplateSource, body: SubscriptionTemplateSourceRequest(format: selected.format, source: source))
            if let config = response["config"] { edit { doc in doc.set(layer == "common" ? ["common"] : ["platforms", layer, "set"], config) } }
        }
    }
    func switchMode(_ api: APIClient) async throws {
        try requireSession(api)
        guard let selected else { return }
        if sourceMode { try await syncSource(api); sourceMode = false }
        else {
            let response = try await api.post(APIEndpoints.subscriptionTemplateSource, body: SubscriptionTemplateSourceRequest(format: selected.format, config: (layer == "common" ? common : overlay).objectValue ?? [:]))
            source = response["source"]?.stringValue ?? ""; sourceBaseline = source; sourceMode = true
        }
    }
    func save(_ api: APIClient) async throws {
        try requireSession(api)
        try await syncSource(api)
        guard let selected else { return }
        do {
            self.selected = try await api.put(APIEndpoints.saveSubscriptionTemplateDraft(id: selected.id), body: SaveSubscriptionTemplateDraftRequest(expectedRevision: selected.revision, document: document.objectValue ?? [:]))
            baseline = document; sourceBaseline = source; remoteConflict = nil; try await loadList(api)
        } catch {
            // Fetch a comparison copy without overwriting the user's document or revision.
            if let remote = try? await api.get(APIEndpoints.getSubscriptionTemplateDraft(id: selected.id)) { remoteConflict = .object(remote.document); conflictTemplate = remote.template }
            throw error
        }
    }
    func validate(_ api: APIClient) async throws {
        try requireSession(api)
        try await syncSource(api); guard let selected else { return }
        let response = try await api.post(APIEndpoints.validateSubscriptionTemplate(id: selected.id), body: ValidateSubscriptionTemplateRequest(document: document.objectValue))
        diagnostics = response["diagnostics"]?.arrayValue ?? []
    }
    func publish(_ api: APIClient) async throws {
        try requireSession(api)
        if dirty { try await save(api) }; guard let selected else { return }
        self.selected = try await api.post(APIEndpoints.publishSubscriptionTemplate(id: selected.id), body: PublishSubscriptionTemplateRequest(expectedRevision: selected.revision))
        try await loadHistory(api); try await loadList(api)
    }
    func loadHistory(_ api: APIClient) async throws {
        try requireSession(api)
        guard let selected else { return }; history = []; var page = 1
        while true {
            let response = try await api.get(APIEndpoints.listSubscriptionTemplateVersions(id: selected.id, page: page, pageSize: 200))
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
        self.selected = try await api.patch(APIEndpoints.updateSubscriptionTemplate(id: selected.id), body: UpdateSubscriptionTemplateRequest(expectedRevision: selected.revision, name: name, isDefault: makeDefault ? true : nil, archived: archive ? true : nil, replacementId: replacement?.id, replacementRevision: replacement?.revision))
        try await loadList(api)
        if archive { self.selected = nil }
    }
    func rollback(_ api: APIClient, version: Int) async throws {
        try requireSession(api)
        guard let selected else { return }
        let next = try await api.post(APIEndpoints.rollbackSubscriptionTemplate(id: selected.id), body: RollbackSubscriptionTemplateRequest(expectedRevision: selected.revision, version: version))
        try await open(next, api: api); try await loadList(api)
    }
    func render(_ api: APIClient, user: String, platform: String, version: String, reveal: Bool) async throws {
        try requireSession(api)
        try await syncSource(api); guard let selected else { return }
        preview = try await api.post(APIEndpoints.previewSubscriptionTemplate(id: selected.id), body: PreviewSubscriptionTemplateRequest(userId: user, platform: platform, kernelVersion: version.isEmpty ? nil : version, revealSecrets: reveal, document: document.objectValue))
        diagnostics = preview["diagnostics"]?.arrayValue ?? []
    }
    func analyzeImport(_ api: APIClient, source: String) async throws {
        try requireSession(api)
        guard let selected else { return }
        pendingImport = try await api.post(APIEndpoints.importSubscriptionTemplate, body: ImportSubscriptionTemplateRequest(format: selected.format, source: source))
    }
    func acceptImport() {
        if let next = pendingImport?["document"] { edit { $0 = next }; diagnostics = pendingImport?["diagnostics"]?.arrayValue ?? [] }
        pendingImport = nil; sourceMode = false
    }
    func refactor(_ api: APIClient, kind: String, old: String, new: String) async throws {
        try requireSession(api)
        try await syncSource(api); guard let selected else { return }
        let response = try await api.post(APIEndpoints.refactorSubscriptionTemplate(id: selected.id), body: RefactorSubscriptionTemplateRequest(expectedRevision: selected.revision, document: document.objectValue ?? [:], kind: kind, oldName: old, newName: new))
        if let next = response["document"] { edit { $0 = next }; diagnostics = response["diagnostics"]?.arrayValue ?? []; sourceMode = false }
    }
    private func candidate(_ action: (SubscriptionTemplateStore) -> Void) -> JSONValue {
        let editor = SubscriptionTemplateStore(); editor.document = document; editor.layer = layer
        action(editor); return editor.document
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
        let kind = parent == ["rule-providers"] ? "rule_set" : (parent == ["sub-rules"] ? "sub_rule" : nil)
        let operation: (SubscriptionTemplateStore) -> Void = { editor in editor.setState(editor.layer == "common" ? "inherit" : "remove", path: path, schema: .object([:])) }
        guard permitsDeletion(candidate: candidate(operation), names: [key], kind: kind) else { return }
        operation(self)
    }
    func deleteItem(path: [String], index: Int) {
        var values = effective.at(path)?.arrayValue ?? []; guard values.indices.contains(index) else { return }
        let removed = values.remove(at: index)
        let locations: [String: String] = ["proxy-groups":"outbound", "outbounds":"outbound", "inbounds":"inbound", "servers":"dns_server", "rule_set":"rule_set", "http_clients":"http_client", "certificate_providers":"certificate_provider", "network_namespaces":"network_namespace"]
        let names = [removed["name"].stringValue ?? removed["tag"].stringValue].compactMap { $0 } + (removed["tag"].arrayValue?.compactMap(\.stringValue) ?? [])
        let next = candidate { $0.set(path, .array(values)) }
        guard permitsDeletion(candidate: next, names: names, kind: locations[path.last ?? ""]) else { return }
        set(path, .array(values))
    }
    func adoptRemoteRevision() {
        if let conflictTemplate, let remoteConflict { selected = conflictTemplate; baseline = remoteConflict }
        self.remoteConflict = nil; self.conflictTemplate = nil
    }
    func compare(_ api: APIClient, version: Int) async throws {
        try requireSession(api)
        guard let selected else { return }
        comparison = .object(try await api.get(APIEndpoints.getSubscriptionTemplateVersion(id: selected.id, version: String(version))).document)
    }
    func referenceNames(_ kind: String) -> [String] {
        let config = effective
        let paths: [String: [String]] = ["outbound": ["outbounds"], "inbound": ["inbounds"], "dns_server": ["dns", "servers"], "rule_set": ["route", "rule_set"], "network_namespace": ["network_namespaces"], "http_client": ["http_clients"], "certificate_provider": ["certificate_providers"]]
        if selected?.format == "mihomo" {
            if kind == "outbound" { return ["DIRECT", "REJECT", "REJECT-DROP", "PASS", "COMPATIBLE"] + (config["proxy-groups"].arrayValue?.compactMap { $0["name"].stringValue } ?? []) }
            if kind == "rule_set" { return (config["rule-providers"].objectValue ?? [:]).keys.sorted() }
            if kind == "sub_rule" { return (config["sub-rules"].objectValue ?? [:]).keys.sorted() }
        }
        guard let path = paths[kind] else { return [] }
        return (config.at(path)?.arrayValue ?? []).flatMap { item in
            item["tag"].stringValue.map { [$0] } ?? item["tag"].arrayValue?.compactMap(\.stringValue) ?? []
        }.sorted()
    }
    func focus(_ diagnostic: JSONValue, api: APIClient) async throws {
        try await syncSource(api)
        let target = diagnostic["layer"].stringValue ?? "common"
        if target == "common" || TemplatePlatform.all.contains(target) { layer = target }
        focusedPath = diagnostic["path"].stringValue ?? ""
        search = focusedPath.hasPrefix("/injections/") ? "" : (Self.path(focusedPath).last(where: { Int($0) == nil }) ?? "")
        sourceMode = false
    }
    func record(_ failure: Error) {
        let message = failure.localizedDescription
        if let parsed = try? JSONDecoder().decode([JSONValue].self, from: Data(message.utf8)), parsed.allSatisfy({ $0["path"].stringValue != nil }) {
            diagnostics = parsed; error = L10n.text("配置未通过校验，请查看诊断。")
        } else { error = message }
    }
    func run(_ action: @escaping @MainActor () async throws -> Void) {
        guard !busy else { return }; busy = true; error = nil
        Task { defer { busy = false }; do { try await action() } catch { self.record(error) } }
    }
}
extension SubscriptionTemplatesPage: APIPage {}
