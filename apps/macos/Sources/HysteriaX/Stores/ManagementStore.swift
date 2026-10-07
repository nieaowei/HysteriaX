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
}

@MainActor
@Observable
final class ManagementStore {
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
    var jobs: [JobSummary] = []
    var auditRecords: [AuditSummary] = []
    var adminTokens: [AdminTokenSummary] = []
    var currentAdminTokenID: String?
    var serverMonitoring: ServerMonitoring?
    var serverMonitoringError: String?
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
    var errorMessage: String?

    private var api: APIClient?
    private var eventTask: Task<Void, Never>?
    private var refreshTask: Task<Void, Never>?
    private var lastEventID: String? {
        UserDefaults.standard.string(forKey: "lastEventID.\(serviceAddress)")
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
                await refresh()
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
        guard version.apiVersion == "1.0.0" else { throw APIClientError.incompatibleAPI(version.apiVersion) }
        let normalizedAddress = url.absoluteString.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        let serviceChanged = normalizedAddress != self.serviceAddress
        let tokenChanged = previousToken != token
        try KeychainStore.saveToken(token)
        serviceGeneration = UUID()
        overviewRequestGeneration = UUID()
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
            serverMonitoring = nil
            serverMonitoringError = nil
            nodes = []
            users = []
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
        isConnected = false
        restoreDisplaySnapshot()
        restoreDNSSnapshot()
        let connectionGeneration = serviceGeneration
        do {
            try await loadData(using: client)
            guard connectionGeneration == serviceGeneration else { throw CancellationError() }
            isConnected = true
            errorMessage = nil
            await (nodeNotifications ?? .shared).deliver(nodes: nodes, service: serviceAddress)
            await (nodeNotifications ?? .shared).deliver(credentials: credentials, service: serviceAddress)
        } catch {
            guard connectionGeneration == serviceGeneration else { throw CancellationError() }
            isConnected = false
            errorMessage = error.localizedDescription
            throw error
        }
        supportsDNSManagement = version.features?.contains("dns_management") == true
        supportsNodePackages = version.features?.contains("node_packages") == true
        supportsOverviewMonitoring = version.features?.contains("overview_monitoring") == true
        supportsJobRetryLinks = version.features?.contains("job_retry_links") == true
        supportsNodeRecordRemoval = version.features?.contains("node_record_removal") == true
        await refreshOverview()
        await refreshDNS()
        startEventUpdates(using: client)
    }

    func refresh() async {
        guard let api else { isConnected = false; return }
        let refreshGeneration = serviceGeneration
        isLoading = true
        defer { if refreshGeneration == serviceGeneration { isLoading = false } }
        do {
            let version: APIVersion = try await api.get(APIEndpoints.getAPIVersion)
            currentAdminTokenID = version.currentAdminTokenId
            guard version.apiVersion == "1.0.0" else { throw APIClientError.incompatibleAPI(version.apiVersion) }
            try await loadData(using: api)
            guard refreshGeneration == serviceGeneration, !Task.isCancelled else { return }
            supportsDNSManagement = version.features?.contains("dns_management") == true
            supportsNodePackages = version.features?.contains("node_packages") == true
            supportsOverviewMonitoring = version.features?.contains("overview_monitoring") == true
            supportsJobRetryLinks = version.features?.contains("job_retry_links") == true
            supportsNodeRecordRemoval = version.features?.contains("node_record_removal") == true
            isConnected = true
            errorMessage = nil
            await (nodeNotifications ?? .shared).deliver(nodes: nodes, service: serviceAddress)
            await (nodeNotifications ?? .shared).deliver(credentials: credentials, service: serviceAddress)
            await refreshOverview()
            await refreshDNS()
        } catch {
            guard refreshGeneration == serviceGeneration, !Task.isCancelled else { return }
            isConnected = false
            errorMessage = error.localizedDescription
        }
    }

    func refreshOverview() async {
        guard supportsOverviewMonitoring else { overview = nil; overviewError = nil; return }
        guard let api else { return }
        let generation = serviceGeneration
        let request = UUID()
        overviewRequestGeneration = request
        do {
            let value = try await api.get(APIEndpoints.getOverview)
            guard generation == serviceGeneration, request == overviewRequestGeneration, !Task.isCancelled else { return }
            overview = value
            overviewError = nil
            let snapshot = ClientDisplaySnapshot(credentials: credentials, nodes: nodes, users: users, jobs: jobs, auditRecords: auditRecords, updatedAt: lastUpdated ?? Date(), serverMonitoring: serverMonitoring, overview: value)
            if let data = try? JSONEncoder().encode(snapshot) { UserDefaults.standard.set(data, forKey: displaySnapshotKey) }
        } catch {
            guard generation == serviceGeneration, request == overviewRequestGeneration, !Task.isCancelled else { return }
            overviewError = L10n.text("概览监控暂不可用：{0}", String(describing: (error.localizedDescription)))
        }
    }

