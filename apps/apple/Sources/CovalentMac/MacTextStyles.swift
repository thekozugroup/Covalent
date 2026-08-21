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
/// | token | backdrop | ratio |
/// | --- | --- | --- |
/// | secondary light #4B4B4B | windowBackground #ECECEC | 7.4:1 |
/// | secondary light #4B4B4B | controlBackground #FFFFFF | 8.7:1 |
/// | secondary dark #C4C4C4 | windowBackground #323232 | 8.3:1 |
/// | secondary dark #C4C4C4 | controlBackground #1E1E1E | 10.5:1 |
///
/// The margin is deliberate. The audit measures rendered pixels, not declared
/// colours, so anti-aliasing on a 1x CI display eats some of any ratio computed
/// on paper; landing at 7:1+ leaves room for that without relying on it.
///
/// iOS is intentionally left on `.secondary`: `UIColor.secondaryLabel` is 60%
/// alpha, which clears 4.5:1 on the iOS backgrounds, and the iOS audit gate is
/// green. Changing it would risk a passing gate for no accessibility gain.
enum MacLabelColor {
    /// Body and title text. Replaces `.primary`.
    static let primary = Color(nsColor: NSColor(name: "CovalentPrimaryLabel") { appearance in
        appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
            ? NSColor(white: 0.937, alpha: 1)
            : NSColor(white: 0.102, alpha: 1)
    })

    /// Supporting text that should read as quieter without becoming unreadable.
    /// Replaces `.secondary`.
    static let secondary = Color(nsColor: NSColor(name: "CovalentSecondaryLabel") { appearance in
        appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
            ? NSColor(white: 0.769, alpha: 1)
            : NSColor(white: 0.294, alpha: 1)
    })
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

    var body: some View {
        VStack(spacing: 10) {
            Image(systemName: systemImage)
                .scaledSymbolFont(size: 34)
                .foregroundStyle(MacLabelColor.secondary)
                .accessibilityHidden(true)
            Text(title)
                .font(.title3.weight(.semibold))
                .primaryLabelStyle()
            Text(message)
                .font(.callout)
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

    /// Leading text. Opaque, so vibrancy cannot blend it away. See ``MacLabelColor``.
    func primaryLabelStyle() -> some View {
        foregroundStyle(MacLabelColor.primary)
    }
}
