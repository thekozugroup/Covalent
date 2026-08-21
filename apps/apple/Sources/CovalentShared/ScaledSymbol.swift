import SwiftUI

/// Dynamic Type-safe sizing for the large SF Symbol glyphs the app uses as
/// illustration.
///
/// `.font(.system(size: 42))` freezes a glyph at one size forever: it ignores
/// the reader's text-size setting entirely. That is invisible to the system
/// accessibility audit, which checks clipping, contrast and labels — not
/// scaling — so the defect survives a green audit. These modifiers keep the
/// designed proportions while still honouring the reader's setting.
private struct ScaledSymbolFont: ViewModifier {
    private let weight: Font.Weight
    @ScaledMetric private var scaledSize: CGFloat

    init(size: CGFloat, weight: Font.Weight, relativeTo textStyle: Font.TextStyle) {
        self.weight = weight
        _scaledSize = ScaledMetric(wrappedValue: size, relativeTo: textStyle)
    }

    func body(content: Content) -> some View {
        content.font(.system(size: scaledSize, weight: weight))
    }
}

private struct ScaledSymbolFrame: ViewModifier {
    @ScaledMetric private var scaledSide: CGFloat

    init(side: CGFloat, relativeTo textStyle: Font.TextStyle) {
        _scaledSide = ScaledMetric(wrappedValue: side, relativeTo: textStyle)
    }

    func body(content: Content) -> some View {
        content.frame(width: scaledSide, height: scaledSide)
    }
}

public extension View {
    /// Sizes a symbol glyph in points that scale with the reader's text size.
    ///
    /// Use this instead of `.font(.system(size:))` whenever the design calls
    /// for a glyph larger than any semantic text style provides.
    func scaledSymbolFont(
        size: CGFloat,
        weight: Font.Weight = .regular,
        relativeTo textStyle: Font.TextStyle = .largeTitle
    ) -> some View {
        modifier(ScaledSymbolFont(size: size, weight: weight, relativeTo: textStyle))
    }

    /// A square frame that grows with the reader's text size.
    ///
    /// Pair this with ``scaledSymbolFont(size:weight:relativeTo:)`` whenever a
    /// glyph sits inside a decorative tile, so the tile never clips the glyph
    /// at large accessibility sizes.
    func scaledSymbolFrame(
        _ side: CGFloat,
        relativeTo textStyle: Font.TextStyle = .largeTitle
    ) -> some View {
        modifier(ScaledSymbolFrame(side: side, relativeTo: textStyle))
    }
}
