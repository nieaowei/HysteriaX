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
        case "ssh_private_key": L10n.text("SSH 私钥")
        case "ssh_password": L10n.text("SSH 密码")
        case "tls_identity": L10n.text("TLS 证书对")
        case "ca_certificate": L10n.text("CA 证书")
        case "ech_key": L10n.text("ECH 密钥")
        case "dns": L10n.text("ACME DNS 凭据")
        case "api_token": "API Token"
        case "admin_token": L10n.text("管理员 Token")
        case "subscription_token": L10n.text("订阅 Token")
        case "user_credential": L10n.text("用户连接凭据")
        default: value
        }
    }
    static func source(_ value: String) -> String {
        switch value {
        case "desired": L10n.text("目标配置")
        case "deployed": L10n.text("已部署配置")
        case "history": L10n.text("历史配置")
        case "ssh": L10n.text("SSH 访问")
        case "mtls": L10n.text("mTLS 身份")
        case "batch": L10n.text("更新任务")
        default: value
        }
    }
    static func status(_ value: String) -> String {
        switch value {
        case "active": L10n.text("有效")
        case "archived": L10n.text("已归档")
        case "expiring": L10n.text("即将到期")
        case "expired": L10n.text("已到期")
        case "revoked": L10n.text("已撤销")
        case "disabled": L10n.text("已停用")
        case "quota_exhausted": L10n.text("额度耗尽")
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
                        name: "\(entry.name) · v\(version)\(entry.archived ? L10n.text("（已归档，当前使用）") : "")",
                        resourceKind: kind, contentSHA256: version == entry.latestVersion ? (entry.metadata["fingerprint"]?.stringValue ?? "") : "",
                        sizeBytes: 0, createdAt: entry.createdAt, reference: reference))
                }
            }
        }
        return output
    }
}
