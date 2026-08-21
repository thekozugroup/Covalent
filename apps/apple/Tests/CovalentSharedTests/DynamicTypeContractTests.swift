import Foundation
import Testing

@testable import CovalentShared

/// The system accessibility audit that gates `Scripts/ios-ui-test.sh` checks
/// clipping, contrast and labels. It does **not** check whether text scales,
/// so a hardcoded `.font(.system(size: 42))` sails straight through a green
/// audit while silently ignoring the reader's text-size setting.
///
/// This suite closes that hole by reading the view sources directly: any
/// absolute point size reintroduced into a SwiftUI view fails the build.
@Suite struct DynamicTypeContractTests {
    /// Files allowed to name an absolute point size, because they are the
    /// mechanism that makes absolute sizes scale.
    private static let scalingPrimitives: Set<String> = ["ScaledSymbol.swift"]

    private static let absoluteFontPattern = ".font(.system(size:"

    @Test func noViewFreezesAFontAtAnAbsolutePointSize() throws {
        var offenders: [String] = []
        for file in try Self.viewSources() {
            guard !Self.scalingPrimitives.contains(file.lastPathComponent) else { continue }
            let contents = try String(contentsOf: file, encoding: .utf8)
            for (index, line) in contents.components(separatedBy: .newlines).enumerated()
            where line.contains(Self.absoluteFontPattern) {
                offenders.append("\(file.lastPathComponent):\(index + 1) \(line.trimmingCharacters(in: .whitespaces))")
            }
        }
        #expect(
            offenders.isEmpty,
            """
            These views freeze a font at an absolute point size, so they ignore Dynamic Type.
            Use a semantic style (.title, .largeTitle) or `.scaledSymbolFont(size:)` instead:
            \(offenders.joined(separator: "\n"))
            """
        )
    }

    /// A decorative glyph sized with `scaledSymbolFont` must not be pinned
    /// inside an unscaled `.frame(width:height:)`, or it clips at large
    /// accessibility sizes — which the audit *would* catch, in CI, later.
    @Test func scaledSymbolsAreNotPinnedInsideFixedFrames() throws {
        var offenders: [String] = []
        for file in try Self.viewSources() {
            guard !Self.scalingPrimitives.contains(file.lastPathComponent) else { continue }
            let lines = try String(contentsOf: file, encoding: .utf8).components(separatedBy: .newlines)
            for (index, line) in lines.enumerated() where line.contains(".scaledSymbolFont(") {
                let following = lines[(index + 1)..<min(index + 5, lines.count)]
                if following.contains(where: { $0.contains(".frame(width:") && $0.contains("height:") }) {
                    offenders.append("\(file.lastPathComponent):\(index + 1)")
                }
            }
        }
        #expect(
            offenders.isEmpty,
            """
            Use `.scaledSymbolFrame(_:)` so the container grows with the glyph:
            \(offenders.joined(separator: "\n"))
            """
        )
    }

    /// Every recovery hint that a person can act on must name its button, and
    /// the two that cannot must not — otherwise an alert either dead-ends on
    /// "OK" or offers a button that does nothing.
    @Test func everyActionableRecoveryHintNamesItsButton() {
        for hint in RecoveryHint.allCases {
            let title = AppAlert(title: "t", message: "m", recovery: hint).recoveryActionTitle
            switch hint {
            case .none, .freeUpSpace:
                #expect(title == nil, "\(hint) offers a button but has no action to perform")
            default:
                #expect(title?.isEmpty == false, "\(hint) is actionable but names no button")
            }
        }
    }

    private static func viewSources() throws -> [URL] {
        let sources = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()   // CovalentSharedTests
            .deletingLastPathComponent()   // Tests
            .deletingLastPathComponent()   // apps/apple
            .appending(path: "Sources", directoryHint: .isDirectory)
        let found = try FileManager.default
            .subpathsOfDirectory(atPath: sources.path)
            .filter { $0.hasSuffix(".swift") }
            .map { sources.appending(path: $0, directoryHint: .notDirectory) }
        // A silently empty scan would make this suite pass forever.
        #expect(found.count > 10, "Could not locate the Apple view sources at \(sources.path)")
        return found
    }
}
