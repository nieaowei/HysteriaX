import Foundation

struct ServerConfigurationDraft {
    var name = ""
    var sshHost = ""
    var sshPort = "22"
    var sshUsername = "root"
    var sshCredentialId = ""
    var sshCredentialVersion = 1

    init(_ detail: NodeDetail? = nil) {
        guard let detail else { return }
        name = detail.name
        sshHost = detail.ssh.host
        sshPort = String(detail.ssh.port)
        sshUsername = detail.ssh.username
        sshCredentialId = detail.ssh.credentialId
        sshCredentialVersion = detail.ssh.credentialVersion
    }

    func request(revision: Int, originalAuthType: String) throws -> NodePatchRequest {
        guard !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              !sshHost.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              !sshUsername.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              let port = Int(sshPort), (1...65535).contains(port) else {
            throw APIClientError.server(L10n.text("请填写服务器名称、SSH 地址和用户；端口须在 1 到 65535 之间。"))
        }
        guard !sshCredentialId.isEmpty else { throw APIClientError.server(L10n.text("请选择 SSH 凭据。")) }
        return NodePatchRequest(
            expectedRevision: revision, name: name.trimmingCharacters(in: .whitespacesAndNewlines),
            sshHost: sshHost.trimmingCharacters(in: .whitespacesAndNewlines), sshPort: port,
            sshUsername: sshUsername.trimmingCharacters(in: .whitespacesAndNewlines),
            sshCredentialId: sshCredentialId, sshCredentialVersion: sshCredentialVersion
        )
    }
}
