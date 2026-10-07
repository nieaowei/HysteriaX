import Foundation

enum JobDisplayText {
    static func kind(_ value: String) -> String {
        switch value {
        case "dns-verify": "验证 DNS 连接"
        case "dns-connection-refresh": "刷新 DNS 域名"
        case "dns-zone-refresh": "刷新 DNS 记录"
        case "dns-record-create": "创建 DNS 记录"
        case "dns-record-update": "修改 DNS 记录"
        case "dns-record-delete": "删除 DNS 记录"
        case "dns-record-check": "验证 DNS 解析"
        case "dns-credential-apply": "更新 DNS 连接凭据"
        case "ssh-test": "SSH 测试"
        case "credential-apply": "应用凭据"
        case "deploy": "部署"
        case "sync": "同步"
        case "rollback": "回滚"
        case "kick": "断开客户端"
        case "uninstall": "卸载"
        default: value
        }
    }

    static func status(_ value: String) -> String {
        switch value {
        case "queued": "排队中"
        case "running": "执行中"
        case "succeeded": "成功"
        case "failed": "失败"
        case "rolled_back": "已回滚"
        case "cancelled": "已取消"
        default: value
        }
    }

    static func stage(_ value: String) -> String {
        let labels = [
            "credential_applied": "凭据已应用",
            "credential_deployment_queued": "凭据部署已排队",
            "credential_revocation_queued": "连接撤销已排队",
            "queued": "排队中",
            "running": "执行中",
            "starting": "准备中",
            "retry_wait": "等待重试",
            "waiting_recovery": "等待节点 SSH 恢复",
            "needs_attention": "需要人工处理",
            "restriction_cleared": "限制已解除，跳过断开",
            "recovered": "重启后恢复",
            "loading_connection": "读取 SSH 连接",
            "connecting": "连接节点",
            "checking_environment": "检查节点环境",
            "loading_revision": "读取配置版本",
            "resolving_resources": "检查配置资源",
            "checking_drift": "检查配置漂移",
            "downloading_release": "下载并校验程序",
            "rendering_configuration": "生成服务配置",
            "uploading_files": "上传部署文件",
            "installing_service": "安装系统服务",
            "checking_health": "检查服务健康状态",
            "checking_proxy_traffic": "检查客户端代理转发",
            "health_checked": "健康检查通过",
            "rolling_back": "恢复上一版本",
            "rolled_back": "已回滚",
            "rollback_failed": "回滚失败",
            "fingerprint_confirmation_required": "等待确认 SSH 指纹",
            "environment_checked": "环境检查通过",
            "kicking_clients": "断开客户端",
            "checking_clients": "检查客户端状态",
            "clients_offline": "客户端已下线",
            "uninstalling_service": "卸载系统服务",
            "remote_uninstalled": "远端卸载完成",
            "node_deleting": "删除节点中",
            "node_removed": "管理记录已移除",
            "superseded": "已被新任务替代",
            "dns_completed": "DNS 操作完成",
            "dns_propagation_wait": "等待 DNS 解析更新",
            "failed": "失败",
        ]
        return labels[value] ?? value
    }

