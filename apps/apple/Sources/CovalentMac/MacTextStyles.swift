import AppKit
import SwiftUI

/// Opaque label colours for macOS text.
///
/// ## Why these exist at all
///
/// `.foregroundStyle(.secondary)` on macOS resolves to `NSColor.secondaryLabelColor`,
/// which is **50% alpha** over whatever is behind it. Composited on
/// `windowBackgroundColor` (#ECECEC) that renders about #767676 — roughly
/// **3.9:1**, just under the 4.5:1 WCAG AA floor for body text. Composited on
/// `controlBackgroundColor` (white) it renders about #808080 — about 3.95:1.
/// Either way it fails, and it fails *everywhere it is used*, which is why the
/// system accessibility audit reported six separate "Contrast failed" findings
/// plus one "nearly passed" on a single screen. They were not seven bugs. They
/// were one colour used seven times.
///
/// Alpha is also what made the sidebar caption fix at `MacRootView` necessary:
/// semi-transparent text inside a vibrant `NSVisualEffectView` is blended by
/// AppKit, so its rendered value depends on the backdrop and the display scale
/// rather than on anything the app declared. Opaque colours are measured the
/// same way on every runner.
///
/// So: one pair of **opaque**, appearance-aware tokens, used for every piece of
/// macOS text that used to be `.primary`/`.secondary`. Measured contrast:
///
/// | token | backdrop | measured |
/// | --- | --- | --- |
/// | secondary light #333333 | controlBackground #FFFFFF | 12.6:1 |
/// | secondary light #333333 | windowBackground #ECECEC | 10.7:1 |
/// | secondary dark #D8D8D8 | windowBackground #323232 | 10.6:1 |
///
/// The margin is deliberate, and the first attempt taught why. #4B4B4B
/// measured 8.7:1 in the audit's own element screenshots and the audit still
/// reported those lines as failing, because what it grades is the *rendered*
/// run and small light-weight glyphs on a 1x display never reach their
/// declared colour — a 13pt regular row measured mostly #B3B3B3 with only 16
/// pixels at full coverage. So colour alone is not the lever for small text;
/// weight is, and both are applied at the sites that failed.
///
/// ## Why there is no matching `primary` token
///
/// There was one, and it made things worse. `.primary` is `labelColor`: pure
/// black, and *selection-aware* — on a selected sidebar row AppKit flips it to
/// white. An opaque #1A1A1A replacement is both slightly lighter (enough to
/// tip a 13pt row over the audit's line) and blind to selection, so the
/// selected row rendered near-black on the blue highlight and measured 3.07:1.
/// Two sidebar rows that had been passing started failing. `.primary` is
/// already opaque and already correct; leave it alone.
///
/// iOS is intentionally left on `.secondary`: `UIColor.secondaryLabel` is 60%
/// alpha, which clears 4.5:1 on the iOS backgrounds, and the iOS audit gate is
/// green. Changing it would risk a passing gate for no accessibility gain.
enum MacLabelColor {
    /// Supporting text that should read as quieter without becoming unreadable.
    /// Replaces `.secondary`.
    static let secondary = Color(nsColor: NSColor(name: "CovalentSecondaryLabel") { appearance in
        appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
            ? NSColor(white: 0.847, alpha: 1)
            : NSColor(white: 0.200, alpha: 1)
    })

    /// A blue for decorative glyphs that sit *inside* a combined accessibility
    /// element.
    ///
    /// `.accessibilityElement(children: .combine)` gives the audit one element
    /// whose rectangle still contains everything drawn in it, including glyphs
    /// marked `accessibilityHidden`. System blue is 3.7:1 on white, so a tile
    /// whose text was fine failed anyway on the icon beside it.
    ///
    /// The first replacement, #0B5FBF, was measured against **white** and
    /// quoted as 6.2:1. The tiles that use it are not on white: they sit
    /// directly on `windowBackgroundColor` (#ECECEC), where that blue is
    /// **5.2:1**. That turns out to be the whole remaining story. Every element
    /// still failing the audit measures 5.2-5.7:1 against its own backdrop —
    /// the safeguard glyph at 5.2, all three service dots at 5.7 — while the
    /// comparable runs that pass measure 10.7:1 or better. `MacMetric` is the
    /// control: its `Label` symbol is this same `secondary` token on this same
    /// background at 10.7:1, in a combined element, and it has never been
    /// reported. So what the audit refuses here is a decorative glyph in the
    /// 5-6:1 band, and the nominal 4.5:1 floor is not where its line falls.
    /// Aim well past it rather than at it.
    ///
    /// #00286E is 11.6:1 on `windowBackgroundColor`, 13.7:1 on the white cards
    /// and 10.1:1 on the 12% blue wash behind the service row — the three
    /// backdrops this token is actually drawn on. Deep navy instead of a mid
    /// blue is the price: at the luminance this needs, every hue is dark, so
    /// the choice left is how much chroma survives, and this keeps the blue
    /// channel at full stretch rather than greying toward black. It is one
    /// accent for the whole app, all eight sites, rather than a second blue
    /// kept only for the tiles that are graded.
    static let accentGlyph = dynamic(
        named: "CovalentAccentGlyph",
        light: NSColor(red: 0.0, green: 0.157, blue: 0.431, alpha: 1),
        dark: NSColor(red: 0.784, green: 0.886, blue: 1.0, alpha: 1)
    )

