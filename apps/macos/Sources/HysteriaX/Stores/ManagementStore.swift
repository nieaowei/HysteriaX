import Foundation
import Observation

private struct ClientDisplaySnapshot: Codable {
    let credentials: [CredentialSummary]?
    let nodes: [NodeSummary]
    let users: [UserSummary]
    let jobs: [JobSummary]
    let auditRecords: [AuditSummary]
    let updatedAt: Date
    let serverMonitoring: ServerMonitoring?
    let overview: OverviewResponse?
    var clientSummary: ClientSummary? = nil
}

@MainActor
@Observable
final class ManagementStore {
    var templateWindows = TemplateEditorWindows()
    private var subscriptionTemplateLists: [String: SubscriptionTemplateListStore] = [:]
    func subscriptionTemplateList() -> SubscriptionTemplateListStore {
        let service = TemplateEditorID(service: serviceAddress, template: "").service
        if let list = subscriptionTemplateLists[service] { return list }
        let list = SubscriptionTemplateListStore()
        list.sessionAvailable = { [weak self] api in
            guard let self else { return false }
            return TemplateEditorID(service: self.serviceAddress, template: "").service == service && (try? self.requireConnectedAPI()) === api
        }
        subscriptionTemplateLists[service] = list
        return list
    }
    var serviceAddress = UserDefaults.standard.string(forKey: "serviceAddress") ?? ""
    var requestedSection: String?
    var supportsDNSManagement = false
    var dnsConnections: [DNSConnection] = []
    var dnsZones: [DNSZone] = []
    var dnsRecords: [DNSRecord] = []
    var dnsUpdatedAt: Date?
    var dnsError: String?
    var dnsIsLoading = false
    var dnsRequestGeneration = UUID()
    var requestedDNSRecordID: String?
    var credentials: [CredentialSummary] = []
    var nodes: [NodeSummary] = []
    var users: [UserSummary] = []
    var authorizationGroups: [AuthorizationGroupSummary] = []
    var authorizationGroupsError: String?
    var isLoadingAuthorizationGroups = false
    var authorizationGroupsRequestID = UUID()
    var jobs: [JobSummary] = []
    var auditRecords: [AuditSummary] = []
    var adminTokens: [AdminTokenSummary] = []
    var currentAdminTokenID: String?
    var serverMonitoring: ServerMonitoring?
    var serverMonitoringError: String?
    var overviewMonitoringPage: OverviewNodesPage?
    var overviewMonitoringPageKey: String?
    var clientSummary: ClientSummary?
    var supportsClientSummary = false
    var supportsSSHDNSSync = false
    enum Directory: String, Hashable { case nodes, users, credentials }
    private var loadedDirectories: Set<Directory> = []
    private var directoryTasks: [Directory: Task<Void, Never>] = [:]
    var loadingDirectories: Set<Directory> = []
    var overview: OverviewResponse?
    var overviewError: String?
    var overviewHistoryRefreshToken = UUID()
    var supportsOverviewMonitoring = false
    var supportsNodeRecordRemoval = false
    var supportsJobRetryLinks = false
    private let nodeNotifications: NodeAlertNotifications?
    private var historyCache: [String: OverviewHistory] = [:]
    private var serviceGeneration = UUID()
    private var overviewRequestGeneration = UUID()
    var supportsNodePackages = false
    var isConnected = false
    var isLoading = false
    var lastUpdated: Date?
    var dataRefreshToken = UUID()
    var listCacheSession = UUID()
    let listPageCache = ListPageCache()
    private var directoryVersion: String?
    var errorMessage: String?

    private var api: APIClient?
    private var eventTask: Task<Void, Never>?
    private var refreshTask: Task<Void, Never>?
    @ObservationIgnored private var refreshFlight: Task<Bool, Never>?
    @ObservationIgnored private var pendingForcedRefresh = false
    // A display snapshot is not proof that all events through a persisted cursor
    // were applied. Each new connection obtains a head before loading its state.
    private var eventCursor: String?

    static func eventCursorStorageKey(service: String, actor: String) -> String {
        "management.eventCursor.v2." + String(data: try! JSONEncoder().encode([service, actor]), encoding: .utf8)!
    }

    private func saveEventCursor(_ cursor: String) throws {
        guard let actor = currentAdminTokenID else { throw APIClientError.invalidResponse }
        eventCursor = cursor
        UserDefaults.standard.set(cursor, forKey: Self.eventCursorStorageKey(service: serviceAddress, actor: actor))
    }

    private func synchronizeEvents(using client: APIClient) async throws {
        let generation = serviceGeneration
        let head: EventStreamHead = try await client.get(APIEndpoints.getEventStreamHead, bypassCache: true)
        let cursor = try ManagementEventCursor(head.cursor)
        guard cursor.epoch == head.epoch, cursor.sequence == head.publicationSeq else { throw APIClientError.invalidResponse }
        guard generation == serviceGeneration, api === client, !Task.isCancelled else { throw CancellationError() }
        guard await refresh(forceDirectories: true) else { throw APIClientError.invalidResponse }
        guard generation == serviceGeneration, api === client, !Task.isCancelled else { throw CancellationError() }
        try saveEventCursor(head.cursor)
    }

    init(client: APIClient? = nil, restoreSnapshot: Bool = true, nodeNotifications: NodeAlertNotifications? = nil) {
        self.nodeNotifications = nodeNotifications
        api = client
        guard restoreSnapshot else { return }
        restoreDisplaySnapshot()
        restoreDNSSnapshot()
        currentAdminTokenID = UserDefaults.standard.string(forKey: adminTokenIDKey)
        guard client == nil else { return }
        if let token = KeychainStore.readToken(), let url = URL(string: serviceAddress), url.scheme == "https" {
            let client = APIClient(baseURL: url, token: token)
            api = client
            Task {
                startEventUpdates(using: client)
            }
        }
    }