    func overviewHistory(range: String, nodeID: String?, source: String) async throws -> OverviewHistory {
        let client = try requireConnectedAPI()
        let generation = serviceGeneration
        let value = try await client.get(APIEndpoints.getOverviewHistory(range: range, timezone: TimeZone.current.identifier, nodeID: nodeID, source: source))
        guard generation == serviceGeneration, !Task.isCancelled else { throw CancellationError() }
        cacheOverviewHistory(value, nodeID: nodeID)
        return value
    }

    func cacheOverviewHistory(_ value: OverviewHistory, nodeID: String?) {
        let key = historyKey(range: value.range, nodeID: nodeID, source: value.source)
        historyCache[key] = value
        if historyCache.count > 16, let oldest = historyCache.min(by: { $0.value.generatedAt < $1.value.generatedAt })?.key { historyCache.removeValue(forKey: oldest) }
        if let data = try? JSONEncoder().encode(historyCache) { UserDefaults.standard.set(data, forKey: "overviewHistory.\(serviceAddress)") }
    }

    private func historyKey(range: String, nodeID: String?, source: String) -> String { "rolling-v1|\(TimeZone.current.identifier)|\(range)|\(nodeID ?? "all")|\(source)" }
    func requestOverviewHistoryRefresh() { overviewHistoryRefreshToken = UUID() }
    func cachedOverviewHistory(range: String, nodeID: String?, source: String) -> OverviewHistory? { historyCache[historyKey(range: range, nodeID: nodeID, source: source)] }

    func createNode(_ request: NodeCreateRequest) async throws -> CreatedEntity {
        let api = try requireConnectedAPI()
        let created: CreatedEntity = try await api.post(APIEndpoints.createNode, body: request)
        await refresh()
        return created
    }

