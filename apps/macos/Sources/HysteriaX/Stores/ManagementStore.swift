import Foundation
import Observation

private struct ClientDisplaySnapshot: Codable {
    let nodes: [NodeSummary]
    let users: [UserSummary]
    let jobs: [JobSummary]
    let auditRecords: [AuditSummary]
    let updatedAt: Date
}

@MainActor
@Observable
final class ManagementStore {
    var serviceAddress = UserDefaults.standard.string(forKey: "serviceAddress") ?? ""
    var nodes: [NodeSummary] = []
    var users: [UserSummary] = []
    var jobs: [JobSummary] = []
    var auditRecords: [AuditSummary] = []
    var adminTokens: [AdminTokenSummary] = []
    var currentAdminTokenID: String?
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

    init() {
        restoreDisplaySnapshot()
        currentAdminTokenID = UserDefaults.standard.string(forKey: adminTokenIDKey)
        if let token = KeychainStore.readToken(), let url = URL(string: serviceAddress), url.scheme == "https" {
            let client = APIClient(baseURL: url, token: token)
            api = client
            Task {
                await refresh()
                startEventUpdates(using: client)
            }
        }
    }

    func connect(serviceAddress: String, token: String) async throws {
        guard let url = URL(string: serviceAddress.trimmingCharacters(in: .whitespacesAndNewlines)),
              url.scheme == "https", url.host != nil else { throw APIClientError.invalidBaseURL }
        let previousToken = KeychainStore.readToken()
        eventTask?.cancel()
        refreshTask?.cancel()
        let client = APIClient(baseURL: url, token: token)
        let _: APIHealth = try await client.get(APIEndpoints.readinessCheck)
        let version: APIVersion = try await client.get(APIEndpoints.getAPIVersion)
        guard version.apiVersion == "1.0.0" else { throw APIClientError.incompatibleAPI(version.apiVersion) }
        let normalizedAddress = url.absoluteString.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        let serviceChanged = normalizedAddress != self.serviceAddress
        let tokenChanged = previousToken != token
        try KeychainStore.saveToken(token)
        if serviceChanged {
            nodes = []
            users = []
            jobs = []
            auditRecords = []
            adminTokens = []
            lastUpdated = nil
            currentAdminTokenID = UserDefaults.standard.string(forKey: "currentAdminTokenID.\(normalizedAddress)")
        }
        if tokenChanged {
            currentAdminTokenID = nil
            UserDefaults.standard.removeObject(forKey: "currentAdminTokenID.\(normalizedAddress)")
        }
        self.serviceAddress = normalizedAddress
        UserDefaults.standard.set(self.serviceAddress, forKey: "serviceAddress")
        api = client
        isConnected = false
        restoreDisplaySnapshot()
        do {
            try await loadData(using: client)
            isConnected = true
            errorMessage = nil
        } catch {
            isConnected = false
            errorMessage = error.localizedDescription
            throw error
        }
        startEventUpdates(using: client)
    }

    func refresh() async {
        guard let api else { isConnected = false; return }
        isLoading = true
        defer { isLoading = false }
        do {
            let version: APIVersion = try await api.get(APIEndpoints.getAPIVersion)
            guard version.apiVersion == "1.0.0" else { throw APIClientError.incompatibleAPI(version.apiVersion) }
            try await loadData(using: api)
            isConnected = true
            errorMessage = nil
        } catch {
            isConnected = false
            errorMessage = error.localizedDescription
        }
    }

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

    func runNodeAction(_ node: NodeSummary, action: String) async throws {
        let api = try requireConnectedAPI()
        let operation: APIOperation<RevisionRequest, JobReceipt>
        switch action {
        case "ssh-test": operation = APIEndpoints.testNodeSSH(id: node.id)
        case "deploy": operation = APIEndpoints.deployNode(id: node.id)
        case "sync": operation = APIEndpoints.syncNode(id: node.id)
        case "rollback": operation = APIEndpoints.rollbackNode(id: node.id)
        default: throw APIClientError.server("未知的节点操作。")
        }
        let _: JobReceipt = try await api.post(operation, body: RevisionRequest(expectedRevision: node.revision))
        await refresh()
    }

    func deleteNode(_ node: NodeSummary) async throws {
        let api = try requireConnectedAPI()
        _ = try await api.delete(APIEndpoints.deleteNode(id: node.id, expectedRevision: node.revision))
        await refresh()
    }

    func nodeDetail(_ nodeID: String) async throws -> NodeDetail {
        let api = try requireConnectedAPI()
        return try await api.get(APIEndpoints.getNode(id: nodeID))
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
        proxyProbeURL: String
    ) async throws {
        let api = try requireConnectedAPI()
        let _: NodeUpdateResponse = try await api.patch(
            APIEndpoints.updateNode(id: detail.id),
            body: NodePatchRequest(
                expectedRevision: detail.revision,
                listenAddr: listenAddress,
                proxyProbeUrl: proxyProbeURL,
                config: config.objectValue ?? [:]
            )
        )
        await refresh()
    }

    func nodeResources(_ nodeID: String) async throws -> [NodeResource] {
        let api = try requireConnectedAPI()
        return try await api.get(APIEndpoints.listNodeResources(id: nodeID))
    }

    func uploadResource(nodeID: String, name: String, kind: String, data: Data) async throws -> ResourceReceipt {
        let api = try requireConnectedAPI()
        let request = ResourceUploadRequest(name: name, resourceKind: kind, contentBase64: data.base64EncodedString())
        return try await api.post(APIEndpoints.uploadNodeResource(id: nodeID), body: request)
    }