    func connect(serviceAddress: String, token: String, session: URLSession? = nil) async throws {
        guard let url = URL(string: serviceAddress.trimmingCharacters(in: .whitespacesAndNewlines)),
              url.scheme == "https", url.host != nil else { throw APIClientError.invalidBaseURL }
        let previousToken = KeychainStore.readToken()
        eventTask?.cancel()
        refreshTask?.cancel()
        let client = APIClient(baseURL: url, token: token, session: session)
        let _: APIHealth = try await client.get(APIEndpoints.readinessCheck)
        let version: APIVersion = try await client.get(APIEndpoints.getAPIVersion)
        currentAdminTokenID = version.currentAdminTokenId
        guard version.apiVersion == "2.0.0" && version.managementProtocol == 2 else { throw APIClientError.incompatibleAPI(version.apiVersion) }
        let normalizedAddress = url.absoluteString.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        let serviceChanged = normalizedAddress != self.serviceAddress
        let tokenChanged = previousToken != token
        try KeychainStore.saveToken(token)
        refreshFlight?.cancel()
        refreshFlight = nil
        pendingForcedRefresh = false
        isLoading = false
        listPageCache.invalidate(clear: true)
        listCacheSession = UUID()
        overviewMonitoringPage = nil
        overviewMonitoringPageKey = nil
        serviceGeneration = UUID()
        directoryVersion = nil
        eventCursor = nil
        for task in directoryTasks.values { task.cancel() }
        directoryTasks = [:]
        loadedDirectories = []
        loadingDirectories = []
        clientSummary = nil
        supportsClientSummary = version.features?.contains("client_summary") == true
        overviewRequestGeneration = UUID()
        authorizationGroupsRequestID = UUID()
        dnsRequestGeneration = UUID()
        if serviceChanged {
            historyCache = [:]
            overview = nil
            overviewError = nil
            supportsOverviewMonitoring = false
            supportsJobRetryLinks = false
            supportsDNSManagement = false
            dnsRequestGeneration = UUID()
            dnsConnections = []
            dnsZones = []
            dnsRecords = []
            dnsUpdatedAt = nil
            dnsError = nil
            requestedDNSRecordID = nil
            supportsNodeRecordRemoval = false
            supportsSSHDNSSync = false
            serverMonitoring = nil
            serverMonitoringError = nil
            nodes = []
            users = []
            authorizationGroups = []
            authorizationGroupsError = nil
            authorizationGroupsRequestID = UUID()
            isLoadingAuthorizationGroups = false
            jobs = []
            auditRecords = []
            adminTokens = []
            lastUpdated = nil
            currentAdminTokenID = version.currentAdminTokenId
        }
        if tokenChanged {
            currentAdminTokenID = version.currentAdminTokenId
            UserDefaults.standard.removeObject(forKey: "currentAdminTokenID.\(normalizedAddress)")
        }
        self.serviceAddress = normalizedAddress
        UserDefaults.standard.set(self.serviceAddress, forKey: "serviceAddress")
        api = client
        templateWindows.connectionChanged()
        isConnected = false
        restoreDisplaySnapshot()
        restoreDNSSnapshot()
        let connectionGeneration = serviceGeneration
        do {
            try await synchronizeEvents(using: client)
            await templateWindows.reconcile(store: self)
        } catch {
            guard connectionGeneration == serviceGeneration else { throw CancellationError() }
            isConnected = false
            errorMessage = error.localizedDescription
            throw error
        }
        startEventUpdates(using: client)
    }

    @discardableResult
    func refresh(forceDirectories: Bool = true) async -> Bool {
        guard let api else { isConnected = false; return false }
        if let flight = refreshFlight {
            if forceDirectories { pendingForcedRefresh = true }
            return await flight.value
        }
        let generation = serviceGeneration
        isLoading = true
        pendingForcedRefresh = false
        let flight = Task<Bool, Never> { [self] in
            defer {
                if generation == serviceGeneration {
                    refreshFlight = nil
                    pendingForcedRefresh = false
                    isLoading = false
                }
            }
            var force = forceDirectories
            var success = false
            repeat {
                success = await performRefresh(using: api, forceDirectories: force)
                guard generation == serviceGeneration, !Task.isCancelled else { return false }
                force = pendingForcedRefresh
                pendingForcedRefresh = false
            } while force
            return success
        }
        refreshFlight = flight
        return await flight.value
    }

    private func performRefresh(using api: APIClient, forceDirectories: Bool) async -> Bool {
        let refreshGeneration = serviceGeneration
        if forceDirectories {
            listPageCache.invalidate()
            historyCache = [:]
            overviewMonitoringPage = nil
            overviewMonitoringPageKey = nil
            overviewHistoryRefreshToken = UUID()
        }
        do {
            let version: APIVersion = try await api.get(APIEndpoints.getAPIVersion)
            guard refreshGeneration == serviceGeneration, !Task.isCancelled else { return false }
            currentAdminTokenID = version.currentAdminTokenId
            guard version.apiVersion == "2.0.0" && version.managementProtocol == 2 else { throw APIClientError.incompatibleAPI(version.apiVersion) }
            supportsClientSummary = version.features?.contains("client_summary") == true
            let changed=forceDirectories || directoryVersion != version.directoryVersion || !isConnected
            if changed && !forceDirectories { listPageCache.invalidate() }
            if changed {
                for task in directoryTasks.values { task.cancel() }
                directoryTasks = [:]; loadedDirectories = []; loadingDirectories = []
                try await loadData(using: api, bypassCache: forceDirectories)
                guard refreshGeneration == serviceGeneration, !Task.isCancelled else { return false }
                directoryVersion=version.directoryVersion
            }
            guard refreshGeneration == serviceGeneration, !Task.isCancelled else { return false }
            supportsDNSManagement = version.features?.contains("dns_management") == true
            supportsNodePackages = version.features?.contains("node_packages") == true
            supportsSSHDNSSync = version.features?.contains("ssh_dns_sync") == true
            supportsOverviewMonitoring = version.features?.contains("overview_monitoring") == true
            supportsJobRetryLinks = version.features?.contains("job_retry_links") == true
            supportsNodeRecordRemoval = version.features?.contains("node_record_removal") == true
            isConnected = true
            if changed, !(await refreshAuthorizationGroups()) { return false }
            guard refreshGeneration == serviceGeneration, !Task.isCancelled else { return false }
            errorMessage = nil
            if !changed {
                guard await refreshClientSummary(using: api) else { throw APIClientError.invalidResponse }
                let monitoring=await fetchServerMonitoring(using:api)
                guard refreshGeneration==serviceGeneration,!Task.isCancelled else {return false}
                serverMonitoring=monitoring.value ?? serverMonitoring; serverMonitoringError=monitoring.error
            }
            await (nodeNotifications ?? .shared).deliver(nodes: clientSummary?.notificationNodes ?? nodes, service: serviceAddress)
            await (nodeNotifications ?? .shared).deliver(credentials: clientSummary?.notificationCredentials ?? credentials, service: serviceAddress)
            guard await refreshOverview(bypassCache: forceDirectories) else { return false }
            guard refreshGeneration == serviceGeneration, !Task.isCancelled else { return false }
            dataRefreshToken=UUID()
            if changed, !(await refreshDNS(bypassCache: forceDirectories)) { return false }
            guard refreshGeneration == serviceGeneration, !Task.isCancelled else { return false }
            return true
        } catch {
            guard refreshGeneration == serviceGeneration, !Task.isCancelled else { return false }
            isConnected = false
            errorMessage = error.localizedDescription
            return false
        }
    }

