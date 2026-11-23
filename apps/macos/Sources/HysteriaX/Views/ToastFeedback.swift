import Observation
import SwiftUI

@MainActor
@Observable
final class ToastCenter {
    struct ToastItem: Identifiable, Equatable {
        let id = UUID()
        let message: String
        let detail: String?
        let tone: StatusTone
        let symbol: String
    }

    private(set) var current: ToastItem?
    @ObservationIgnored private var dismissTask: Task<Void, Never>?

    func show(
        _ message: String,
        detail: String? = nil,
        tone: StatusTone = .positive,
        symbol: String? = nil,
        duration: Duration = .seconds(3.5)
    ) {
        let trimmedDetail = detail?.trimmingCharacters(in: .whitespacesAndNewlines)
        let item = ToastItem(
            message: message,
            detail: trimmedDetail?.isEmpty == true ? nil : trimmedDetail,
            tone: tone,
            symbol: symbol ?? tone.defaultSymbol
        )
        dismissTask?.cancel()
        current = item
        dismissTask = Task { [weak self] in
            try? await Task.sleep(for: duration)
            guard !Task.isCancelled, let self, self.current?.id == item.id else { return }
            self.current = nil
        }
    }

    func dismiss() {
        dismissTask?.cancel()
        dismissTask = nil
        current = nil
    }
}

struct ToastOverlayModifier: ViewModifier {
    var center: ToastCenter

    func body(content: Content) -> some View {
        content.overlay(alignment: .bottom) {
            if let item = center.current {
                HStack(alignment: .center, spacing: 10) {
                    Image(systemName: item.symbol)
                        .font(.body.weight(.semibold))
                        .foregroundStyle(item.tone.color)
                        .accessibilityHidden(true)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(item.message)
                            .font(.callout.weight(.medium))
                            .lineLimit(2)
                        if let detail = item.detail {
                            Text(detail)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .lineLimit(2)
                        }
                    }
                    Button {
                        center.dismiss()
                    } label: {
                        Image(systemName: "xmark")
                            .font(.caption.weight(.semibold))
                            .foregroundStyle(.secondary)
                            .frame(width: 20, height: 20)
                            .contentShape(Circle())
                    }
                    .buttonStyle(.plain)
                    .help(L10n.text("关闭提示"))
                    .accessibilityLabel(L10n.text("关闭提示"))
                    .accessibilityIdentifier("app.toast.dismiss")
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
                .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
                .overlay(
                    RoundedRectangle(cornerRadius: 12, style: .continuous)
                        .strokeBorder(.primary.opacity(0.12), lineWidth: 0.5)
                )
                .shadow(color: .black.opacity(0.12), radius: 10, y: 4)
                .frame(maxWidth: 480)
                .padding(.horizontal, 20)
                .padding(.bottom, 18)
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("app.toast")
                .transition(.move(edge: .bottom).combined(with: .opacity))
            }
        }
        .animation(.snappy(duration: 0.22), value: center.current)
    }
}

extension View {
    func toastOverlay(_ center: ToastCenter) -> some View {
        modifier(ToastOverlayModifier(center: center))
    }
}