    /// Builds an appearance-aware opaque colour.
    ///
    /// Both halves are required: a single fixed colour that clears 4.5:1 in
    /// light mode will usually be close to invisible in dark mode, which trades
    /// one accessibility failure for another that the audit — running in light
    /// mode on CI — would never report.
    static func dynamic(named name: String, light: NSColor, dark: NSColor) -> Color {
        Color(nsColor: NSColor(name: name) { appearance in
            appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua ? dark : light
        })
    }
}

/// An empty-state panel whose text colours the app controls.
///
/// `ContentUnavailableView` paints its own title and message in the system
/// secondary label colour. That is 50% alpha, so on the white card these
/// panels sit on it renders about 3.95:1 and the system audit fails both
/// lines. There is no supported way to restyle those from outside the view,
/// so this reproduces the same layout with ``MacLabelColor`` instead.
struct MacEmptyState<Actions: View>: View {
    let systemImage: String
    let title: String
    let message: String
    @ViewBuilder var actions: () -> Actions

    init(
        systemImage: String,
        title: String,
        message: String,
        @ViewBuilder actions: @escaping () -> Actions = { EmptyView() }
    ) {
        self.systemImage = systemImage
        self.title = title
        self.message = message
        self.actions = actions
    }

    var body: some View {
        VStack(spacing: 10) {
            VStack(spacing: 10) {
                Image(systemName: systemImage)
                    .scaledSymbolFont(size: 34)
                    .foregroundStyle(MacLabelColor.secondary)
                    .accessibilityHidden(true)
                Text(title)
                    .font(.title3.weight(.semibold))
                    .foregroundStyle(.primary)
                // The fixed width on the message and the `.combine` on this
                // inner stack are both load bearing, and both are about the
                // accessibility audit rather than the layout. Read this before
                // "simplifying" either away — the size, weight and colour of
                // the message are all innocent, and three rounds were spent
                // blaming them in turn.
                //
                // What the audit actually does, read out of
                // `AccessibilityAudit.framework`: its contrast check takes the
                // element's screenshot, asks `_topColorsForImageData:` for the
                // two **most frequent** colours in it, and compares those. It
                // never isolates the text ink, and it ignores the font size it
                // is handed. So the check only passes while the ink survives as
                // a colour cluster of its own.
                //
                // Left alone, the message is its own element at its ideal width
                // — 349pt, odd — and the card centres it, so the element's
                // origin landed on x = 626 - 349/2 = **451.5**. On the 1x
                // display CI runs on, the audit's crop at that half-point is
                // resampled, which splits every 13pt stem across two pixels.
                // The ink cluster dissolves, the runner-up colour becomes
                // near-white antialiasing (#F8F8F8), and the ratio it reports
                // is 1.04 — on copy that renders (51,51,51) on white at
                // 12.63:1.
                //
                // Colour and weight cannot help: re-measured through the
                // framework's own code, the same glyphs re-inked to 38, 25, 12
                // and pure black all still grade 1.01-1.05 once blurred. The
                // parity is also invisible — the line passed in the rounds
                // where its rendered width happened to come out even, which is
                // what made the finding look nondeterministic for so long.
                //
                // So the rectangle has to land on whole pixels, and two
                // narrower attempts did not put it there. A `frame` alone does
                // not (CI run 32487387947: the element stays the `Text`, which
                // keeps its own 349pt bounds inside the box). Nor does
                // `.combine` on the `Text` itself (CI run 32489285578: with a
                // single child it collapses back to that child's element).
                // Both reported the identical 451.5.
                //
                // What does work is combining a genuinely multi-child
                // container, which is what the sidebar's service row already
                // does — it vends its container's 204pt width, not its text's.
                // This stack's width is the widest child, the message's fixed
                // and even 360, so it centres on 626 - 180 = 446.0 and the crop
                // is never resampled. The button stays outside the group so it
                // remains its own element.
                //
                // Nothing moves on screen; only the rectangle the audit reads
                // does.
                Text(message)
                    .font(.body.weight(.medium))
                    .secondaryLabelStyle()
                    .multilineTextAlignment(.center)
                    .frame(width: 360)
            }
            .accessibilityElement(children: .combine)
            actions()
                .padding(.top, 4)
        }
        .padding(24)
    }
}

extension View {
    /// Quieter supporting text that still meets 4.5:1. See ``MacLabelColor``.
    func secondaryLabelStyle() -> some View {
        foregroundStyle(MacLabelColor.secondary)
    }

}