    @discardableResult
    func refreshOverview(bypassCache: Bool = false) async -> Bool {
        guard supportsOverviewMonitoring else { overview = nil; overviewError = nil; return true }
        guard let api else { return false }
        let generation = serviceGeneration
        let request = UUID()
        overviewRequestGeneration = request
        do {
            let value = try await api.get(APIEndpoints.getOverview, bypassCache: bypassCache)
            guard generation == serviceGeneration, request == overviewRequestGeneration, !Task.isCancelled else { return false }
            overview = value
            overviewError = nil
            let snapshot = ClientDisplaySnapshot(credentials: Array(credentials.prefix(50)), nodes: Array(nodes.prefix(50)), users: Array(users.prefix(50)), jobs: jobs, auditRecords: auditRecords, updatedAt: lastUpdated ?? Date(), serverMonitoring: serverMonitoring, overview: value, clientSummary: clientSummary)
            if let data = try? JSONEncoder().encode(snapshot) { UserDefaults.standard.set(data, forKey: displaySnapshotKey) }
            return true
        } catch {
            guard generation == serviceGeneration, request == overviewRequestGeneration, !Task.isCancelled else { return false }
            overviewError = L10n.text("概览监控暂不可用：{0}", String(describing: (error.localizedDescription)))
            return false
        }
    }

    func overviewNodesPage(page: Int, pageSize: Int, query: String, cacheKey: String, revalidate: Bool) async throws -> OverviewNodesPage {
        let response = try await loadListPage(APIEndpoints.listOverviewNodes(page: page, pageSize: pageSize, q: query), cacheKey: cacheKey, ttl: 3, revalidate: revalidate)
        overviewMonitoringPage = response
        overviewMonitoringPageKey = cacheKey
        return response
    }

    func cachedOverviewNodesPage(_ key: String) -> OverviewNodesPage? {
        cachedListPage(key, as: OverviewNodesPage.self) ?? (overviewMonitoringPageKey == key ? overviewMonitoringPage : nil)
    }

    func overviewHistory(range: String, nodeID: String?, source: String, metrics: String = "all", bypassCache: Bool = false) async throws -> OverviewHistory {
        let client = try requireConnectedAPI()
        let generation = serviceGeneration
        let value = try await client.get(APIEndpoints.getOverviewHistory(metrics: metrics, range: range, timezone: TimeZone.current.identifier, nodeID: nodeID, source: source), bypassCache: bypassCache)
        guard generation == serviceGeneration, !Task.isCancelled else { throw CancellationError() }
        cacheOverviewHistory(value, nodeID: nodeID, metrics: metrics)
        return value
    }

    func cacheOverviewHistory(_ value: OverviewHistory, nodeID: String?, metrics: String = "all") {
        let key = historyKey(range: value.range, nodeID: nodeID, source: value.source, metrics: metrics)
        historyCache[key] = value
        if historyCache.count > 16, let oldest = historyCache.min(by: { $0.value.generatedAt < $1.value.generatedAt })?.key { historyCache.removeValue(forKey: oldest) }
        if let data = try? JSONEncoder().encode(historyCache) { UserDefaults.standard.set(data, forKey: "overviewHistory.\(serviceAddress)") }
    }

    private func historyKey(range: String, nodeID: String?, source: String, metrics: String) -> String { "rolling-v2|\(metrics)|\(TimeZone.current.identifier)|\(range)|\(nodeID ?? "all")|\(source)" }
    func requestOverviewHistoryRefresh() { overviewHistoryRefreshToken = UUID() }
    func cachedOverviewHistory(range: String, nodeID: String?, source: String, metrics: String = "all") -> OverviewHistory? { historyCache[historyKey(range: range, nodeID: nodeID, source: source, metrics: metrics)] }

    func createNode(_ request: NodeCreateRequest) async throws -> CreatedEntity {
        let api = try requireConnectedAPI()
        let generation = serviceGeneration
        let created: CreatedEntity = try await api.post(APIEndpoints.createNode, body: request)
        guard generation == serviceGeneration else { return created }
        if let node = created.node, !nodes.contains(where: { $0.id == node.id }) {
            let summary = NodeSummary(
                acmeStatus: nil,
                mtlsState: "ready", mtlsRecoveryConfigured: false, mtlsError: nil, mtlsGeneration: 0,
                publishedConnection: nil, dnsBinding: nil, package: request.package, packageUsage: nil,
                id: node.id, name: node.name, ssh: nil, revision: node.revision,
                deployedRevision: nil, state: node.state, lastSampleAt: nil,
                dataFreshness: "not_collected", mtlsRequired: nil, openGaps: nil,
                pendingRevocations: nil, proxyProbeUrl: request.proxyProbeUrl
            )
            if !loadedDirectories.contains(.nodes) { nodes = Array(nodes.suffix(199)) }
            nodes.append(summary)
        }
        listPageCache.invalidate(types: [NodesPage.self, OverviewNodesPage.self].map { String(reflecting: $0) })
        dataRefreshToken = UUID()
        // Creation has committed; unrelated directory/monitoring requests must
        // not delay showing the receipt and its one-time authentication token.
        Task { [weak self] in
            guard let self, generation == self.serviceGeneration else { return }
            await self.refresh()
        }
        return created
    }

