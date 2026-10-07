import SwiftUI

/// Reflows one title/action tree instead of probing separate horizontal and vertical trees.
struct DetailHeaderLayout: Layout {
    var spacing: CGFloat = 16
    var stackedSpacing: CGFloat = 10
    struct Cache {
        var width: CGFloat = -1
        var horizontal = false
        var sizes: [CGSize] = []
    }
    func makeCache(subviews: Subviews) -> Cache { Cache() }

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout Cache) -> CGSize {
        let width = proposal.width.flatMap { $0.isFinite ? $0 : nil } ?? 640
        measure(width, subviews, &cache)
        let height = cache.horizontal ? cache.sizes.map(\.height).max() ?? 0 : cache.sizes.map(\.height).reduce(0, +) + stackedSpacing * CGFloat(max(0, subviews.count - 1))
        return CGSize(width: width, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout Cache) {
        if cache.width != bounds.width || cache.sizes.count != subviews.count { measure(bounds.width, subviews, &cache) }
        var y = bounds.minY
        for (index, view) in subviews.enumerated() {
            let trailing = cache.horizontal && index == 1
            view.place(at: CGPoint(x: trailing ? bounds.maxX : bounds.minX, y: y),
                       anchor: trailing ? .topTrailing : .topLeading, proposal: ProposedViewSize(cache.sizes[index]))
            if !cache.horizontal { y += cache.sizes[index].height + stackedSpacing }
        }
    }

    private func measure(_ width: CGFloat, _ subviews: Subviews, _ cache: inout Cache) {
        cache.width = width
        cache.horizontal = false
        guard subviews.count == 2 else {
            // Conditional action groups can be absent while details load.
            cache.sizes = subviews.map { view in
                CGSize(width: width, height: view.sizeThatFits(ProposedViewSize(width: width, height: nil)).height)
            }
            return
        }
        let titleWidth = min(320, subviews[0].sizeThatFits(.unspecified).width)
        let actionsWidth = subviews[1].sizeThatFits(.unspecified).width
        cache.horizontal = width >= titleWidth + actionsWidth + spacing
        let widths = cache.horizontal ? [max(0, width - actionsWidth - spacing), actionsWidth] : [width, width]
        cache.sizes = subviews.enumerated().map { index, view in
            CGSize(width: widths[index], height: view.sizeThatFits(ProposedViewSize(width: widths[index], height: nil)).height)
        }
    }
}
