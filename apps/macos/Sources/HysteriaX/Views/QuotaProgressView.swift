import SwiftUI

struct QuotaProgressView: View {
    let usageBytes: Int64?
    let quotaBytes: Int64?

    private var fraction: Double {
        guard let usageBytes, let quotaBytes else { return 0 }
        guard quotaBytes > 0 else { return 1 }
        return min(1, max(0, Double(usageBytes) / Double(quotaBytes)))
    }

    private var usageText: String {
        usageBytes.map { ByteCountFormatter.string(fromByteCount: $0, countStyle: .file) } ?? "—"
    }

    private var quotaText: String {
        quotaBytes.map { ByteCountFormatter.string(fromByteCount: $0, countStyle: .file) } ?? L10n.text("不限")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 4) {
                Text(usageText)
                Text("/").foregroundStyle(.secondary)
                if quotaBytes != nil {
                    Text(quotaText)
                } else {
                    Image(systemName: "infinity")
                }
            }
            .font(.caption)
            .monospacedDigit()
            .lineLimit(1)
            ProgressView(value: fraction)
                .progressViewStyle(GradientUsageProgressStyle())
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(L10n.text("流量用量"))
        .accessibilityValue(L10n.text("已用 {0}，额度 {1}", String(describing: (usageText)), String(describing: (quotaText))))
        .help(L10n.text("已用 {0}，额度 {1}", String(describing: (usageText)), String(describing: (quotaText))))
    }
}

struct GradientUsageProgressStyle: ProgressViewStyle {
    func makeBody(configuration: Configuration) -> some View {
        let fraction = min(1, max(0, configuration.fractionCompleted ?? 0))
        GeometryReader { geometry in
            ZStack(alignment: .leading) {
                Capsule().fill(.quaternary)
                if fraction > 0 {
                    Capsule()
                        .fill(LinearGradient(
                            colors: fraction >= 0.9 ? [.orange, .red] : [.blue, .cyan],
                            startPoint: .leading,
                            endPoint: .trailing
                        ))
                        .frame(width: geometry.size.width * fraction)
                }
            }
            .frame(height: 4)
            .clipShape(Capsule())
        }
        .frame(height: 4)
    }
}
