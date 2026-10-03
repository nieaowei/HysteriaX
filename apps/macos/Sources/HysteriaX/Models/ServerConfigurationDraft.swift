import Foundation

struct ServerConfigurationDraft {
    var name = ""
    var sshHost = ""
    var sshPort = "22"
    var sshUsername = "root"
    var sshAuthType = "password"
    var sshSecret = ""
    var sshPassphrase = ""
    var clearPassphrase = false

    init(_ detail: NodeDetail? = nil) {
        guard let detail else { return }
        name = detail.name
        sshHost = detail.ssh.host
        sshPort = String(detail.ssh.port)
        sshUsername = detail.ssh.username
        sshAuthType = detail.ssh.authType
    }

    func request(revision: Int, originalAuthType: String) throws -> NodePatchRequest {
        guard !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              !sshHost.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              !sshUsername.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              let port = Int(sshPort), (1...65535).contains(port) else {
            throw APIClientError.server("请填写服务器名称、SSH 地址和用户；端口须在 1 到 65535 之间。")
        }
        guard originalAuthType == sshAuthType || !sshSecret.isEmpty else {
            throw APIClientError.server("切换 SSH 认证方式时，请填写新密码或私钥。")
        }
        return NodePatchRequest(
            expectedRevision: revision, name: name.trimmingCharacters(in: .whitespacesAndNewlines),
            sshHost: sshHost.trimmingCharacters(in: .whitespacesAndNewlines), sshPort: port,
            sshUsername: sshUsername.trimmingCharacters(in: .whitespacesAndNewlines), sshAuthType: sshAuthType,
            sshSecret: sshSecret.isEmpty ? nil : sshSecret,
            sshPassphrase: clearPassphrase ? "" : (sshPassphrase.isEmpty ? nil : sshPassphrase)
        )
    }
}
