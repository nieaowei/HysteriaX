import SwiftUI

struct OverviewServerCard: View {
    let monitoring: ServerMonitoring?
    let serviceAddress: String
    let isConnected: Bool
    let isLoading: Bool
    let error: String?
    var toast: ToastCenter? = nil

    var body: some View {
        OverviewCard(L10n.text("管理服务器"), systemImage: "server.rack") {
            headerBadges
        } content: {
            if let monitor = monitoring {
                VStack(alignment: .leading, spacing: 12) {
                    if !isConnected || error != nil {
                        staleBanner(error: error)
                    }
                    HStack(alignment: .top, spacing: 12) {
                        serverSystemAndAddress(monitor)
                            .frame(maxWidth: .infinity, alignment: .leading)
                        serverUptimes(monitor)
                            .fixedSize(horizontal: true, vertical: false)
                    }
                    OverviewColumnsLayout(wideColumns: 3, wideMinimum: 400, narrowColumns: 1, spacing: 10) {
                        serverResources(monitor)
                    }
                    Text(L10n.text("采集于 {0} · 每 15 秒刷新", String(describing: sampleTime(monitor.sampledAt))))
                        .font(.caption2)
                        .monospacedDigit()
                        .foregroundStyle(.tertiary)
                        .help(L10n.text("资源指标为管理服务所在环境可见的主机数据。"))
                }
            } else {
                if isLoading {
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small)
                        Text(L10n.text("正在获取服务器监控信息…"))
                            .foregroundStyle(.secondary)
                    }
                    .padding(.vertical, 6)
                } else {
                    Label(L10n.text("暂无服务器监控信息"), systemImage: "server.rack")
                        .foregroundStyle(.secondary)
                        .padding(.vertical, 6)
                }
                if let error {
                    errorBanner(error)
                }
            }
        }
        .accessibilityIdentifier("overview.serverMonitoring")
    }

    @ViewBuilder
    private var headerBadges: some View {
        if let monitor = monitoring {
            HStack(spacing: 6) {
                StatusBadge(versionLabel(monitor.serviceVersion), tone: .neutral, size: .compact, showDot: false)
                StatusBadge(
                    monitor.database == "ok" ? L10n.text("数据库正常") : L10n.text("数据库不可用"),
                    tone: monitor.database == "ok" ? .positive : .danger,
                    size: .compact
                )
            }
        }
    }

    private func staleBanner(error: String?) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Label(L10n.text("以下为上次采集的数据"), systemImage: "clock.arrow.circlepath")
                .font(.caption.weight(.medium))
                .foregroundStyle(.orange)
            if let error {
                Text(error)
                    .font(.caption2)
                    .foregroundStyle(.orange.opacity(0.9))
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 8)
        .padding(.vertical, 5)
        .background(.orange.opacity(0.08), in: RoundedRectangle(cornerRadius: 6, style: .continuous))
    }

    private func errorBanner(_ error: String) -> some View {
        Label(error, systemImage: "exclamationmark.triangle")
            .font(.caption)
            .foregroundStyle(.orange)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 8)
            .padding(.vertical, 5)
            .background(.orange.opacity(0.08), in: RoundedRectangle(cornerRadius: 6, style: .continuous))
    }

    private func serverSystemAndAddress(_ monitor: ServerMonitoring) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(monitor.os ?? L10n.text("系统信息未提供"))
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.middle)
                .frame(minHeight: 16, alignment: .leading)
            if !serviceAddress.isEmpty {
                HStack(spacing: 4) {
                    Text(serviceAddress)
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .textSelection(.enabled)
                        .help(serviceAddress)
                    CopyActionButton(value: serviceAddress, title: L10n.text("已复制服务地址"), toast: toast)
                }
                .frame(minHeight: 20, alignment: .leading)
            }
        }
    }

    private func serverUptimes(_ monitor: ServerMonitoring) -> some View {
        VStack(alignment: .trailing, spacing: 3) {
            uptimeRow(label: L10n.text("服务运行"), value: uptime(monitor.serviceUptimeSeconds))
                .frame(minHeight: 16, alignment: .trailing)
            uptimeRow(label: L10n.text("主机运行"), value: uptime(monitor.hostUptimeSeconds))
                .frame(minHeight: 20, alignment: .trailing)
        }
    }

    private func uptimeRow(label: String, value: String) -> some View {
        HStack(spacing: 6) {
            Text(label)
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(value)
                .font(.caption.weight(.medium))
                .monospacedDigit()
        }
    }

    private func serverResources(_ monitor: ServerMonitoring) -> some View {
        let memoryFraction = fraction(used: monitor.memoryUsedBytes, total: monitor.memoryTotalBytes)
        let diskFraction = fraction(used: monitor.rootDiskUsedBytes, total: monitor.rootDiskTotalBytes)
        return Group {
            resourceMetric(
                "CPU",
                symbol: "cpu",
                fraction: monitor.cpuUsagePercent / 100,
                value: String(format: "%.1f%%", monitor.cpuUsagePercent),
                detail: L10n.text("{0} 个逻辑核心", String(describing: monitor.cpuCount))
            )
            resourceMetric(
                L10n.text("内存"),
                symbol: "memorychip",
                fraction: memoryFraction,
                value: percentage(memoryFraction),
                detail: usage(monitor.memoryUsedBytes, monitor.memoryTotalBytes)
            )
            resourceMetric(
                L10n.text("根目录磁盘"),
                symbol: "internaldrive",
                fraction: diskFraction,
                value: percentage(diskFraction),
                detail: usage(monitor.rootDiskUsedBytes, monitor.rootDiskTotalBytes)
            )
        }
    }

    private func resourceMetric(_ title: String, symbol: String, fraction: Double?, value: String, detail: String) -> some View {
        let tone = resourceTone(for: fraction)
        return VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Label(title, systemImage: symbol)
                    .font(.caption.weight(.medium))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                Spacer(minLength: 4)
                Text(value)
                    .font(.callout.weight(.semibold))
                    .monospacedDigit()
                    .foregroundStyle(tone == .warning || tone == .danger ? AnyShapeStyle(tone.color) : AnyShapeStyle(.primary))
                    .lineLimit(1)
            }
            ProgressView(value: min(max(fraction ?? 0, 0), 1))
                .progressViewStyle(GradientUsageProgressStyle())
                .opacity(fraction == nil ? 0.35 : 1)
            Text(detail)
                .font(.caption2)
                .monospacedDigit()
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .minimumScaleFactor(0.85)
                .help(detail)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 9)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary.opacity(0.14), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
    }

    private func resourceTone(for fraction: Double?) -> StatusTone {
        guard let fraction else { return .neutral }
        if fraction >= 0.9 { return .danger }
        if fraction >= 0.75 { return .warning }
        return .neutral
    }

    private func versionLabel(_ version: String) -> String {
        version.hasPrefix("v") || version.hasPrefix("V") ? version : "v\(version)"
    }

    private func fraction(used: Int?, total: Int?) -> Double? {
        guard let used, let total, total > 0 else { return nil }
        return Double(used) / Double(total)
    }

    private func percentage(_ fraction: Double?) -> String {
        guard let fraction else { return "—" }
        return String(format: "%.1f%%", min(max(fraction, 0), 1) * 100)
    }

    private func usage(_ used: Int?, _ total: Int?) -> String {
        guard let used, let total, total > 0 else { return L10n.text("未提供") }
        return "\(ByteCountFormatter.string(fromByteCount: Int64(used), countStyle: .binary)) / \(ByteCountFormatter.string(fromByteCount: Int64(total), countStyle: .binary))"
    }

    private func uptime(_ seconds: Int) -> String {
        let days = seconds / 86_400
        let hours = seconds % 86_400 / 3_600
        let minutes = seconds % 3_600 / 60
        return days > 0 ? L10n.text("{0} 天 {1} 小时", String(describing: days), String(describing: hours)) : L10n.text("{0} 小时 {1} 分钟", String(describing: hours), String(describing: minutes))
    }

    private func sampleTime(_ timestamp: String) -> String {
        DateDisplayText.parse(timestamp).map { L10n.date($0, time: .standard) } ?? timestamp
    }
}