    func createUser(_ request: UserCreateRequest) async throws {
        let api = try requireConnectedAPI()
        let _: CreatedEntity = try await api.post(APIEndpoints.createUser, body: request)
        await refresh()
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

    func jobDetail(_ jobID: String) async throws -> JobDetailResponse {
        let api = try requireConnectedAPI()
        return try await api.get(APIEndpoints.getJob(id: jobID))
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

    func updateServerConfiguration(nodeID: String, request: NodePatchRequest) async throws {
        let api = try requireConnectedAPI()
        let _: NodeUpdateResponse = try await api.patch(APIEndpoints.updateNode(id: nodeID), body: request)
        await refresh()
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

    func assign(
        _ user: UserSummary,
        to node: NodeSummary,
        clientCertificate: String? = nil,
        clientPrivateKey: String? = nil,
        mtlsCredentialID: String? = nil
    ) async throws -> String {
        let api = try requireConnectedAPI()
        let mtls = try await resolveMTLSCredential(userID: user.id, nodeID: node.id, certificate: clientCertificate, privateKey: clientPrivateKey, selected: mtlsCredentialID)
        let response: AssignmentReceipt = try await api.post(
            APIEndpoints.assignUserToNode(id: user.id),
            body: AssignmentRequest(
                expectedRevision: user.revision,
                nodeID: node.id,
                mtlsCredentialId: mtls?.id,
                mtlsCredentialVersion: mtls?.version
            )
        )
        await refresh()
        return response.hy2Credential
    }

    func setNodeAssignment(
        userID: String, nodeID: String, expectedRevision: Int, assigned: Bool,
        clientCertificate: String? = nil, clientPrivateKey: String? = nil, mtlsCredentialID: String? = nil
    ) async throws -> (revision: Int, credential: String?) {
        let api = try requireConnectedAPI()
        if assigned {
            let mtls = try await resolveMTLSCredential(userID: userID, nodeID: nodeID, certificate: clientCertificate, privateKey: clientPrivateKey, selected: mtlsCredentialID)
            let response: AssignmentReceipt = try await api.post(
                APIEndpoints.assignUserToNode(id: userID),
                body: AssignmentRequest(expectedRevision: expectedRevision, nodeID: nodeID,
                                        mtlsCredentialId: mtls?.id, mtlsCredentialVersion: mtls?.version)
            )
            return (response.revision, response.hy2Credential)
        }
        let response: AssignmentMutationResponse = try await api.delete(
            APIEndpoints.unassignUserFromNode(id: userID, nodeID: nodeID),
            body: RevisionRequest(expectedRevision: expectedRevision)
        )
        return (response.revision, nil)
    }

    func updateAssignmentClientCertificate(
        _ user: UserSummary,
        for node: NodeSummary,
        clientCertificate: String,
        clientPrivateKey: String,
        mtlsCredentialID: String? = nil
    ) async throws {
        let api = try requireConnectedAPI()
        let mtls = try await resolveMTLSCredential(userID: user.id, nodeID: node.id, certificate: clientCertificate, privateKey: clientPrivateKey, selected: mtlsCredentialID)
        let _: AssignmentMutationResponse = try await api.put(
            APIEndpoints.updateAssignmentClientCertificate(id: user.id, nodeID: node.id),
            body: AssignmentCertificateUpdateRequest(
                expectedRevision: user.revision,
                mtlsCredentialId: mtls!.id,
                mtlsCredentialVersion: mtls!.version
            )
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
        return response.autoUrl ?? response.url
    }

    func currentSubscriptionURL(for user: UserSummary, format: SubscriptionFileFormat? = nil) async throws -> String {
        let api = try requireConnectedAPI()
        let subscription: SubscriptionStatus = try await api.get(APIEndpoints.getSubscription(id: user.id))
        guard let active = subscription.active else {
            throw APIClientError.server(L10n.text("此用户尚未生成订阅地址，请先生成订阅。"))
        }
        if let format {
            return try format.subscriptionURL(autoURL: active.autoUrl, legacyURL: active.url)
        }
        return active.autoUrl ?? active.url
    }

    func subscriptionFile(for user: UserSummary, format: SubscriptionFileFormat) async throws -> Data {
        let api = try requireConnectedAPI()
        let subscription: SubscriptionStatus = try await api.get(APIEndpoints.getSubscription(id: user.id))
        guard let active = subscription.active else {
            throw APIClientError.server(L10n.text("此用户尚未生成订阅地址，请先生成订阅再导出。"))
        }
        let operation: APIOperation<NoRequest, NoResponse>
        if format == .mihomo {
            operation = APIEndpoints.downloadSubscription(token: active.token)
        } else {
            guard active.autoUrl != nil else {
                throw APIClientError.server(L10n.text("此服务端尚未提供多格式订阅，请先升级服务端。"))
            }
            operation = APIEndpoints.downloadAutomaticSubscription(token: active.token, format: format.rawValue)
        }
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
        api = nextAPI
        currentAdminTokenID = receipt.id
        UserDefaults.standard.set(receipt.id, forKey: adminTokenIDKey)
        do {
            let _: APIHealth = try await nextAPI.get(APIEndpoints.readinessCheck)
            let version: APIVersion = try await nextAPI.get(APIEndpoints.getAPIVersion)
            currentAdminTokenID = version.currentAdminTokenId
            guard version.apiVersion == "1.0.0" else {
                throw APIClientError.incompatibleAPI(version.apiVersion)
            }
            try await loadData(using: nextAPI)
            isConnected = true
            errorMessage = nil
            await (nodeNotifications ?? .shared).deliver(nodes: nodes, service: serviceAddress)
            await (nodeNotifications ?? .shared).deliver(credentials: credentials, service: serviceAddress)
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

    private func loadData(using client: APIClient) async throws {
        let generation = serviceGeneration
        async let loadedMonitoring = fetchServerMonitoring(using: client)
        async let loadedCredentials: [CredentialSummary] = client.get(APIEndpoints.listCredentials)
        async let loadedNodes: [NodeSummary] = client.get(APIEndpoints.listNodes)
        async let loadedUsers: [UserSummary] = client.get(APIEndpoints.listUsers)
        async let loadedJobs: [JobSummary] = client.get(APIEndpoints.listJobs)
        async let loadedAudit: [AuditSummary] = client.get(APIEndpoints.listAuditRecords)
        async let loadedAdminTokens: [AdminTokenSummary] = client.get(APIEndpoints.listAdminTokens)
        let tokens = try await loadedAdminTokens
        let monitoring = await loadedMonitoring
        let snapshot = try await ClientDisplaySnapshot(
            credentials: loadedCredentials,
            nodes: loadedNodes,
            users: loadedUsers,
            jobs: loadedJobs,
            auditRecords: loadedAudit,
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
        lastUpdated = snapshot.updatedAt
        if let data = try? JSONEncoder().encode(snapshot) {
            UserDefaults.standard.set(data, forKey: displaySnapshotKey)
        }
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
        credentials = snapshot.credentials ?? []
        nodes = snapshot.nodes
        users = snapshot.users
        jobs = snapshot.jobs
        auditRecords = snapshot.auditRecords
        serverMonitoring = snapshot.serverMonitoring
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
            var retryDelay = 1.0
            var lastRefresh = Date()
            while !Task.isCancelled {
                do {
                    let events = try await client.events(APIEndpoints.streamEvents, lastEventID: self.lastEventID)
                    for try await eventID in events {
                        guard !Task.isCancelled else { return }
                        UserDefaults.standard.set(eventID, forKey: "lastEventID.\(self.serviceAddress)")
                        let now = Date()
                        if now.timeIntervalSince(lastRefresh) >= 1 {
                            await self.refresh()
                            lastRefresh = now
                        }
                    }
                    retryDelay = 1
                } catch {
                    guard !Task.isCancelled else { return }
                    await self.refresh()
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
                await self?.refresh()
            }
        }
    }
}
