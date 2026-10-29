import Foundation

extension ManagementStore {
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
            let groups: [AuthorizationGroupSummary] = try await client.get(APIEndpoints.listAuthorizationGroups)
            guard serviceAddress == service, authorizationGroupsRequestID == requestID, !Task.isCancelled else { return false }
            authorizationGroups = groups
            authorizationGroupsError = nil
            return true
        } catch {
            guard serviceAddress == service, authorizationGroupsRequestID == requestID, !Task.isCancelled else { return false }
            authorizationGroupsError = error.localizedDescription
            return false
        }
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

    func createAuthorizationGroup(name: String) async throws -> AuthorizationGroupMutationResponse {
        let client = try requireConnectedAPI()
        let result: AuthorizationGroupMutationResponse = try await client.post(
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
    ) async throws -> AuthorizationGroupMutationResponse {
        let client = try requireConnectedAPI()
        let result: AuthorizationGroupMutationResponse = try await client.patch(
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
    ) async throws -> AuthorizationGroupMutationResponse {
        let client = try requireConnectedAPI()
        let result: AuthorizationGroupMutationResponse = try await client.delete(
            APIEndpoints.deleteAuthorizationGroup(groupID: group.id),
            body: AuthorizationGroupDeleteRequest(expectedRevision: group.revision)
        )
        authorizationMutationDidCommit(using: client, deletedGroupID: group.id)
        return result
    }

    func updateUserAuthorizationGroups(
        _ user: UserSummary,
        groupIDs: [String]
    ) async throws -> UserAuthorizationGroupsMutationResponse {
        let client = try requireConnectedAPI()
        let result: UserAuthorizationGroupsMutationResponse = try await client.put(
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