    static func logMessage(stage: String, message: String) -> String {
        if stage == "checking_clients",
           let captures = captures(#"^(\d+) client device\(s\) remain online; requesting another kick\.$"#, in: message) {
            return "还有 \(captures[0]) 台设备在线，正在再次请求断开。"
        }
        if let captures = captures(#"^Waiting up to (\d+) seconds for the traffic and online statistics APIs, including certificate provisioning\.$"#, in: message) {
            return "正在等待流量统计和在线状态接口就绪（包括证书签发），最长等待 \(captures[0]) 秒。"
        }
        let messages = [
            "Connecting to the node and verifying its pinned SSH host key.": "正在连接节点并验证已保存的 SSH 主机指纹。",
            "Running the pinned Hysteria client through the new server and forwarding a TCP request.": "正在使用固定版本的 Hysteria 客户端连接新服务并验证 TCP 请求转发。",
            "Requesting that the node disconnect the user's active client devices.": "正在请求节点断开该用户的在线客户端。",
            "The new service did not become healthy; restoring the previous configuration.": "新服务未通过健康检查，正在恢复上一配置。",
            "The new service did not become healthy; cleaning up the failed first installation.": "新服务未通过健康检查，正在清理失败的首次安装。",
            "The new service did not become healthy; rolling back the installation.": "新服务未通过健康检查，正在回滚安装。",
            "Loading the saved SSH connection.": "正在读取保存的 SSH 连接。",
            "Connecting to the node and checking its SSH host key.": "正在连接节点并验证 SSH 主机指纹。",
            "Checking the operating system, architecture, systemd, and sudo access.": "正在检查操作系统、架构、systemd 和 sudo 权限。",
            "Loading the target configuration revision and encrypted resources.": "正在加载目标配置版本和加密资源。",
            "Resolving and validating the configuration resource references.": "正在解析并验证配置资源引用。",
            "Checking the operating system, architecture, systemd, sudo access, disk space, and listener requirements.": "正在检查系统版本、架构、systemd、sudo、磁盘空间和监听端口要求。",
            "Comparing the remote configuration with the last successfully deployed version.": "正在将远端配置与上次成功部署版本比较。",
            "Downloading and verifying the pinned Hysteria release asset.": "正在下载并校验固定版本的 Hysteria 程序。",
            "Rendering the server configuration and systemd unit.": "正在生成服务端配置和 systemd 单元文件。",
            "Uploading the verified binary, configuration, unit, and referenced resources.": "正在上传已校验程序、配置、服务文件和引用资源。",
            "Installing the managed systemd service and applying the new configuration.": "正在安装托管的 systemd 服务并应用配置。",
            "Waiting for the traffic and online statistics APIs to become healthy.": "正在等待流量统计和在线状态接口就绪。",
            "The new service did not become healthy; restoring the previous successful configuration.": "新服务未通过健康检查，正在恢复上一成功配置。",
            "Requesting that the node disconnect the user's active client sessions.": "正在请求节点断开该用户的在线客户端。",
            "The node reports no active client devices for this user.": "节点已确认该用户没有在线客户端。",
            "Connecting to the node and verifying HysteriaX ownership before removal.": "正在连接节点并验证 HysteriaX 所有权。",
            "Checking remote systemd and the managed-install marker.": "正在检查远端 systemd 和托管安装标记。",
            "Stopping the managed service and removing its files and service account.": "正在停止托管服务并移除文件和服务账户。",
        ]
        return messages[message] ?? message
    }

    // Translate known contexts while retaining unknown library errors and remote output.
    static func errorMessage(_ message: String) -> String {
        if let translated = errors[message] { return translated }
        if let values = captures(#"^remote command failed with exit code (\d+): ([\s\S]*)$"#, in: message) {
            return "远端命令执行失败（退出码 \(values[0])）：\(errorMessage(values[1]))"
        }
        if let values = captures(#"^SSH host key changed: expected ([^;]+); observed (.+)$"#, in: message) {
            return "SSH 主机指纹已变化：预期 \(values[0])；实际 \(values[1])"
        }
        if let values = captures(#"^confirm this SSH host fingerprint before (deploying|uninstalling|kicking clients): (.+)$"#, in: message) {
            let action = ["deploying": "部署", "uninstalling": "卸载", "kicking clients": "断开客户端"][values[0]] ?? "操作"
            return "请先确认 SSH 主机指纹再\(action)：\(values[1])"
        }
        if let values = captures(#"^(\d+) client device\(s\) remain online after repeated Hysteria kick requests$"#, in: message) {
            return "多次请求断开后，仍有 \(values[0]) 台客户端设备在线。"
        }
        if let values = captures(#"^DNS provider returned HTTP (\d+)$"#, in: message) {
            return "DNS 服务商返回 HTTP \(values[0])"
        }
        if let values = captures(#"^post-deployment health check failed \(([\s\S]*)\)(; | and )([\s\S]*)$"#, in: message) {
            return "部署后健康检查失败（\(errorMessage(values[0]))）；\(errorMessage(values[2]))"
        }
        // anyhow joins error contexts with ': '; only translate a recognized prefix.
        for prefix in errors.keys.sorted(by: { $0.count > $1.count }) {
            if message.hasPrefix(prefix + ": ") {
                return errors[prefix]! + "：" + errorMessage(String(message.dropFirst(prefix.count + 2)))
            }
        }
        for (prefix, translated) in [
            ("remote diagnostics: ", "远端诊断："),
            ("the pinned Hysteria client did not establish an authenticated proxy session: ", "固定版本的 Hysteria 客户端未建立已认证的代理连接："),
        ] {
            if message.hasPrefix(prefix) {
                return translated + errorMessage(String(message.dropFirst(prefix.count)))
            }
        }
        if let values = captures(#"^startup health check timed out after ([\d.]+)s \(budget (\d+)s\); last error: ([\s\S]*)$"#, in: message) {
            return "启动健康检查超时，已等待 \(values[0]) 秒（最长 \(values[1]) 秒）；最后错误：\(errorMessage(values[2]))"
        }
        if let values = captures(#"^([\s\S]+); waited ([\d.]+)s$"#, in: message) {
            return "\(errorMessage(values[0]))；已等待 \(values[1]) 秒"
        }
        if let values = captures(#"^Hysteria service failed during startup: ActiveState=([^,]+), SubState=([^,]+), ExecMainStatus=(\d+), NRestarts=(\d+)$"#, in: message) {
            return "Hysteria 服务启动失败：活动状态=\(values[0])，子状态=\(values[1])，退出码=\(values[2])，重启次数=\(values[3])"
        }
        if let range = message.range(of: "; remote diagnostics: ") {
            return errorMessage(String(message[..<range.lowerBound])) + "；远端诊断：" + String(message[range.upperBound...])
        }
        return message
    }