    func createUser(_ request: UserCreateRequest) async throws -> String {
        let api = try requireConnectedAPI()
        let created: CreatedEntity = try await api.post(APIEndpoints.createUser, body: request)
        await refresh()
        guard let id = created.id else { throw APIClientError.invalidResponse }
        return id
    }

    func runNodeAction(_ node: NodeSummary, action: String) async throws -> JobReceipt {
        let api = try requireConnectedAPI()
        let operation: APIOperation<RevisionRequest, JobReceipt>
        switch action {
        case "ssh-test": operation = APIEndpoints.testNodeSSH(id: node.id)
        case "deploy": operation = APIEndpoints.deployNode(id: node.id)
        case "sync": operation = APIEndpoints.syncNode(id: node.id)
        case "rollback": operation = APIEndpoints.rollbackNode(id: node.id)
        default: throw APIClientError.server(L10n.text("未知的节点操作。"))
        }
        return try await api.post(operation, body: RevisionRequest(expectedRevision: node.revision))
    }

    func deleteNode(_ node: NodeSummary) async throws {
        let api = try requireConnectedAPI()
        _ = try await api.delete(APIEndpoints.deleteNode(id: node.id, expectedRevision: node.revision))
        await refresh()
    }

    func removeNodeRecord(_ node: NodeSummary) async throws {
        let api = try requireConnectedAPI()
        guard supportsNodeRecordRemoval else { throw APIClientError.server(L10n.text("请先更新管理服务以支持仅移除节点记录。")) }
        try await api.deleteNoContent(APIEndpoints.removeNodeRecord(id: node.id, expectedRevision: node.revision))
        await refresh()
    }

    func nodesPage(page: Int, pageSize: Int, query: String, sort: String, order: String, cacheKey: String? = nil, revalidate: Bool = false) async throws -> NodesPage {
        let result: NodesPage = try await loadListPage(APIEndpoints.listNodes(page: page, pageSize: pageSize, q: query, stateMatches: NodeDisplayText.matchingStates(query), sort: sort, order: order), cacheKey: cacheKey, ttl: 15, revalidate: revalidate)
        if !loadedDirectories.contains(.nodes) { nodes = result.items }
        return result
    }

    func nodeDetail(_ nodeID: String) async throws -> NodeDetail {
        let api = try requireConnectedAPI()
        return try await api.get(APIEndpoints.getNode(id: nodeID))
    }

    func retryJob(_ job: JobSummary, on node: NodeSummary) async throws {
        let client = try requireConnectedAPI()
        guard supportsJobRetryLinks else { throw APIClientError.server(L10n.text("请先更新管理服务以支持关联重试。")) }
        let _: JobReceipt = try await client.post(APIEndpoints.retryJob(id: job.id), body: RevisionRequest(expectedRevision: node.revision))
        await refresh()
    }

    func usersPage(page: Int, pageSize: Int, query: String, order: String, cacheKey: String? = nil, revalidate: Bool = false) async throws -> UsersPage {
        let result: UsersPage = try await loadListPage(APIEndpoints.listUsers(page: page, pageSize: pageSize, q: query, sort: "name", order: order), cacheKey: cacheKey, ttl: 15, revalidate: revalidate)
        if !loadedDirectories.contains(.users) { users = result.items }
        return result
    }

    func auditPage(page: Int, pageSize: Int, query: String, sort: String, order: String, cacheKey: String? = nil, revalidate: Bool = false) async throws -> AuditPage {
        let matches = AuditDisplayText.searchMatches(query)
        return try await loadListPage(APIEndpoints.listAuditRecords(page: page, pageSize: pageSize, q: query, sort: sort, order: order, actionMatches: matches.actions, entityTypeMatches: matches.entityTypes, actorMatches: matches.actors), cacheKey: cacheKey, ttl: 60, revalidate: revalidate)
    }

    func jobsPage(page: Int, pageSize: Int, query: String, sort: String, order: String, cacheKey: String? = nil, revalidate: Bool = false) async throws -> JobsPage {
        try await loadListPage(APIEndpoints.listJobs(page: page, pageSize: pageSize, q: query, sort: sort, order: order), cacheKey: cacheKey, ttl: 15, revalidate: revalidate)
    }

    func jobDetail(_ jobID: String) async throws -> JobDetailResponse {
        let api = try requireConnectedAPI()
        return try await api.get(APIEndpoints.getJob(id: jobID))
    }

    func executionOperations(workID: String, after: String? = nil) async throws -> ExecutionOperationsPage {
        let client = try requireConnectedAPI()
        let generation = serviceGeneration
        let page = try await client.get(APIEndpoints.listExecutionOperations(workId: workID, after: after), bypassCache: true)
        guard generation == serviceGeneration, api === client, !Task.isCancelled else { throw CancellationError() }
        guard page.items.count <= 100, page.items.allSatisfy({ $0.workId == workID }), Set(page.items.map(\.operationId)).count == page.items.count else { throw APIClientError.invalidResponse }
        return page
    }

    func reverifyExecution(_ operation: ExecutionOperation) async throws -> ExecutionRecheckResult {
        let client = try requireConnectedAPI()
        let generation = serviceGeneration
        let result = try await client.post(APIEndpoints.reverifyExecutionOperation(id: operation.operationId), body: ReverifyExecutionOperationRequest(expectedEpoch: operation.epoch, expectedGeneration: operation.generation))
        guard generation == serviceGeneration, api === client, !Task.isCancelled else { throw CancellationError() }
        guard result.operationId == operation.operationId, ["verified", "needs_attention"].contains(result.state), result.state == "verified" || (result.resourceBlocked && !result.retryAllowed) else { throw APIClientError.invalidResponse }
        await refresh()
        return result
    }

