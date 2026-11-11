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
enum MacLabelColor {
    /// Supporting text that should read as quieter without becoming unreadable.
    /// Replaces `.secondary`.
    static let secondary = Color(nsColor: NSColor(name: "CovalentSecondaryLabel") { appearance in
        appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
            ? NSColor(white: 0.847, alpha: 1)
            : NSColor(white: 0.200, alpha: 1)
    })

    /// Opaque unselected sidebar content. Sidebar vibrancy can lower the
    /// rendered coverage of semantic `.primary`, especially on thin SF
    /// Symbol strokes; keep the selected row explicit in the view instead.
    static let sidebarUnselected = dynamic(
        named: "CovalentSidebarUnselected",
        light: NSColor(white: 0.120, alpha: 1),
        dark: NSColor(white: 0.920, alpha: 1)
    )

    /// Opaque blue for decorative glyphs inside combined accessibility
    /// elements. It preserves contrast on the app's system backgrounds.
    /// Kozu Cobalt 800 / 200: 7.5:1 on the light window, 9.0:1 on the dark.
    static let accentGlyph = dynamic(
        named: "CovalentAccentGlyph",
        light: srgb(0x2539B8),
        dark: srgb(0xC0DAFF)
    )

    // Status text. System `.orange`, `.green` and `.red` are tuned as fills:
    // as text on the light window they measure roughly 2:1 to 4:1. These are
    // the Kozu status ramps, one step darker in light mode than the web and
    // Android apps use, because the macOS window background is #ECECEC rather
    // than white. Each clears 6:1 on the window and 7:1 on a white card.

    /// Something needs review or did not finish. Replaces `.orange` text.
    static let warning = dynamic(
        named: "CovalentWarningLabel",
        light: srgb(0x953D00),
        dark: srgb(0xFABC00)
    )

    /// Something is connected or completed. Replaces `.green` text.
    static let success = dynamic(
        named: "CovalentSuccessLabel",
        light: srgb(0x016630),
        dark: srgb(0x05DF72)
    )

    /// Something failed or is about to be destroyed. Replaces `.red` text.
    static let danger = dynamic(
        named: "CovalentDangerLabel",
        light: srgb(0x9F0712),
        dark: srgb(0xFFA3A4)
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

    /// An opaque sRGB colour from a `0xRRGGBB` literal, so the values here read
    /// the same as the Kozu tokens they are taken from.
    static func srgb(_ hex: UInt32) -> NSColor {
        NSColor(
            srgbRed: CGFloat((hex >> 16) & 0xFF) / 255,
            green: CGFloat((hex >> 8) & 0xFF) / 255,
            blue: CGFloat(hex & 0xFF) / 255,
            alpha: 1
        )
    }
}

/// Opaque fills from the Kozu Cobalt and status ramps.
///
/// These replace `Color.blue.opacity(…)` and `Color.orange.opacity(…)`, which
/// blended with whatever sat behind them and so rendered a different colour on
/// a window, a card and a sheet. Text placed on the tints keeps at least 7:1
/// with ``MacLabelColor/secondary`` and ``MacLabelColor/accentGlyph``.
enum MacFillColor {
    /// The accent: prominent buttons, switches, checkboxes, progress bars and
    /// badges. Apply it to a control with `accentFill()`.
    ///
    /// The same value in both appearances. Kozu's dark accent is the lighter
    /// #96C4FF, but a prominent macOS button keeps its white label, and white
    /// on #96C4FF is 1.8:1. White on this is 5.3:1.
    static let accent = Color(nsColor: MacLabelColor.srgb(0x2C5AFC))

    /// Quiet blue panel behind an icon tile or a code to compare.
    static let accentTint = MacLabelColor.dynamic(
        named: "CovalentAccentTint",
        light: MacLabelColor.srgb(0xDCEAFF),
        dark: MacLabelColor.srgb(0x23378E)
    )

    /// Quiet amber panel behind details to check before agreeing.
    static let warningTint = MacLabelColor.dynamic(
        named: "CovalentWarningTint",
        light: MacLabelColor.srgb(0xFFFBEB),
        dark: MacLabelColor.srgb(0x461901)
    )
}

/// The two corner radii the app draws itself. Native controls keep their own.
enum MacCornerRadius {
    /// A large standalone panel: an empty state, a list card, a callout.
    static let card: CGFloat = 28

    /// A block nested inside a view or card: a tile, a notice, a field.
    /// Blocks have 10 to 16 points of padding, too little for ``card``.
    static let block: CGFloat = 12
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
                // Keep an even group width: the macOS contrast audit samples
                // the combined accessibility element at whole-pixel bounds.
                // Actions stay outside so they remain independently operable.
                Text(message)
                    .font(.body.weight(.medium))
                    .secondaryLabelStyle()
                    .multilineTextAlignment(.center)
                    .frame(width: 360)
            }
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier("mac.emptyState")
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

    /// Gives one native control the Kozu accent in place of the system one.
    ///
    /// Use it only on a control that *fills* with its tint and draws its own
    /// contrasting label or knob over it: a `.borderedProminent` button, a
    /// switch, a checkbox, a progress bar.
    ///
    /// It is deliberately not set once on the window. A tint is inherited, and
    /// since macOS 26 an ordinary bordered button that inherits one draws its
    /// *label* in that colour. Every secondary button in the app would turn
    /// blue, and in dark mode ``MacFillColor/accent`` as text on the window
    /// measures about 3:1.
    func accentFill() -> some View {
        tint(MacFillColor.accent)
    }
}
