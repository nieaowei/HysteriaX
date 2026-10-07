import SwiftUI

struct OverviewServerCard: View {
    let monitoring: ServerMonitoring?
    let serviceAddress: String
    let isConnected: Bool
    let isLoading: Bool
    let error: String?

    var body: some View {
        OverviewCard(L10n.text("管理服务器"), systemImage: "server.rack") {
            if let monitor = monitoring {
                if !isConnected || error != nil {
                    Label(L10n.text("以下为上次采集的数据"), systemImage: "clock.arrow.circlepath")
                        .font(.callout).foregroundStyle(.orange)
                }
                OverviewPairLayout {
                    serverIdentity(monitor)
                    serverUptimes(monitor, alignment: .leading)
                }
                OverviewColumnsLayout(wideColumns: 3, wideMinimum: 480) { serverResources(monitor) }
                OverviewPairLayout(flexibleIndex: 1, spacing: 16, stackedSpacing: 6) {
                    serverDatabase(monitor)
                    Text(L10n.text("采集于 {0} · 每 15 秒刷新", String(describing: (sampleTime(monitor.sampledAt)))))
                        .foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }.font(.caption)
                Text(L10n.text("资源指标为管理服务所在环境可见的主机数据。"))
                    .font(.caption).foregroundStyle(.secondary)
            } else {
                Text(isLoading ? L10n.text("正在获取服务器监控信息…") : L10n.text("暂无服务器监控信息"))
                    .foregroundStyle(.secondary).padding(.vertical, 8)
            }
            if let error = error {
                Text(error).font(.caption).foregroundStyle(.orange)
            }
        }.accessibilityIdentifier("overview.serverMonitoring")
    }


    private func serverDatabase(_ monitor: ServerMonitoring) -> some View {
        Label(monitor.database == "ok" ? L10n.text("数据库正常") : L10n.text("数据库不可用"),
              systemImage: monitor.database == "ok" ? "checkmark.circle" : "exclamationmark.triangle")
            .foregroundStyle(monitor.database == "ok" ? Color.secondary : Color.orange)
    }

    private func serverIdentity(_ monitor: ServerMonitoring) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(monitor.hostname ?? L10n.text("主机名未提供")).fontWeight(.medium)
            Text(monitor.os ?? L10n.text("系统信息未提供")).foregroundStyle(.secondary)
            Text(serviceAddress).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
        }
    }

    private func serverUptimes(_ monitor: ServerMonitoring, alignment: HorizontalAlignment) -> some View {
        VStack(alignment: alignment, spacing: 6) {
            Text(L10n.text("服务版本 {0}", String(describing: (monitor.serviceVersion))))
            Text(L10n.text("服务运行 {0}", String(describing: (uptime(monitor.serviceUptimeSeconds)))))
            Text(L10n.text("主机运行 {0}", String(describing: (uptime(monitor.hostUptimeSeconds))))).foregroundStyle(.secondary)
        }
    }

    private func serverResources(_ monitor: ServerMonitoring) -> some View {
        Group {
            resourceMetric("CPU", symbol: "cpu", fraction: monitor.cpuUsagePercent / 100,
                           value: String(format: "%.1f%%", monitor.cpuUsagePercent),
                           detail: L10n.text("{0} 个逻辑核心", String(describing: (monitor.cpuCount))))
            resourceMetric(L10n.text("内存"), symbol: "memorychip",
                           fraction: fraction(used: monitor.memoryUsedBytes, total: monitor.memoryTotalBytes),
                           value: usage(monitor.memoryUsedBytes, monitor.memoryTotalBytes), detail: L10n.text("已用 / 总量"))
            resourceMetric(L10n.text("根目录磁盘"), symbol: "internaldrive",
                           fraction: fraction(used: monitor.rootDiskUsedBytes, total: monitor.rootDiskTotalBytes),
                           value: usage(monitor.rootDiskUsedBytes, monitor.rootDiskTotalBytes), detail: L10n.text("已用 / 总量"))
        }
    }

    private func resourceMetric(_ title: String, symbol: String, fraction: Double?, value: String, detail: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Label(title, systemImage: symbol).foregroundStyle(.secondary)
            Text(value).font(.title3).fontWeight(.semibold).monospacedDigit()
            if let fraction {
                ProgressView(value: min(max(fraction, 0), 1))
                    .progressViewStyle(GradientUsageProgressStyle())
            }
            Text(detail).font(.caption).foregroundStyle(.secondary)
        }.frame(maxWidth: .infinity, alignment: .leading)
    }

    private func fraction(used: Int?, total: Int?) -> Double? {
        guard let used, let total, total > 0 else { return nil }
        return Double(used) / Double(total)
    }

    private func usage(_ used: Int?, _ total: Int?) -> String {
        guard let used, let total, total > 0 else { return L10n.text("未提供") }
        return "\(ByteCountFormatter.string(fromByteCount: Int64(used), countStyle: .binary)) / \(ByteCountFormatter.string(fromByteCount: Int64(total), countStyle: .binary))"
    }

    private func uptime(_ seconds: Int) -> String {
        let days = seconds / 86_400
        let hours = seconds % 86_400 / 3_600
        let minutes = seconds % 3_600 / 60
        return days > 0 ? L10n.text("{0} 天 {1} 小时", String(describing: (days)), String(describing: (hours))) : L10n.text("{0} 小时 {1} 分钟", String(describing: (hours)), String(describing: (minutes)))
    }

    private func sampleTime(_ timestamp: String) -> String {
        DateDisplayText.parse(timestamp).map { L10n.date($0, time: .standard) } ?? timestamp
    }
}