    func userUsage(_ userID: String) async throws -> UserUsageResponse {
        let api = try requireConnectedAPI()
        return try await api.get(APIEndpoints.getUserUsage(id: userID))
    }

    func updateNodeConfig(
        _ detail: NodeDetail,
        config: JSONValue,
        listenAddress: String,
        publicPort: Int,
        trafficStatsPort: Int,
        proxyProbeURL: String,
        tlsSNI: String,
        skipCertVerify: Bool
    ) async throws {
        let api = try requireConnectedAPI()
        let _: NodeUpdateResponse = try await api.patch(
            APIEndpoints.updateNode(id: detail.id),
            body: NodePatchRequest(
                expectedRevision: detail.revision,
                publicPort: publicPort,
                listenAddr: listenAddress,
                trafficStatsPort: trafficStatsPort,
                proxyProbeUrl: proxyProbeURL,
                tlsSNI: tlsSNI.trimmingCharacters(in: .whitespacesAndNewlines),
                tlsSkipVerify: skipCertVerify,
                config: config.objectValue ?? [:]
            )
        )
        await refresh()
    }

    @discardableResult
    func updateServerConfiguration(nodeID: String, request: NodePatchRequest) async throws -> NodeUpdateResponse {
        let api = try requireConnectedAPI()
        let response: NodeUpdateResponse = try await api.patch(APIEndpoints.updateNode(id: nodeID), body: request)
        await refresh()
        return response
    }

    func updateNodePackage(_ detail: NodeDetail, package: NodePackage) async throws {
        let api = try requireConnectedAPI()
        let _: NodeUpdateResponse = try await api.patch(
            APIEndpoints.updateNode(id: detail.id),
            body: NodePatchRequest(expectedRevision: detail.revision, package: package)
        )
        await refresh()
    }

    func updateNodePackageUsage(_ detail: NodeDetail, usage: Int64, reset: Bool) async throws {
        let api = try requireConnectedAPI()
        let _: NodeUsageUpdateResponse = try await api.put(
            APIEndpoints.updateNodeUsage(id: detail.id),
            body: NodeUsageUpdateRequest(expectedRevision: detail.revision, usageBytes: usage, reset: reset)
        )
        await refresh()
    }

    func nodeResources(_ nodeID: String) async throws -> [NodeResource] {
        let api = try requireConnectedAPI()
        return try await api.get(APIEndpoints.listNodeResources(id: nodeID))
    }

    func uploadResource(nodeID: String, name: String, kind: String, data: Data) async throws -> ResourceReceipt {
        guard ["acl", "geoip", "geosite"].contains(kind) else {
            throw APIClientError.server(L10n.text("配置资源仅支持 ACL、GeoIP 和 GeoSite；证书和密钥请在凭据中心导入。"))
        }
        let api = try requireConnectedAPI()
        let request = ResourceUploadRequest(name: name, resourceKind: kind, contentBase64: data.base64EncodedString())
        return try await api.post(APIEndpoints.uploadNodeResource(id: nodeID), body: request)
    }

    func confirmChangedHostFingerprint(_ job: JobSummary) async throws {
        guard let change = SSHHostFingerprintChange(job: job), let nodeID = job.nodeID else {
            throw APIClientError.server(L10n.text("此任务没有可确认的 SSH 指纹变更。"))
        }
        let client = try requireConnectedAPI()
        let generation = serviceGeneration
        let latestJob: JobDetailResponse = try await client.get(APIEndpoints.getJob(id: job.id))
        let node: NodeDetail = try await client.get(APIEndpoints.getNode(id: nodeID))
        guard generation == serviceGeneration, !Task.isCancelled else { throw CancellationError() }
        guard latestJob.job.nodeID == nodeID,
              latestJob.job.retryJobId == nil,
              SSHHostFingerprintChange(job: latestJob.job) == change,
              change.canConfirm(state: node.state, savedFingerprint: node.ssh.hostFingerprint) else {
            throw APIClientError.server(L10n.text("节点指纹或任务状态已变化，请刷新并重新运行 SSH 测试后再确认。"))
        }
        let _: NodeUpdateResponse = try await client.patch(
            APIEndpoints.updateNode(id: nodeID),
            body: NodePatchRequest(expectedRevision: node.revision, sshHostFingerprint: change.observed)
        )
        guard generation == serviceGeneration else { throw CancellationError() }
        await refresh()
    }

    func confirmJobFingerprint(_ job: JobSummary, fingerprint: String) async throws {
        guard ["ssh-test", "deploy", "sync", "rollback"].contains(job.kind), job.status == "waiting_confirmation", let nodeID = job.nodeID else {
            throw APIClientError.server(L10n.text("此任务没有可确认的 SSH 指纹变更。"))
        }
        let client = try requireConnectedAPI()
        let generation = serviceGeneration
        let node: NodeDetail = try await client.get(APIEndpoints.getNode(id: nodeID))
        guard generation == serviceGeneration, !Task.isCancelled else { throw CancellationError() }
        let _: JobReceipt = try await client.post(
            APIEndpoints.confirmJobFingerprint(id: job.id),
            body: JobFingerprintConfirmationRequest(expectedRevision: node.revision, fingerprint: fingerprint)
        )
        guard generation == serviceGeneration else { throw CancellationError() }
        await refresh()
    }

    func confirmHostFingerprint(nodeID: String, fingerprint: String) async throws {
        guard let node = nodes.first(where: { $0.id == nodeID }) else {
            throw APIClientError.server(L10n.text("找不到对应节点。"))
        }
        let api = try requireConnectedAPI()
        let _: NodeUpdateResponse = try await api.patch(
            APIEndpoints.updateNode(id: nodeID),
            body: NodePatchRequest(expectedRevision: node.revision, sshHostFingerprint: fingerprint)
        )
        await refresh()
    }

    func rotateSubscription(for user: UserSummary) async throws -> String {
        let api = try requireConnectedAPI()
        let response: SubscriptionReceipt = try await api.post(
            APIEndpoints.rotateSubscription(id: user.id),
            body: RevisionRequest(expectedRevision: user.revision)
        )
        await refresh()
        return response.url
    }

