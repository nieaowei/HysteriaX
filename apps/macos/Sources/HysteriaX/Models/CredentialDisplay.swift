import Foundation

extension CredentialSummary {
    var isManaged: Bool { !id.contains(":") }
    var typeTitle: String { CredentialDisplay.kind(kind) }
    var statusTitle: String { CredentialDisplay.status(status) }
    func reference(_ field: String) -> String { "credential://\(id)/\(latestVersion)/\(field)" }
}

enum CredentialDisplay {
    static func kind(_ value: String) -> String {
        switch value {
        case "ssh_private_key": "SSH 私钥"
        case "ssh_password": "SSH 密码"
        case "tls_identity": "TLS 证书对"
        case "ca_certificate": "CA 证书"
        case "ech_key": "ECH 密钥"
        case "dns": "ACME DNS 凭据"
        case "api_token": "API Token"
        case "admin_token": "管理员 Token"
        case "subscription_token": "订阅 Token"
        case "user_credential": "用户连接凭据"
        default: value
        }
    }
    static func source(_ value: String) -> String {
        switch value {
        case "desired": "目标配置"
        case "deployed": "已部署配置"
        case "history": "历史配置"
        case "ssh": "SSH 访问"
        case "mtls": "mTLS 身份"
        case "batch": "更新任务"
        default: value
        }
    }
    static func status(_ value: String) -> String {
        switch value {
        case "active": "有效"
        case "archived": "已归档"
        case "expiring": "即将到期"
        case "expired": "已到期"
        case "revoked": "已撤销"
        case "disabled": "已停用"
        case "quota_exhausted": "额度耗尽"
        default: value
        }
    }
}


/// Keep pinned and archived material visible when a configuration still uses it.
/// Only active latest versions are offered as new choices.
enum CredentialResources {
    static func catalog(_ entries: [CredentialSummary], configuration: [String: JSONValue]) -> [NodeResource] {
        var current = Set<String>()
        func visit(_ value: JSONValue) {
            switch value {
            case .string(let uri) where uri.hasPrefix("credential://"): current.insert(uri)
            case .object(let map): map.values.forEach(visit)
            case .array(let values): values.forEach(visit)
            default: break
            }
        }
        visit(.object(configuration))
        var output: [NodeResource] = []
        for entry in entries where entry.isManaged && entry.ownerUserId == nil {
            let fields: [(String, String)]
            switch entry.kind {
            case "tls_identity": fields = [("tls_identity", "certificate")]
            case "ca_certificate", "ech_key": fields = [(entry.kind, "content")]
            default: fields = []
            }
            for (kind, field) in fields {
                var references = current.filter { uri in
                    let parts = uri.dropFirst("credential://".count).split(separator: "/")
                    return parts.count == 3 && parts[0] == entry.id && parts[2] == field
                }
                if !entry.archived { references.insert(entry.reference(field)) }
                for reference in references.sorted() {
                    let parts = reference.dropFirst("credential://".count).split(separator: "/")
                    let version = Int(parts[1]) ?? entry.latestVersion
                    output.append(NodeResource(id: "\(entry.id):\(version):\(field)",
                        name: "\(entry.name) · v\(version)\(entry.archived ? "（已归档，当前使用）" : "")",
                        resourceKind: kind, contentSHA256: version == entry.latestVersion ? (entry.metadata["fingerprint"]?.stringValue ?? "") : "",
                        sizeBytes: 0, createdAt: entry.createdAt, reference: reference))
                }
            }
        }
        return output
    }
}