    private static func captures(_ pattern: String, in value: String) -> [String]? {
        guard let expression = try? NSRegularExpression(pattern: pattern),
              let match = expression.firstMatch(in: value, range: NSRange(value.startIndex..., in: value)) else { return nil }
        return (1..<match.numberOfRanges).map { index in
            guard let range = Range(match.range(at: index), in: value) else { return "" }
            return String(value[range])
        }
    }

    private static let errors = [
        "node changed before SSH credential update; retry after reviewing current configuration": "节点在更新 SSH 凭据前已发生变化，请检查当前配置后重试",
        "node changed before credential update; retry after reviewing current configuration": "节点在更新凭据前已发生变化，请检查当前配置后重试",
        "invalid SSH credential": "SSH 凭据无效",
        "user-owned credentials cannot be deployed to nodes": "用户专属凭据不能部署到节点",
        "mTLS credential is not owned by this user": "mTLS 凭据不属于此用户",
        "user changed before mTLS credential update": "用户在更新 mTLS 凭据前已发生变化",
        "user assignment no longer references this credential": "用户分配已不再引用此凭据",
        "unknown credential update mode": "未知的凭据更新模式",
        "credential update superseded or archived": "凭据更新已被替代或凭据已归档",
        "credential batch item was replaced by a retry": "凭据批次项已被重试任务替代",
        "credential update superseded or archived; this target was not applied": "凭据更新已被替代或凭据已归档，未应用到此目标",
        "credential batch item is missing": "缺少凭据批次项",
        "node is missing or being deleted": "节点不存在或正在删除",
        "DNS credential publication was superseded or archived": "DNS 凭据发布已被替代或凭据已归档",
        "DNS credential publication was superseded": "DNS 凭据发布已被替代",
        "DNS connection changed while verifying credentials; retry the latest batch": "DNS 连接在验证凭据期间已发生变化，请重试最新批次",
        "record name conflicts with an existing remote record; refresh and select it explicitly": "记录名称与已有远端记录冲突，请刷新并明确选择该记录",
        "a Cloudflare DNS credential is required": "需要 Cloudflare DNS 凭据",
        "Cloudflare token is missing": "缺少 Cloudflare 令牌",
        "remote Hysteria configuration changed outside HysteriaX; inspect the difference, then start an explicit sync to apply the desired revision": "远端 Hysteria 配置已被外部修改，请检查差异后手动同步以应用目标版本",
        "statistics APIs have not become ready": "统计接口尚未就绪",
        "startup check timed out; last error": "启动检查超时，最后错误",
        "missing credential batch": "缺少凭据批次",
        "missing credential ID": "缺少凭据 ID",
        "missing credential version": "缺少凭据版本",
        "missing node": "缺少节点",
        "missing SSH secret": "缺少 SSH 密钥或密码",
        "missing DNS configuration": "缺少 DNS 配置",
        "node DNS reference is missing": "缺少节点 DNS 引用",
        "missing provider zone ID": "缺少服务商域名 ID",
        "missing provider zone name": "缺少服务商域名名称",
        "missing provider record ID": "缺少服务商记录 ID",
        "provider record ID is missing": "缺少服务商记录 ID",
        "missing record name": "缺少记录名称",
        "missing record type": "缺少记录类型",
        "missing written provider record ID": "写入结果缺少服务商记录 ID",
        "missing written name": "写入结果缺少名称",
        "missing written type": "写入结果缺少类型",
        "missing written content": "写入结果缺少内容",

        "SSH authentication failed; check the username and credential": "SSH 认证失败，请检查用户名和凭据",
        "SSH connection failed": "SSH 连接失败",
        "SSH connection timed out": "SSH 连接超时",
        "SSH password authentication timed out": "SSH 密码认证超时",
        "SSH public-key authentication timed out": "SSH 公钥认证超时",
        "SSH server did not provide a host key": "SSH 服务端未提供主机密钥",
        "decode SSH private key; check its format and passphrase": "无法解析 SSH 私钥，请检查格式和口令",
        "DNS provider transport failure": "无法连接 DNS 服务商",
        "DNS provider returned an invalid response": "DNS 服务商返回了无效响应",
        "DNS provider result is not a list": "DNS 服务商返回的结果不是列表",
        "DNS propagation check exceeded ten minutes; check resolution again": "DNS 解析检查已超过十分钟，请重新检查解析",
        "DNS connection changed before verification": "DNS 连接在验证前已发生变化",
        "created record changed externally; review before retrying": "新建记录已被外部修改，请检查后重试",
        "record was removed externally; refresh before retrying": "记录已被外部删除，请刷新后重试",
        "record changed externally; refresh and review the differences": "记录已被外部修改，请刷新并检查差异",
        "unsupported DNS operation": "不支持的 DNS 操作",
        "missing DNS operation": "缺少 DNS 操作信息",
        "confirm the node SSH host fingerprint before applying credentials": "请先确认节点 SSH 主机指纹再应用凭据",
        "SSH host key changed; credential was not applied": "SSH 主机指纹已变化，未应用凭据",
        "SSH verification failed; previous binding was retained, but its remote credential may already be invalid": "SSH 验证失败，已保留原绑定，但远端凭据可能已失效",
        "node changed during SSH verification; credential was not applied": "节点在 SSH 验证期间已发生变化，未应用凭据",
        "node no longer references this credential": "节点已不再引用此凭据",
        "credential configuration could not be resolved or validated": "无法解析或验证凭据配置",
        "new credential produces an invalid node configuration": "新凭据产生了无效的节点配置",
        "node changed while applying credential; retry after reviewing current configuration": "节点在应用凭据期间已发生变化，请检查当前配置后重试",
        "credential deployment superseded; retry the latest credential batch": "凭据部署已被新任务替代，请重试最新的凭据批次",
        "Kick interrupted after exhausting its execution budget": "断开客户端任务在执行次数耗尽后中断",
        "node was deleted before deployment": "节点在部署前已被删除",
        "node not found": "找不到节点",
        "target configuration version is missing": "目标配置版本不存在",
        "stored deployment snapshot is invalid": "保存的部署快照无效",
        "unsupported node architecture": "不支持的节点架构",
        "remote operating system is outside the supported deployment matrix": "远端操作系统不在支持的部署范围内",
        "HYSTERIAX_PUBLIC_URL must be configured before installing nodes": "安装节点前必须配置 HYSTERIAX_PUBLIC_URL",
        "official Hysteria asset has an unexpected size": "官方 Hysteria 程序大小与预期不符",
        "official Hysteria asset SHA-256 did not match the pinned release digest": "官方 Hysteria 程序的 SHA-256 与固定版本校验值不符",
        "traffic and online endpoints did not become ready": "流量统计和在线状态接口未就绪",
        "remote service diagnostics unavailable": "无法获取远端服务诊断信息",
        "automatic rollback also failed": "自动回滚也失败了",
        "previous configuration was restored": "已恢复上一配置",
        "failed first installation was cleaned up": "已清理失败的首次安装",
        "installation was rolled back": "已回滚安装",
        "mTLS deployment probe requires an assigned client certificate and private key": "mTLS 部署探测需要已分配的客户端证书和私钥",
        "the Hysteria client probe did not start cleanly": "Hysteria 客户端探测未能正常启动",
        "Hysteria online API returned invalid JSON": "Hysteria 在线状态接口返回了无效 JSON",
        "Hysteria online API returned invalid JSON during custom probe": "自定义探测期间，Hysteria 在线状态接口返回了无效 JSON",
        "Hysteria online API returned invalid JSON during proxy probe": "代理探测期间，Hysteria 在线状态接口返回了无效 JSON",
        "failed to remove temporary Hysteria probe files": "无法移除 Hysteria 探测临时文件",
        "ECH client probe could not find the uploaded key resource": "ECH 客户端探测找不到已上传的密钥资源",
        "ECH resource is not valid UTF-8": "ECH 资源不是有效的 UTF-8 文本",
        "HysteriaX ownership marker is missing; refusing to remove remote files": "缺少 HysteriaX 所有权标记，已拒绝删除远端文件",
    ]

}

