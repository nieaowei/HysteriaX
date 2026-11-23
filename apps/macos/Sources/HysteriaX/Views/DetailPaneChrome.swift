import AppKit
import SwiftUI

struct DetailCloseButton: View {
    let action: () -> Void
    @State private var isHovered = false

    var body: some View {
        Button(action: action) {
            Image(systemName: "xmark")
                .font(.caption.weight(.semibold))
                .foregroundStyle(isHovered ? .primary : .secondary)
                .frame(width: 24, height: 24)
                .background(
                    Circle().fill(isHovered ? Color.primary.opacity(0.08) : Color.clear)
                )
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .onHover { isHovered = $0 }
        .help(L10n.text("关闭详情"))
        .accessibilityLabel(L10n.text("关闭详情"))
        .accessibilityIdentifier("detail.close")
    }
}

struct DetailHeaderBar<Content: View>: View {
    @ViewBuilder var content: () -> Content

    var body: some View {
        VStack(spacing: 0) {
            content()
                .padding(16)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(.quaternary.opacity(0.16))
            Divider()
        }
    }
}

struct InteractiveRowHoverModifier: ViewModifier {
    var cornerRadius: CGFloat = 6
    @State private var isHovered = false

    func body(content: Content) -> some View {
        content
            .background(
                RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                    .fill(isHovered ? Color.primary.opacity(0.05) : Color.clear)
            )
            .onHover { isHovered = $0 }
            .animation(.easeOut(duration: 0.12), value: isHovered)
    }
}

extension View {
    func interactiveRowHover(cornerRadius: CGFloat = 6) -> some View {
        modifier(InteractiveRowHoverModifier(cornerRadius: cornerRadius))
    }
}

struct CopyActionButton: View {
    let value: String
    var title: String? = nil
    var toast: ToastCenter? = nil

    @State private var isHovered = false
    @State private var copied = false

    var body: some View {
        Button {
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(value, forType: .string)
            copied = true
            toast?.show(
                title ?? L10n.text("已复制"),
                detail: value,
                tone: .positive,
                symbol: "doc.on.doc",
                duration: .seconds(2.2)
            )
            Task { @MainActor in
                try? await Task.sleep(for: .seconds(1.5))
                copied = false
            }
        } label: {
            Image(systemName: copied ? "checkmark" : "doc.on.doc")
                .font(.caption)
                .foregroundStyle(copied ? Color.green : (isHovered ? Color.primary : Color.secondary))
                .frame(width: 20, height: 20)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { isHovered = $0 }
        .help(L10n.text("复制"))
        .accessibilityLabel(L10n.text("复制"))
    }
}
