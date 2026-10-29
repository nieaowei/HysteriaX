import Foundation

extension ManagementStore {
    func authorizationTaskDidFinish() {
        guard let client = try? requireConnectedAPI() else { return }
        authorizationMutationDidCommit(using: client)
    }

    func retryAuthorizationJob(_ job: JobSummary) async throws -> String {
        let result: JobReceipt = try await requireConnectedAPI().post(APIEndpoints.retryJob(id: job.id), body: RevisionRequest(expectedRevision: 1))
        return result.jobId
    }

    @discardableResult
    func refreshAuthorizationGroups() async -> Bool {
        guard let client = try? requireConnectedAPI() else { return false }

        let requestID = UUID()
        let service = serviceAddress
        authorizationGroupsRequestID = requestID
        isLoadingAuthorizationGroups = true
        defer {
            if authorizationGroupsRequestID == requestID {
                isLoadingAuthorizationGroups = false
            }
        }

        do {
            let groups: AuthorizationGroupsPage = try await client.get(APIEndpoints.listAuthorizationGroups(page: 1, pageSize: 50))
            guard serviceAddress == service, authorizationGroupsRequestID == requestID, !Task.isCancelled else { return false }
            authorizationGroups = groups.items
            authorizationGroupsError = nil
            return true
        } catch {
            guard serviceAddress == service, authorizationGroupsRequestID == requestID, !Task.isCancelled else { return false }
            authorizationGroupsError = error.localizedDescription
            return false
        }
    }

    func authorizationGroupsPage(page: Int, query: String, sort: String = "name", order: String = "asc") async throws -> AuthorizationGroupsPage {
        try await requireConnectedAPI().get(APIEndpoints.listAuthorizationGroups(page: page, pageSize: 50, q: query, sort: sort, order: order), bypassCache: true)
    }

    func authorizationRelationPage(groupID: String, users: Bool, page: Int, query: String, member: Bool, sort: String, order: String, enabled: Bool?) async throws -> AuthorizationRelationPage {
        let client = try requireConnectedAPI()
        if users {
            let result = try await client.get(APIEndpoints.listAuthorizationGroupUsers(groupID: groupID, page: page, pageSize: 100, q: query, sort: sort, order: order, member: member, enabled: enabled), bypassCache: true)
            return AuthorizationRelationPage(rows: result.items.map { .init(id: $0.id, name: $0.name, enabled: $0.enabled, expiresAt: $0.expiresAt, state: nil) }, total: result.total, page: result.page, revision: result.revision)
        }
        let result = try await client.get(APIEndpoints.listAuthorizationGroupNodes(groupID: groupID, page: page, pageSize: 100, q: query, sort: sort, order: order, member: member), bypassCache: true)
        return AuthorizationRelationPage(rows: result.items.map { .init(id: $0.id, name: $0.name, enabled: nil, expiresAt: nil, state: $0.state) }, total: result.total, page: result.page, revision: result.revision)
    }

    func authorizationUserDetail(_ userID: String) async throws -> UserSummary {
        try await requireConnectedAPI().get(APIEndpoints.getUser(id: userID))
    }

    func authorizationGroupDetail(_ groupID: String) async throws -> AuthorizationGroupSummary {
        try await requireConnectedAPI().get(APIEndpoints.getAuthorizationGroup(groupID: groupID))
    }

    func userSummary(_ userID: String) async throws -> UserSummary {
        try await requireConnectedAPI().get(APIEndpoints.getUser(id: userID))
    }

    func createAuthorizationGroup(name: String) async throws -> AuthorizationChangeReceipt {
        let client = try requireConnectedAPI()
        let result: AuthorizationChangeReceipt = try await client.post(
            APIEndpoints.createAuthorizationGroup,
            body: AuthorizationGroupCreateRequest(name: name)
        )
        authorizationMutationDidCommit(using: client)
        return result
    }

    func updateAuthorizationGroup(
        _ group: AuthorizationGroupSummary,
        name: String? = nil,
        addUserIDs: [String] = [],
        removeUserIDs: [String] = [],
        addNodeIDs: [String] = [],
        removeNodeIDs: [String] = []
    ) async throws -> AuthorizationChangeReceipt {
        let client = try requireConnectedAPI()
        let result: AuthorizationChangeReceipt = try await client.patch(
            APIEndpoints.updateAuthorizationGroup(groupID: group.id),
            body: AuthorizationGroupUpdateRequest(
                expectedRevision: group.revision,
                name: name,
                addUserIds: addUserIDs.isEmpty ? nil : addUserIDs.sorted(),
                removeUserIds: removeUserIDs.isEmpty ? nil : removeUserIDs.sorted(),
                addNodeIds: addNodeIDs.isEmpty ? nil : addNodeIDs.sorted(),
                removeNodeIds: removeNodeIDs.isEmpty ? nil : removeNodeIDs.sorted()
            )
        )
        authorizationMutationDidCommit(using: client)
        return result
    }

    func deleteAuthorizationGroup(
        _ group: AuthorizationGroupSummary
    ) async throws -> AuthorizationChangeReceipt {
        let client = try requireConnectedAPI()
        let result: AuthorizationChangeReceipt = try await client.delete(
            APIEndpoints.deleteAuthorizationGroup(groupID: group.id),
            body: AuthorizationGroupDeleteRequest(expectedRevision: group.revision)
        )
        authorizationMutationDidCommit(using: client)
        return result
    }

    func updateUserAuthorizationGroups(
        _ user: UserSummary,
        groupIDs: [String]
    ) async throws -> AuthorizationChangeReceipt {
        let client = try requireConnectedAPI()
        let result: AuthorizationChangeReceipt = try await client.put(
            APIEndpoints.updateUserAuthorizationGroups(id: user.id),
            body: UserAuthorizationGroupsUpdateRequest(
                expectedRevision: user.revision,
                groupIds: groupIDs
            )
        )
        authorizationMutationDidCommit(using: client)
        return result
    }
}