    func currentSubscriptionURL(for user: UserSummary, format: SubscriptionFileFormat? = nil, template: String? = nil, platform: String? = nil) async throws -> String {
        let api = try requireConnectedAPI()
        let subscription: SubscriptionStatus = try await api.get(APIEndpoints.getSubscription(id: user.id))
        guard let active = subscription.active else {
            throw APIClientError.server(L10n.text("此用户尚未生成订阅地址，请先生成订阅。"))
        }
        if let format {
            return try format.subscriptionURL(baseURL: active.url, template: template, platform: platform)
        }
        return active.url
    }

    func subscriptionFile(for user: UserSummary, format: SubscriptionFileFormat, template: String? = nil, platform: String? = nil) async throws -> Data {
        let api = try requireConnectedAPI()
        let subscription: SubscriptionStatus = try await api.get(APIEndpoints.getSubscription(id: user.id))
        guard let active = subscription.active else {
            throw APIClientError.server(L10n.text("此用户尚未生成订阅地址，请先生成订阅再导出。"))
        }
        _ = try format.subscriptionURL(baseURL: active.url, template: template, platform: platform)
        let operation = APIEndpoints.downloadAutomaticSubscription(token: active.token, template: template, format: format.rawValue, platform: platform)
        return try await api.download(operation, accept: format.accept)
    }

    func setEnabled(_ enabled: Bool, for user: UserSummary) async throws {
        let api = try requireConnectedAPI()
        let _: UserUpdateResponse = try await api.patch(
            APIEndpoints.updateUser(id: user.id),
            body: UserPatchRequest(expectedRevision: user.revision, enabled: enabled)
        )
        await refresh()
    }

    func updateUserDetails(
        _ user: UserSummary,
        name: String,
        enabled: Bool,
        expiresAt: Date?,
        quotaBytes: Int64?
    ) async throws {
        let api = try requireConnectedAPI()
        let expiryValue = expiresAt.map {
            PatchValue<String>.value(ISO8601DateFormatter().string(from: $0))
        } ?? .null
        let quotaValue = quotaBytes.map(PatchValue<Int64>.value) ?? .null
        let _: UserUpdateResponse = try await api.patch(
            APIEndpoints.updateUser(id: user.id),
            body: UserPatchRequest(
                expectedRevision: user.revision,
                name: name,
                enabled: enabled,
                expiresAt: expiryValue,
                quotaBytes: quotaValue
            )
        )
        await refresh()
    }

    func resetQuota(for user: UserSummary) async throws {
        let api = try requireConnectedAPI()
        let _: QuotaResetResponse = try await api.post(
            APIEndpoints.resetUserQuota(id: user.id),
            body: RevisionRequest(expectedRevision: user.revision)
        )
        await refresh()
    }

    func rotateCredentials(for user: UserSummary) async throws -> String {
        let api = try requireConnectedAPI()
        let response: RotatedCredentials = try await api.post(
            APIEndpoints.rotateUserCredentials(id: user.id),
            body: RevisionRequest(expectedRevision: user.revision)
        )
        await refresh()
        return response.credentials.map { "\($0.nodeID): \($0.credential)" }.joined(separator: "\n")
    }

    func delete(_ user: UserSummary) async throws {
        let api = try requireConnectedAPI()
        try await api.deleteNoContent(APIEndpoints.deleteUser(id: user.id, expectedRevision: user.revision))
        await refresh()
    }

    func createAndSwitchAdminToken(label: String) async throws -> AdminTokenReceipt {
        let currentAPI = try requireConnectedAPI()
        let normalizedLabel = label.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !normalizedLabel.isEmpty, normalizedLabel.unicodeScalars.count <= 100 else {
            throw APIClientError.server(L10n.text("令牌用途需为 1 到 100 个字符。"))
        }
        guard let url = URL(string: serviceAddress), url.scheme == "https", url.host != nil else {
            throw APIClientError.invalidBaseURL
        }
        let receipt: AdminTokenReceipt = try await currentAPI.post(
            APIEndpoints.createAdminToken,
            body: CreateAdminTokenRequest(label: normalizedLabel)
        )
        let nextAPI = APIClient(baseURL: url, token: receipt.token)

        do {
            try KeychainStore.saveToken(receipt.token)
        } catch {
            errorMessage = L10n.text("令牌已创建，但无法保存到 Keychain；请立即复制并安全保存此令牌。{0}", String(describing: (error.localizedDescription)))
            return receipt
        }
        eventTask?.cancel()
        refreshTask?.cancel()
        refreshFlight?.cancel()
        refreshFlight = nil
        pendingForcedRefresh = false
        serviceGeneration = UUID()
        eventCursor = nil
        listPageCache.invalidate(clear: true)
        listCacheSession = UUID()
        authorizationGroupsRequestID = UUID()
        dnsRequestGeneration = UUID()
        api = nextAPI
        currentAdminTokenID = receipt.id
        UserDefaults.standard.set(receipt.id, forKey: adminTokenIDKey)
        do {
            let _: APIHealth = try await nextAPI.get(APIEndpoints.readinessCheck)
            let version: APIVersion = try await nextAPI.get(APIEndpoints.getAPIVersion)
            currentAdminTokenID = version.currentAdminTokenId
            guard version.apiVersion == "2.0.0" && version.managementProtocol == 2 else {
                throw APIClientError.incompatibleAPI(version.apiVersion)
            }
            try await synchronizeEvents(using: nextAPI)
        } catch {
            isConnected = false
            errorMessage = L10n.text("新令牌已保存并切换，但服务暂不可用：{0}", String(describing: (error.localizedDescription)))
        }
        startEventUpdates(using: nextAPI)
        return receipt
    }

    func revokeAdminToken(_ token: AdminTokenSummary) async throws {
        guard let currentAdminTokenID else {
            throw APIClientError.server(L10n.text("先创建并切换到本 Mac 的新令牌，再撤销旧令牌。"))
        }
        guard token.id != currentAdminTokenID else {
            throw APIClientError.server(L10n.text("不能撤销本 Mac 当前使用的令牌。"))
        }
        let api = try requireConnectedAPI()
        try await api.deleteNoContent(APIEndpoints.revokeAdminToken(id: token.id))
        await refresh()
    }

