import SwiftUI

enum StatusTone: Sendable {
    case positive
    case active
    case warning
    case danger
    case neutral
    case accent

    var color: Color {
        switch self {
        case .positive: .green
        case .active: .blue
        case .warning: .orange
        case .danger: .red
        case .neutral: .secondary
        case .accent: .indigo
        }
    }

    var defaultSymbol: String {
        switch self {
        case .positive: "checkmark.circle.fill"
        case .active: "arrow.triangle.2.circlepath"
        case .warning: "exclamationmark.triangle.fill"
        case .danger: "xmark.circle.fill"
        case .neutral: "info.circle.fill"
        case .accent: "sparkles"
        }
    }
}

struct StatusBadge: View {
    enum Size {
        case compact
        case regular
    }

    let text: String
    let tone: StatusTone
    let size: Size
    let showDot: Bool
    let symbol: String?

    init(_ text: String, tone: StatusTone, size: Size = .regular, showDot: Bool = true, symbol: String? = nil) {
        self.text = text
        self.tone = tone
        self.size = size
        self.showDot = showDot
        self.symbol = symbol
    }

    var body: some View {
        HStack(spacing: size == .compact ? 4 : 5) {
            if let symbol {
                Image(systemName: symbol)
                    .font(size == .compact ? .system(size: 9, weight: .semibold) : .caption2.weight(.semibold))
                    .accessibilityHidden(true)
            } else if showDot {
                Circle()
                    .fill(tone.color)
                    .frame(width: 5, height: 5)
                    .accessibilityHidden(true)
            }
            Text(text)
                .font((size == .compact ? Font.caption2 : Font.caption).weight(.medium))
                .lineLimit(1)
        }
        .foregroundStyle(tone.color)
        .padding(.horizontal, size == .compact ? 6 : 8)
        .padding(.vertical, size == .compact ? 2 : 3)
        .background(tone.color.opacity(0.12), in: Capsule())
        .overlay(Capsule().strokeBorder(tone.color.opacity(0.22), lineWidth: 0.5))
        .fixedSize()
    }
}

struct SidebarCountBadge: View {
    let count: Int
    var tone: StatusTone = .warning

    private var label: String {
        count > 99 ? "99+" : String(count)
    }

    var body: some View {
        Text(label)
            .font(.caption2.weight(.semibold))
            .monospacedDigit()
            .foregroundStyle(tone.color)
            .padding(.horizontal, 6)
            .padding(.vertical, 1)
            .background(tone.color.opacity(0.16), in: Capsule())
            .fixedSize()
    }
}

enum JobStatusTone {
    static func tone(for status: String) -> StatusTone {
        switch status {
        case "succeeded": .positive
        case "queued", "running": .active
        case "failed", "rolled_back", "waiting_confirmation", "reconciling", "needs_attention": .warning
        case "cancelled": .neutral
        default: .neutral
        }
    }
}