    func confirmHostFingerprint(nodeID: String, fingerprint: String) async throws {
        guard let node = nodes.first(where: { $0.id == nodeID }) else {
            throw APIClientError.server("找不到对应节点。")
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
        clientPrivateKey: String? = nil
    ) async throws -> String {
        let api = try requireConnectedAPI()
        let response: AssignmentReceipt = try await api.post(
            APIEndpoints.assignUserToNode(id: user.id),
            body: AssignmentRequest(
                expectedRevision: user.revision,
                nodeID: node.id,
                clientCertificate: clientCertificate,
                clientPrivateKey: clientPrivateKey
            )
        )
        await refresh()
        return response.hy2Credential
    }

    func updateAssignmentClientCertificate(
        _ user: UserSummary,
        for node: NodeSummary,
        clientCertificate: String,
        clientPrivateKey: String
    ) async throws {
        let api = try requireConnectedAPI()
        let _: AssignmentMutationResponse = try await api.put(
            APIEndpoints.updateAssignmentClientCertificate(id: user.id, nodeID: node.id),
            body: AssignmentCertificateUpdateRequest(
                expectedRevision: user.revision,
                clientCertificate: clientCertificate,
                clientPrivateKey: clientPrivateKey
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
        return response.url
    }

    func currentSubscriptionURL(for user: UserSummary) async throws -> String {
        let api = try requireConnectedAPI()
        let subscription: SubscriptionStatus = try await api.get(APIEndpoints.getSubscription(id: user.id))
        guard let active = subscription.active else {
            throw APIClientError.server("此用户尚未生成订阅地址，请先生成订阅。")
        }
        return active.url
    }

    func subscriptionYAML(for user: UserSummary) async throws -> Data {
        let api = try requireConnectedAPI()
        let subscription: SubscriptionStatus = try await api.get(APIEndpoints.getSubscription(id: user.id))
        guard let active = subscription.active else {
            throw APIClientError.server("此用户尚未生成订阅地址，请先生成订阅再导出。")
        }
        return try await api.download(APIEndpoints.downloadSubscription(token: active.token))
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
            throw APIClientError.server("令牌用途需为 1 到 100 个字符。")
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
            errorMessage = "令牌已创建，但无法保存到 Keychain；请立即复制并安全保存此令牌。\(error.localizedDescription)"
            return receipt
        }
        api = nextAPI
        currentAdminTokenID = receipt.id
        UserDefaults.standard.set(receipt.id, forKey: adminTokenIDKey)
        do {
            let _: APIHealth = try await nextAPI.get(APIEndpoints.readinessCheck)
            let version: APIVersion = try await nextAPI.get(APIEndpoints.getAPIVersion)
            guard version.apiVersion == "1.0.0" else {
                throw APIClientError.incompatibleAPI(version.apiVersion)
            }
            try await loadData(using: nextAPI)
            isConnected = true
            errorMessage = nil
        } catch {
            isConnected = false
            errorMessage = "新令牌已保存并切换，但服务暂不可用：\(error.localizedDescription)"
        }
        startEventUpdates(using: nextAPI)
        return receipt
    }

    func revokeAdminToken(_ token: AdminTokenSummary) async throws {
        guard let currentAdminTokenID else {
            throw APIClientError.server("先创建并切换到本 Mac 的新令牌，再撤销旧令牌。")
        }
        guard token.id != currentAdminTokenID else {
            throw APIClientError.server("不能撤销本 Mac 当前使用的令牌。")
        }
        let api = try requireConnectedAPI()
        try await api.deleteNoContent(APIEndpoints.revokeAdminToken(id: token.id))
        await refresh()
    }

    private func loadData(using client: APIClient) async throws {
        async let loadedNodes: [NodeSummary] = client.get(APIEndpoints.listNodes)
        async let loadedUsers: [UserSummary] = client.get(APIEndpoints.listUsers)
        async let loadedJobs: [JobSummary] = client.get(APIEndpoints.listJobs)
        async let loadedAudit: [AuditSummary] = client.get(APIEndpoints.listAuditRecords)
        async let loadedAdminTokens: [AdminTokenSummary] = client.get(APIEndpoints.listAdminTokens)
        let tokens = try await loadedAdminTokens
        let snapshot = try await ClientDisplaySnapshot(
            nodes: loadedNodes,
            users: loadedUsers,
            jobs: loadedJobs,
            auditRecords: loadedAudit,
            updatedAt: Date()
        )
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

    private var displaySnapshotKey: String { "displaySnapshot.\(serviceAddress)" }
    private var adminTokenIDKey: String { "currentAdminTokenID.\(serviceAddress)" }

    private func restoreDisplaySnapshot() {
        guard let data = UserDefaults.standard.data(forKey: displaySnapshotKey),
              let snapshot = try? JSONDecoder().decode(ClientDisplaySnapshot.self, from: data) else { return }
        nodes = snapshot.nodes
        users = snapshot.users
        jobs = snapshot.jobs
        auditRecords = snapshot.auditRecords
        lastUpdated = snapshot.updatedAt
    }

    private func requireConnectedAPI() throws -> APIClient {
        guard isConnected, let api else {
            throw APIClientError.server("管理服务当前不可用；恢复连接后才能读取详情或提交写操作。")
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
