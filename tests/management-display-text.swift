import Foundation

@main
struct ManagementDisplayTextTests {
    static func main() {
        let healthLog = JobDisplayText.logMessage(stage: "checking_health", message: "Waiting up to 180 seconds for the traffic and online statistics APIs, including certificate provisioning.")
        precondition(healthLog.contains("180 秒") && healthLog.contains("证书签发"))
        let kickLog = JobDisplayText.logMessage(stage: "checking_clients", message: "12 client device(s) remain online; requesting another kick.")
        precondition(kickLog.contains("12 台设备"))
        let unrelated = "12 unexpected devices: inspect manually"
        precondition(JobDisplayText.logMessage(stage: "checking_clients", message: unrelated) == unrelated)

        let fingerprint = "SSH host key changed: expected SHA256:old; observed SHA256:new"
        let translatedFingerprint = JobDisplayText.errorMessage(fingerprint)
        precondition(translatedFingerprint.contains("指纹已变化") && translatedFingerprint.contains("SHA256:old") && translatedFingerprint.contains("SHA256:new"))
        let chained = "SSH verification failed; previous binding was retained, but its remote credential may already be invalid: SSH connection failed: SSH connection timed out"
        let translatedChain = JobDisplayText.errorMessage(chained)
        precondition(translatedChain.contains("已保留原绑定") && translatedChain.contains("远端凭据可能已失效") && translatedChain.contains("SSH 连接超时"))

        let diagnostics = "Oct 07 node systemd[1]: Failed (code=exited, status=1)\nhttps://example.test:8443"
        let failed = "remote command failed with exit code 53: post-deployment health check failed (traffic and online endpoints did not become ready; remote diagnostics: \(diagnostics)); previous configuration was restored"
        let translated = JobDisplayText.errorMessage(failed)
        precondition(translated.contains("退出码 53") && translated.contains("接口未就绪") && translated.contains("已恢复上一配置"))
        precondition(translated.contains(diagnostics), "Remote diagnostics must remain byte-for-byte intact")
        let timeout = JobDisplayText.errorMessage("startup health check timed out after 180.5s (budget 180s); last error: SSH connection failed: SSH connection timed out")
        precondition(timeout.contains("180.5 秒") && timeout.contains("180 秒") && timeout.contains("SSH 连接超时"))
        precondition(JobDisplayText.errorMessage("DNS provider returned HTTP 429").contains("HTTP 429"))
        let unknown = "NewProviderError: opaque endpoint https://example.test:443"
        precondition(JobDisplayText.errorMessage(unknown) == unknown)
        precondition(AuditDisplayText.actor("token-id-123") == "token-id-123")
        precondition(AuditDisplayText.action("future.action") == "future.action")
        print("Management display text checks passed")
    }
}
