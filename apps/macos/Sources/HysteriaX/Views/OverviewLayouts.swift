import SwiftUI

// These layouts have one content tree. Width changes arrange that tree directly,
// rather than constructing and probing duplicate candidate trees.
struct OverviewSummaryLayout: Layout {
    struct Cache { var width: CGFloat = -1; var sizes: [CGSize] = [] }
    func makeCache(subviews: Subviews) -> Cache { Cache() }
    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout Cache) -> CGSize {
        let width = proposal.width.flatMap { $0.isFinite ? $0 : nil } ?? 636
        measure(width, subviews, &cache)
        let height = width >= 636 ? cache.sizes.map(\.height).max() ?? 0 : cache.sizes.map(\.height).reduce(0,+) + 20
        return CGSize(width: width, height: height)
    }
    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout Cache) {
        if cache.width != bounds.width || cache.sizes.count != subviews.count { measure(bounds.width, subviews, &cache) }
        var y = bounds.minY
        for (index, view) in subviews.enumerated() {
            let x = bounds.width >= 636 && index == 1 ? bounds.minX + 296 : bounds.minX
            view.place(at: CGPoint(x: x,y: y), anchor: .topLeading, proposal: ProposedViewSize(cache.sizes[index]))
            if bounds.width < 636 { y += cache.sizes[index].height + 20 }
        }
    }
    private func measure(_ width: CGFloat, _ subviews: Subviews, _ cache: inout Cache) {
        cache.width = width
        cache.sizes = subviews.enumerated().map { index, view in
            let w = width >= 636 ? (index == 0 ? 280 : width-296) : width
            let size = view.sizeThatFits(ProposedViewSize(width: max(0,w),height:nil))
            return CGSize(width:max(0,w),height:size.height)
        }
    }
}

struct OverviewColumnsLayout: Layout {
    let wideColumns: Int
    let wideMinimum: CGFloat
    var narrowColumns = 2
    var spacing: CGFloat = 16
    struct Cache { var width: CGFloat = -1; var sizes: [CGSize] = []; var rows: [CGFloat] = []; var columns = 1 }
    func makeCache(subviews: Subviews) -> Cache { Cache() }
    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout Cache) -> CGSize {
        let width = proposal.width.flatMap { $0.isFinite ? $0 : nil } ?? wideMinimum
        measure(width, subviews, &cache)
        return CGSize(width:width,height:cache.rows.reduce(0,+) + CGFloat(max(0,cache.rows.count-1))*spacing)
    }
    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout Cache) {
        if cache.width != bounds.width || cache.sizes.count != subviews.count { measure(bounds.width, subviews, &cache) }
        var y = bounds.minY
        for (index, view) in subviews.enumerated() {
            let column = index % cache.columns
            if index > 0 && column == 0 { y += cache.rows[index/cache.columns-1] + spacing }
            view.place(at:CGPoint(x:bounds.minX + CGFloat(column)*(cache.sizes[index].width+spacing),y:y),anchor:.topLeading,proposal:ProposedViewSize(cache.sizes[index]))
        }
    }
    private func measure(_ width: CGFloat, _ subviews: Subviews, _ cache: inout Cache) {
        cache.width = width
        cache.columns = width >= wideMinimum ? wideColumns : narrowColumns
        let w = max(0,(width-CGFloat(cache.columns-1)*spacing)/CGFloat(cache.columns))
        cache.sizes = subviews.map { CGSize(width:w,height:$0.sizeThatFits(ProposedViewSize(width:w,height:nil)).height) }
        cache.rows = stride(from:0,to:cache.sizes.count,by:cache.columns).map { start in cache.sizes[start..<min(start+cache.columns,cache.sizes.count)].map(\.height).max() ?? 0 }
    }
}

struct OverviewPairLayout: Layout {
    var horizontalMinimum: CGFloat = 420
    var flexibleIndex = 0
    var spacing: CGFloat = 24
    var stackedSpacing: CGFloat = 12
    struct Cache { var width: CGFloat = -1; var sizes: [CGSize] = [] }
    func makeCache(subviews: Subviews) -> Cache { Cache() }
    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout Cache) -> CGSize {
        let width = proposal.width.flatMap { $0.isFinite ? $0 : nil } ?? horizontalMinimum
        measure(width,subviews,&cache)
        return CGSize(width:width,height:width >= horizontalMinimum ? cache.sizes.map(\.height).max() ?? 0 : cache.sizes.map(\.height).reduce(0,+)+stackedSpacing)
    }
    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout Cache) {
        if cache.width != bounds.width || cache.sizes.count != subviews.count { measure(bounds.width,subviews,&cache) }
        var x = bounds.minX
        var y = bounds.minY
        for (index,view) in subviews.enumerated() {
            let trailing = bounds.width >= horizontalMinimum && index == 1
            view.place(at:CGPoint(x:trailing ? bounds.maxX : x,y:y),anchor:trailing ? .topTrailing : .topLeading,proposal:ProposedViewSize(cache.sizes[index]))
            if bounds.width >= horizontalMinimum { x += cache.sizes[index].width+spacing }
            else { y += cache.sizes[index].height+stackedSpacing }
        }
    }
    private func measure(_ width: CGFloat, _ subviews: Subviews, _ cache: inout Cache) {
        cache.width = width
        var widths = Array(repeating:width,count:subviews.count)
        if width >= horizontalMinimum && subviews.count == 2 {
            let fixed = 1-flexibleIndex
            widths[fixed] = min(width/2,subviews[fixed].sizeThatFits(.unspecified).width)
            widths[flexibleIndex] = max(0,width-widths[fixed]-spacing)
        }
        cache.sizes = subviews.enumerated().map { index,view in CGSize(width:widths[index],height:view.sizeThatFits(ProposedViewSize(width:widths[index],height:nil)).height) }
    }
}

struct OverviewControlWidth: LayoutValueKey { static let defaultValue: CGFloat = 240 }

struct OverviewFilterLayout: Layout {
    struct Cache { var width: CGFloat = -1; var sizes: [CGSize] = []; var horizontal = false }
    func makeCache(subviews: Subviews) -> Cache { Cache() }
    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout Cache) -> CGSize {
        let width = proposal.width.flatMap { $0.isFinite ? $0 : nil } ?? 700
        measure(width,subviews,&cache)
        return CGSize(width:width,height:cache.horizontal ? cache.sizes.map(\.height).max() ?? 0 : cache.sizes.map(\.height).reduce(0,+)+CGFloat(max(0,subviews.count-1))*8)
    }
    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout Cache) {
        if cache.width != bounds.width || cache.sizes.count != subviews.count { measure(bounds.width,subviews,&cache) }
        var point = bounds.origin
        for (index,view) in subviews.enumerated() {
            view.place(at:point,anchor:.topLeading,proposal:ProposedViewSize(cache.sizes[index]))
            if cache.horizontal { point.x += cache.sizes[index].width+8 }
            else { point.y += cache.sizes[index].height+8 }
        }
    }
    private func measure(_ width: CGFloat, _ subviews: Subviews, _ cache: inout Cache) {
        cache.width = width
        let preferred = subviews.map { $0[OverviewControlWidth.self] }
        cache.horizontal = preferred.reduce(0,+)+CGFloat(max(0,subviews.count-1))*8 <= width
        cache.sizes = subviews.enumerated().map { index,view in
            let w = cache.horizontal || preferred[index] == 20 ? preferred[index] : width
            return CGSize(width:w,height:view.sizeThatFits(ProposedViewSize(width:w,height:nil)).height)
        }
    }
}
