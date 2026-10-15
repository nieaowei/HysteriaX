import SwiftUI

// Shared native container keeps summary cards visually consistent.
struct OverviewCard<Content: View>: View {
    let title: String
    let systemImage: String
    let content: Content
    let isRefreshing: Bool?

    init(_ title: String, systemImage: String, isRefreshing: Bool? = nil, @ViewBuilder content: () -> Content) {
        self.title = title
        self.systemImage = systemImage
        self.content = content()
        self.isRefreshing = isRefreshing
    }

    var body: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 16) {
                HStack {
                    Label(title, systemImage: systemImage).font(.headline)
                    if let isRefreshing {
                        Spacer()
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