    private func loadData(using client: APIClient, bypassCache: Bool = false) async throws {
        let generation = serviceGeneration
        async let loadedMonitoring = fetchServerMonitoring(using: client)
        async let loadedCredentials: CredentialsPage = client.get(APIEndpoints.listCredentials(page: 1, pageSize: 50), bypassCache: bypassCache)
        async let loadedNodes: NodesPage = client.get(APIEndpoints.listNodes(page: 1, pageSize: 50), bypassCache: bypassCache)
        async let loadedUsers: UsersPage = client.get(APIEndpoints.listUsers(page: 1, pageSize: 50), bypassCache: bypassCache)
        async let loadedJobs: JobsPage = client.get(APIEndpoints.listJobs(pageSize: 200), bypassCache: bypassCache)
        async let loadedAudit: AuditPage = client.get(APIEndpoints.listAuditRecords(pageSize: 200), bypassCache: bypassCache)
        async let loadedAdminTokens: [AdminTokenSummary] = client.get(APIEndpoints.listAdminTokens)
        let tokens = try await loadedAdminTokens
        let monitoring = await loadedMonitoring
        let snapshot = try await ClientDisplaySnapshot(
            credentials: loadedCredentials.items,
            nodes: loadedNodes.items,
            users: loadedUsers.items,
            jobs: loadedJobs.items,
            auditRecords: loadedAudit.items,
            updatedAt: Date(),
            serverMonitoring: monitoring.value ?? serverMonitoring,
            overview: overview
        )
        guard generation == serviceGeneration, !Task.isCancelled else { throw CancellationError() }
        serverMonitoring = snapshot.serverMonitoring
        serverMonitoringError = monitoring.error
        credentials = snapshot.credentials ?? []
        nodes = snapshot.nodes
        users = snapshot.users
        jobs = snapshot.jobs
        auditRecords = snapshot.auditRecords
        adminTokens = tokens
        if !supportsClientSummary {
            // Older servers have no independent counters/notification candidates.
            // Preserve their complete-data behavior until the server is upgraded.
            async let oldNodes: [NodeSummary] = client.collectPages(bypassCache: bypassCache) { APIEndpoints.listNodes(page: $0, pageSize: 200) }
            async let oldUsers: [UserSummary] = client.collectPages(bypassCache: bypassCache) { APIEndpoints.listUsers(page: $0, pageSize: 200) }
            async let oldCredentials: [CredentialSummary] = client.collectPages(bypassCache: bypassCache) { APIEndpoints.listCredentials(page: $0, pageSize: 200) }
            let legacy = try await (oldNodes, oldUsers, oldCredentials)
            guard generation == serviceGeneration, !Task.isCancelled else { throw CancellationError() }
            nodes = legacy.0; users = legacy.1; credentials = legacy.2
            loadedDirectories = [.nodes, .users, .credentials]
        }
        guard await refreshClientSummary(using: client) else { throw APIClientError.invalidResponse }
        guard generation == serviceGeneration, !Task.isCancelled else { throw CancellationError() }
        lastUpdated = snapshot.updatedAt
        var display = snapshot
        display.clientSummary = clientSummary
        if let data = try? JSONEncoder().encode(display) {
            UserDefaults.standard.set(data, forKey: displaySnapshotKey)
        }
    }

    private func refreshClientSummary(using client: APIClient) async -> Bool {
        guard supportsClientSummary else { clientSummary = nil; return true }
        let generation = serviceGeneration
        do {
            let value = try await client.get(APIEndpoints.getClientSummary)
            guard generation == serviceGeneration, !Task.isCancelled else { return false }
            clientSummary = value
            return true
        } catch {
            // Keep the last complete summary without acknowledging an incomplete refresh.
            return false
        }
    }

    // Full directories are needed only by legacy bulk-selection controls. Deduplicate
    // those explicit reads and invalidate them on committed configuration changes.
    func prepareDirectory(_ directory: Directory) async {
        guard !loadedDirectories.contains(directory), let client = try? requireConnectedAPI() else { return }
        if let task = directoryTasks[directory] { await task.value; return }
        let generation = serviceGeneration
        let request = listCacheSession
        loadingDirectories.insert(directory)
        let task = Task { [weak self] in
            guard let self else { return }
            do {
                switch directory {
                case .nodes:
                    let values: [NodeSummary] = try await client.collectPages { APIEndpoints.listNodes(page: $0, pageSize: 200) }
                    guard generation == self.serviceGeneration, request == self.listCacheSession, !Task.isCancelled else { return }
                    self.nodes = values
                case .users:
                    let values: [UserSummary] = try await client.collectPages { APIEndpoints.listUsers(page: $0, pageSize: 200) }
                    guard generation == self.serviceGeneration, request == self.listCacheSession, !Task.isCancelled else { return }
                    self.users = values
                case .credentials:
                    let values: [CredentialSummary] = try await client.collectPages { APIEndpoints.listCredentials(page: $0, pageSize: 200) }
                    guard generation == self.serviceGeneration, request == self.listCacheSession, !Task.isCancelled else { return }
                    self.credentials = values
                }
                self.loadedDirectories.insert(directory)
            } catch {
                guard generation == self.serviceGeneration, !Task.isCancelled else { return }
                self.errorMessage = error.localizedDescription
            }
            guard generation == self.serviceGeneration, !Task.isCancelled else { return }
            self.loadingDirectories.remove(directory)
            self.directoryTasks[directory] = nil
        }
        directoryTasks[directory] = task
        await task.value
    }

    func prepareDirectories(_ directories: [Directory]) async {
        for directory in directories { await prepareDirectory(directory) }
    }

    func prepareNode(_ id: String?) async {
        guard let id else { return }
        do {
            let generation = serviceGeneration
            let detail = try await nodeDetail(id)
            let value = try JSONDecoder().decode(NodeSummary.self, from: JSONEncoder().encode(detail))
            guard generation == serviceGeneration, !Task.isCancelled else { return }
            nodes.removeAll { $0.id == id }
            if !loadedDirectories.contains(.nodes) { nodes = Array(nodes.suffix(199)) }
            nodes.append(value)
        } catch { }
    }

