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
    /// whose text was fine failed anyway on the icon beside it. This is the
    /// same blue, dark enough to be graded on its own.
    static let accentGlyph = dynamic(
        named: "CovalentAccentGlyph",
        light: NSColor(red: 0.043, green: 0.373, blue: 0.749, alpha: 1),
        dark: NSColor(red: 0.561, green: 0.761, blue: 1.0, alpha: 1)
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
            Image(systemName: systemImage)
                .scaledSymbolFont(size: 34)
                .foregroundStyle(MacLabelColor.secondary)
                .accessibilityHidden(true)
            Text(title)
                .font(.title3.weight(.semibold))
                .foregroundStyle(.primary)
            // `.title3`, matching the one piece of secondary copy on this
            // screen that the audit has ever accepted in this colour — the
            // overview header's subtitle, same token, 15pt. At 13pt the same
            // colour was refused twice, at both regular and medium weight.
            Text(message)
                .font(.title3)
                .secondaryLabelStyle()
                .multilineTextAlignment(.center)
                .frame(maxWidth: 360)
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
