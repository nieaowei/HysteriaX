import SwiftUI

// Shared native container keeps summary cards visually consistent.
struct OverviewCard<HeaderTrailing: View, Content: View>: View {
    let title: String
    let systemImage: String
    let isRefreshing: Bool?
    let headerTrailing: HeaderTrailing
    let content: Content

    init(
        _ title: String,
        systemImage: String,
        isRefreshing: Bool? = nil,
        @ViewBuilder headerTrailing: () -> HeaderTrailing,
        @ViewBuilder content: () -> Content
    ) {
        self.title = title
        self.systemImage = systemImage
        self.isRefreshing = isRefreshing
        self.headerTrailing = headerTrailing()
        self.content = content()
    }

    var body: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 16) {
                HStack(spacing: 8) {
                    Label(title, systemImage: systemImage)
                        .font(.headline)
                        .lineLimit(1)
                    Spacer(minLength: 8)
                    headerTrailing
                    if let isRefreshing {
                        ZStack {
                            if isRefreshing {
                                ProgressView().controlSize(.small)
                                    .accessibilityLabel(L10n.text("刷新"))
                            }
                        }
                        .frame(width: 20, height: 20)
                        .accessibilityIdentifier("overview.card.refreshIndicator")
                    }
                }
                content
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(8)
        }
    }
}

extension OverviewCard where HeaderTrailing == EmptyView {
    init(_ title: String, systemImage: String, isRefreshing: Bool? = nil, @ViewBuilder content: () -> Content) {
        self.init(title, systemImage: systemImage, isRefreshing: isRefreshing, headerTrailing: { EmptyView() }, content: content)
    }
}