    private func fetchServerMonitoring(using client: APIClient) async -> (value: ServerMonitoring?, error: String?) {
        do {
            return (try await client.get(APIEndpoints.getServerMonitoring), nil)
        } catch {
            return (nil, L10n.text("监控信息暂不可用：{0}", String(describing: (error.localizedDescription))))
        }
    }

    private var displaySnapshotKey: String { "displaySnapshot.\(serviceAddress)" }
    private var adminTokenIDKey: String { "currentAdminTokenID.\(serviceAddress)" }

    private func restoreDisplaySnapshot() {
        if let data = UserDefaults.standard.data(forKey: "overviewHistory.\(serviceAddress)"), let values = try? JSONDecoder().decode([String: OverviewHistory].self, from: data) { historyCache = values }
        guard let data = UserDefaults.standard.data(forKey: displaySnapshotKey),
              let snapshot = try? JSONDecoder().decode(ClientDisplaySnapshot.self, from: data) else { return }
        credentials = Array((snapshot.credentials ?? []).prefix(50))
        nodes = Array(snapshot.nodes.prefix(50))
        users = Array(snapshot.users.prefix(50))
        jobs = snapshot.jobs
        auditRecords = snapshot.auditRecords
        serverMonitoring = snapshot.serverMonitoring
        clientSummary = snapshot.clientSummary
        overview = snapshot.overview
        supportsOverviewMonitoring = snapshot.overview != nil
        lastUpdated = snapshot.updatedAt
    }

    func requireConnectedAPI() throws -> APIClient {
        guard isConnected, let api else {
            throw APIClientError.server(L10n.text("管理服务当前不可用；恢复连接后才能读取详情或提交写操作。"))
        }
        return api
    }

    private func startEventUpdates(using client: APIClient) {
        eventTask?.cancel()
        refreshTask?.cancel()

        eventTask = Task { [weak self] in
            guard let self else { return }
            let generation = self.serviceGeneration
            let address = self.serviceAddress
            var retryDelay = 1.0
            var appliedSources = Set<Int64>()
            var sourceOrder: [Int64] = []
            while !Task.isCancelled {
                do {
                    if self.eventCursor == nil {
                        try await self.synchronizeEvents(using: client)
                        appliedSources = []; sourceOrder = []
                    }
                    let events = try await client.events(APIEndpoints.streamEvents, lastEventID: self.eventCursor)
                    for try await event in events {
                        guard generation == self.serviceGeneration, address == self.serviceAddress, !Task.isCancelled else { return }
                        let incoming = try ManagementEventCursor(event.cursor)
                        guard let saved = self.eventCursor else { throw APIClientError.invalidResponse }
                        let previous = try ManagementEventCursor(saved)
                        guard incoming.epoch == previous.epoch else { throw APIClientError.eventCursorExpired }
                        guard incoming.sequence > previous.sequence else {
                            if incoming.sequence == previous.sequence { continue }
                            throw APIClientError.invalidResponse
                        }
                        let duplicate = event.sourceID.map { appliedSources.contains($0) } == true
                        if event.kind != "stream.cursor", !duplicate {
                            self.listPageCache.invalidate(types: [NodesPage.self, UsersPage.self, JobsPage.self, OverviewNodesPage.self].map { String(reflecting: $0) })
                            guard await self.refresh(forceDirectories: true) else { throw APIClientError.invalidResponse }
                        }
                        guard generation == self.serviceGeneration, address == self.serviceAddress, !Task.isCancelled else { return }
                        guard self.isConnected else { throw APIClientError.invalidResponse }
                        if let source = event.sourceID, appliedSources.insert(source).inserted {
                            sourceOrder.append(source)
                            if sourceOrder.count > 1024 { appliedSources.remove(sourceOrder.removeFirst()) }
                        }
                        try self.saveEventCursor(event.cursor)
                    }
                    retryDelay = 1
                } catch {
                    guard generation == self.serviceGeneration, address == self.serviceAddress, !Task.isCancelled else { return }
                    if case APIClientError.eventCursorExpired = error {
                        self.eventCursor = nil
                        appliedSources = []; sourceOrder = []
                    }
                }
                guard !Task.isCancelled else { return }
                try? await Task.sleep(for: .seconds(retryDelay))
                retryDelay = min(retryDelay * 2, 30)
            }
        }

        refreshTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(15))
                guard !Task.isCancelled else { return }
                await self?.refresh(forceDirectories: false)
            }
        }
    }
}

extension ManagementStore {
    func listPageKey(_ fields: [String]) -> String {
        String(data: try! JSONEncoder().encode([listCacheSession.uuidString, serviceAddress, L10n.locale.identifier] + fields), encoding: .utf8)!
    }

    private func listCacheKey<Value>(_ key: String, as: Value.Type) -> String {
        // Encode fields independently so delimiters in a search cannot alias another query.
        String(data: try! JSONEncoder().encode([serviceAddress, L10n.locale.identifier, String(reflecting: Value.self), key]), encoding: .utf8)!
    }

    func cachedListPage<Value: Decodable>(_ key: String, as: Value.Type) -> Value? {
        listPageCache.peek(listCacheKey(key, as: Value.self), as: Value.self)
    }

    func loadListPage<Value: Codable & Sendable>(
        _ operation: APIOperation<NoRequest, Value>, cacheKey: String?, ttl: TimeInterval, revalidate: Bool = false
    ) async throws -> Value {
        let client = try requireConnectedAPI()
        let requestKey = String(data: try JSONEncoder().encode([operation.path] + operation.queryParameters.sorted(by: { $0.key < $1.key }).flatMap { [$0.key, $0.value] }), encoding: .utf8)!
        return try await listPageCache.load(listCacheKey(cacheKey ?? requestKey, as: Value.self), ttl: ttl, revalidate: revalidate) { bypassCache in
            try await client.get(operation, bypassCache: bypassCache)
        }
    }
}
