import SwiftUI

struct OverviewServerCard: View {
    let monitoring: ServerMonitoring?
    let serviceAddress: String
    let isConnected: Bool
    let isLoading: Bool
    let error: String?

    var body: some View {
        OverviewCard("管理服务器", systemImage: "server.rack") {
            if let monitor = monitoring {
                if !isConnected || error != nil {
                    Label("以下为上次采集的数据", systemImage: "clock.arrow.circlepath")
                        .font(.callout).foregroundStyle(.orange)
                }
                OverviewPairLayout {
                    serverIdentity(monitor)
                    serverUptimes(monitor, alignment: .leading)
                }
                OverviewColumnsLayout(wideColumns: 3, wideMinimum: 480) { serverResources(monitor) }
                OverviewPairLayout(flexibleIndex: 1, spacing: 16, stackedSpacing: 6) {
                    serverDatabase(monitor)
                    Text("采集于 \(sampleTime(monitor.sampledAt)) · 每 15 秒刷新")
                        .foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }.font(.caption)
                Text("资源指标为管理服务所在环境可见的主机数据。")
                    .font(.caption).foregroundStyle(.secondary)
            } else {
                Text(isLoading ? "正在获取服务器监控信息…" : "暂无服务器监控信息")
                    .foregroundStyle(.secondary).padding(.vertical, 8)
            }
            if let error = error {
                Text(error).font(.caption).foregroundStyle(.orange)
            }
        }.accessibilityIdentifier("overview.serverMonitoring")
    }


    private func serverDatabase(_ monitor: ServerMonitoring) -> some View {
        Label(monitor.database == "ok" ? "数据库正常" : "数据库不可用",
              systemImage: monitor.database == "ok" ? "checkmark.circle" : "exclamationmark.triangle")
            .foregroundStyle(monitor.database == "ok" ? Color.secondary : Color.orange)
    }

    private func serverIdentity(_ monitor: ServerMonitoring) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(monitor.hostname ?? "主机名未提供").fontWeight(.medium)
            Text(monitor.os ?? "系统信息未提供").foregroundStyle(.secondary)
            Text(serviceAddress).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
        }
    }

    private func serverUptimes(_ monitor: ServerMonitoring, alignment: HorizontalAlignment) -> some View {
        VStack(alignment: alignment, spacing: 6) {
            Text("服务版本 \(monitor.serviceVersion)")
            Text("服务运行 \(uptime(monitor.serviceUptimeSeconds))")
            Text("主机运行 \(uptime(monitor.hostUptimeSeconds))").foregroundStyle(.secondary)
        }
    }

    private func serverResources(_ monitor: ServerMonitoring) -> some View {
        Group {
            resourceMetric("CPU", symbol: "cpu", fraction: monitor.cpuUsagePercent / 100,
                           value: String(format: "%.1f%%", monitor.cpuUsagePercent),
                           detail: "\(monitor.cpuCount) 个逻辑核心")
            resourceMetric("内存", symbol: "memorychip",
                           fraction: fraction(used: monitor.memoryUsedBytes, total: monitor.memoryTotalBytes),
                           value: usage(monitor.memoryUsedBytes, monitor.memoryTotalBytes), detail: "已用 / 总量")
            resourceMetric("根目录磁盘", symbol: "internaldrive",
                           fraction: fraction(used: monitor.rootDiskUsedBytes, total: monitor.rootDiskTotalBytes),
                           value: usage(monitor.rootDiskUsedBytes, monitor.rootDiskTotalBytes), detail: "已用 / 总量")
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
        guard let used, let total, total > 0 else { return "未提供" }
        return "\(ByteCountFormatter.string(fromByteCount: Int64(used), countStyle: .binary)) / \(ByteCountFormatter.string(fromByteCount: Int64(total), countStyle: .binary))"
    }

    private func uptime(_ seconds: Int) -> String {
        let days = seconds / 86_400
        let hours = seconds % 86_400 / 3_600
        let minutes = seconds % 3_600 / 60
        return days > 0 ? "\(days) 天 \(hours) 小时" : "\(hours) 小时 \(minutes) 分钟"
    }

    private func sampleTime(_ timestamp: String) -> String {
        DateDisplayText.parse(timestamp)?.formatted(date: .abbreviated, time: .standard) ?? timestamp
    }
}