enum AuditDisplayText {
    static func action(_ value: String) -> String {
        let labels = [
            "node.created": "创建节点",
            "node.updated": "更新节点",
            "node.restricted": "限制节点代理",
            "node.restored": "恢复节点代理",
            "node.usage_reset": "重置节点套餐用量",
            "node.usage_corrected": "校正节点套餐用量",
            "node.deleted": "删除节点",
            "node.record_removed": "移除节点管理记录",
            "node.resource_uploaded": "上传节点资源",
            "node.resource_deleted": "删除节点资源",
            "node.uninstall_requested": "请求卸载节点",
            "user.created": "创建用户",
            "user.updated": "更新用户",
            "user.deleted": "删除用户",
            "user.assigned": "分配用户到节点",
            "user.unassigned": "撤销节点分配",
            "user.client_certificate_updated": "更新客户端证书",
            "user.credentials_rotated": "轮换连接凭据",
            "user.subscription_rotated": "轮换订阅令牌",
            "user.quota_reset": "重置流量额度",
            "resource.created": "上传配置资源",
            "resource.deleted": "删除配置资源",
            "credential.created": "创建凭据",
            "credential.updated": "更新凭据",
            "credential.version_published": "发布凭据版本",
            "credential.deleted": "删除凭据",
            "credential.batch_retried": "重试凭据更新批次",
            "dns.connection.created": "创建 DNS 连接",
            "dns.connection.updated": "更新 DNS 连接",
            "dns.connection.deleted": "删除 DNS 连接",
            "dns.zone.updated": "更新 DNS 域名",
            "dns.node.bound": "绑定节点 DNS",
            "dns.node.unbound": "解除节点 DNS 绑定",
            "dns.verify": "验证 DNS 连接",
            "dns.connection-refresh": "刷新 DNS 域名",
            "dns.zone-refresh": "刷新 DNS 记录",
            "dns.record-create": "创建 DNS 记录",
            "dns.record-update": "修改 DNS 记录",
            "dns.record-delete": "删除 DNS 记录",
            "dns.record-check": "验证 DNS 解析",
            "dns.credential-apply": "更新 DNS 连接凭据",
            "admin_token.created": "创建管理员令牌",
            "admin_token.revoked": "撤销管理员令牌",
        ]
        return labels[value] ?? value
    }

    static func entityType(_ value: String) -> String {
        switch value {
        case "node": "节点"
        case "user": "用户"
        case "resource": "资源"
        case "admin_token": "管理员令牌"
        case "credential": "凭据"
        case "dns": "DNS"
        default: value
        }
    }

    static func actor(_ value: String) -> String {
        switch value {
        case "admin": "管理员"
        case "system": "系统"
        default: value
        }
    }
}
