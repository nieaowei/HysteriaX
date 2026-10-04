import SwiftUI

// Shared native container keeps summary cards visually consistent.
struct OverviewCard<Content: View>: View {
    let title: String
    let systemImage: String
    let content: Content

    init(_ title: String, systemImage: String, @ViewBuilder content: () -> Content) {
        self.title = title
        self.systemImage = systemImage
        self.content = content()
    }

    var body: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 16) {
                Label(title, systemImage: systemImage).font(.headline)
                content
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(8)
        }
    }
}
