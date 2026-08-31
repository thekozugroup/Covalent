import Foundation
import Testing

@testable import CovalentShared

/// Holds the line on the words the apps show people.
///
/// Android ran this sweep once and went from 62 offenders to 0; Apple ran it
/// later and went from 132 to 0. Neither number stays at 0 on its own — the
/// vocabulary that leaks is the vocabulary the code is written in, so every
/// new screen re-imports it unless something objects. `IOSHomeView` showed a
/// reader the literal string "Tier 2" for months.
///
/// This scans the view sources for user-facing string literals containing
/// engineering nouns. It is deliberately a *word list*, not a judgement: it
/// cannot tell good copy from bad, only that a word which has no meaning
/// outside this repository has reached a `Text(…)`.
@Suite struct UserFacingCopyTests {
    @Test func noViewShowsAnEngineeringNounToAPerson() throws {
        var offenders: [String] = []
        var literalsScanned = 0
        for file in try Self.viewSources() {
            let contents = try String(contentsOf: file, encoding: .utf8)
            for (line, text, word) in Self.offendingLiterals(in: contents) {
                offenders.append("\(file.lastPathComponent):\(line) [\(word)] \(text)")
            }
            literalsScanned += Self.userFacingLiterals(in: contents).count
        }
        // A scan that stopped finding literals would pass forever.
        #expect(literalsScanned > 200, "Only \(literalsScanned) user-facing literals found; the scan is broken.")
        #expect(
            offenders.isEmpty,
            """
            These strings show a person a word that only means something inside this repository.
            Use the shared glossary — backup server, storage device, extra copy, backup — which
            `apps/android/app/src/main/res/values/strings.xml` also follows:
            \(offenders.joined(separator: "\n"))
            """
        )
    }

    /// Negative control. A word list that matches nothing is indistinguishable
    /// from a clean codebase, and the exclusions below (identifiers, symbol
    /// names, filenames) are exactly the kind that quietly grow to cover
    /// everything.
    @Test func theScanCatchesJargonAndStillIgnoresIdentifiers() {
        let violating = """
            Text("Tier 2 protects only the folders you choose.")
            Text("Revocation creates a durable tombstone for the provider.")
            """
        let found = Self.offendingLiterals(in: violating)
        #expect(found.count == 2)
        #expect(found.map(\.word).contains("Tier 2"))

        // A modifier on the same line must not blind the scan. An earlier
        // version skipped any line mentioning `accessibilityIdentifier`, which
        // is the ordinary SwiftUI idiom — copy could hide behind it.
        let behindAModifier = #"Text("Replica snapshot on the provider node.").accessibilityIdentifier("probe")"#
        #expect(Self.offendingLiterals(in: behindAModifier).count == 1)

        let clean = """
            // The node writes node-ready.json; this comment is not user-facing.
            Image(systemName: "externaldrive.badge.plus")
            Text("snapshot.\\(snapshot.id.uuidString)")
                .accessibilityIdentifier("snapshot.\\(snapshot.id.uuidString)")
            Text("Choose a folder and keep the first encrypted backup on this Mac.")
            """
        #expect(Self.offendingLiterals(in: clean).isEmpty)
    }

    /// The shared target is where every error summary a person reads is
    /// authored. A scan that covered only the two view targets reported zero
    /// offenders while `CovalentAppModel` was telling people about "replica
    /// devices" and `AppleArchiveTransfer` about what "the node returned".
    @Test func theScanCoversTheSharedTargetWhereErrorCopyLives() throws {
        let scanned = try Self.viewSources().map(\.lastPathComponent)
        #expect(scanned.contains("NodeErrorCopy.swift"))
        #expect(scanned.contains("CovalentAppModel.swift"))
        #expect(scanned.contains("MacOverviewView.swift"))
        #expect(scanned.contains("IOSHomeView.swift"))
    }

    @Test func appleSurfacesNameBothBackupIdentifiersAndDescribeIOSSupport() throws {
        let sources = try Dictionary(uniqueKeysWithValues: Self.viewSources().map {
            ($0.lastPathComponent, try! String(contentsOf: $0, encoding: .utf8))
        })
        for name in ["MacBackupsView.swift", "IOSBackupsView.swift"] {
            let source = try #require(sources[name])
            #expect(source.contains("Backup ID"))
            #expect(source.contains("Backup version ID"))
        }
        let settings = try #require(sources["IOSSettingsView.swift"])
        #expect(settings.contains("Preview — not released"))
        #expect(!settings.contains("PlatformTier.tier2.label"))
    }

    @Test func macRestoreLabelsOnlyActionsTheExecutorCanPerform() throws {
        let source = try String(contentsOf: Self.source(named: "MacBackupsView.swift"), encoding: .utf8)
        #expect(source.contains("Skip existing file"))
        #expect(source.contains("Create renamed copy"))
        #expect(source.contains("Replace existing file"))
        #expect(source.contains("Replace and Restore"))
        #expect(source.contains("hasSignedTargetInventory"))
        #expect(!source.contains("Blocked conflict"))
    }

    // MARK: - Scanner

    /// Words that mean something to this repository and nothing to a reader.
    private static let jargon: [String] = [
        "Tier 1", "Tier 2",
        "node", "nodes", "provider", "providers", "replica", "replicas",
        "snapshot", "snapshots", "tombstone", "transcript", "transcripts",
        "endpoint", "endpoints", "epoch", "opaque locator",
        "durable", "resumable", "bearer", "mutually signed", "mutually finalized",
        "local API token", "covalent-node", "node-ready.json", "local-api-token",
    ]

    /// Literals that are genuinely technical and correctly shown as-is.
    private static let allowed: Set<String> = [
        // The name of the file the export actually writes.
        "Covalent Settings.json",
    ]

    private static func offendingLiterals(in contents: String) -> [(line: Int, text: String, word: String)] {
        userFacingLiterals(in: contents).compactMap { entry in
            guard !allowed.contains(entry.text) else { return nil }
            let haystack = entry.text.replacingOccurrences(
                of: #"\\\([^)]*\)"#,
                with: " ",
                options: .regularExpression
            )
            for word in jargon where haystack.range(of: "\\b\(NSRegularExpression.escapedPattern(for: word))\\b",
                                                    options: [.regularExpression, .caseInsensitive]) != nil {
                return (entry.line, entry.text, word)
            }
            return nil
        }
    }

    /// Argument labels whose literal is a name for the machine, not for a
    /// person. Only the literal that *directly follows* one is skipped —
    /// skipping the whole line would blind the scan to
    /// `Text("…").accessibilityIdentifier("…")`, which is the ordinary SwiftUI
    /// idiom and would have let any copy hide behind a modifier.
    private static let machineArgumentPrefixes = [
        "accessibilityIdentifier(", "systemName:", "systemImage:", "identifier:",
        "forKey:", "named:", "withIdentifier:", "forInfoDictionaryKey:", "value:",
        "environment[", "setValue(", "addValue(", "forHTTPHeaderField:", "queryItem",
    ]

    /// String literals a person could plausibly read.
    ///
    /// Excluded, because they are code rather than copy: comment lines, the
    /// literal directly after a machine-name argument label, environment
    /// variable names, URLs, and bare dotted identifiers.
    private static func userFacingLiterals(in contents: String) -> [(line: Int, text: String)] {
        var results: [(Int, String)] = []
        for (index, line) in contents.components(separatedBy: .newlines).enumerated() {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            guard !trimmed.hasPrefix("//"), !trimmed.hasPrefix("///"), !trimmed.hasPrefix("*") else { continue }
            for (text, preceding) in stringLiterals(in: line) {
                let context = preceding.trimmingCharacters(in: .whitespaces)
                guard !machineArgumentPrefixes.contains(where: { context.hasSuffix($0) }) else { continue }
                guard text.count >= 4 else { continue }
                // Machine strings: URL paths, file names, keychain keys. Shape,
                // not length — "Providers" as a button title stays in scope,
                // while "api/v1/providers" and "snapshot-history.json" do not.
                guard !isMachineString(text) else { continue }
                guard !text.hasPrefix("http"), !text.hasPrefix("x-apple") else { continue }
                guard text.range(of: #"^[A-Z][A-Z0-9_]+$"#, options: .regularExpression) == nil else { continue }
                guard text.range(of: #"^[a-z][a-zA-Z0-9]*(\.[a-zA-Z0-9\\()_.]*)*$"#, options: .regularExpression) == nil
                else { continue }
                results.append((index + 1, text))
            }
        }
        return results
    }

    /// Pulls the double-quoted literals out of one line, each with the text
    /// that preceded it, honouring backslash escapes so an escaped quote does
    /// not end a literal early.
    private static func stringLiterals(in line: String) -> [(text: String, preceding: String)] {
        var literals: [(String, String)] = []
        var current: String?
        var preceding = ""
        var escaped = false
        for character in line {
            if var open = current {
                if escaped {
                    open.append(character)
                    escaped = false
                    current = open
                } else if character == "\\" {
                    // Kept, not dropped: `\(…)` interpolations are stripped
                    // downstream, and that only works if the backslash that
                    // marks them survives this parse.
                    open.append(character)
                    escaped = true
                    current = open
                } else if character == "\"" {
                    literals.append((open, preceding))
                    preceding = ""
                    current = nil
                } else {
                    open.append(character)
                    current = open
                }
            } else if character == "\"" {
                current = ""
            } else {
                preceding.append(character)
            }
        }
        return literals
    }

    /// A path, file name or key: no whitespace, and punctuation that only
    /// appears in identifiers.
    private static func isMachineString(_ text: String) -> Bool {
        guard !text.contains(where: \.isWhitespace) else { return false }
        guard text.range(of: #"^[A-Za-z0-9._/-]+$"#, options: .regularExpression) != nil else { return false }
        return text.contains("/") || text.contains("-") || text.contains(".")
    }

    private static func viewSources() throws -> [URL] {
        let root = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()   // CovalentSharedTests
            .deletingLastPathComponent()   // Tests
            .deletingLastPathComponent()   // apps/apple
            .appending(path: "Sources", directoryHint: .isDirectory)
        var found: [URL] = []
        // `CovalentShared` matters as much as the view targets: every error
        // summary a person reads is authored in `NodeErrorCopy.swift`, and a
        // scan that skipped it would have let the whole error catalog drift
        // back into engine vocabulary while reporting zero offenders.
        for platform in ["CovalentMac", "CovalentIOS", "CovalentShared"] {
            let directory = root.appending(path: platform, directoryHint: .isDirectory)
            let names = try FileManager.default.contentsOfDirectory(atPath: directory.path)
            found += names
                .filter { $0.hasSuffix(".swift") }
                // Not a view: it launches and supervises the bundled helper, so
                // its strings are paths and process arguments by definition.
                .filter { $0 != "LocalNodeManager.swift" }
                .map { directory.appending(path: $0, directoryHint: .notDirectory) }
        }
        #expect(found.count > 10, "Could not locate the Apple view sources at \(root.path)")
        return found
    }

    private static func source(named name: String) -> URL {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .appending(path: "Sources", directoryHint: .isDirectory)
            .appending(path: "CovalentMac", directoryHint: .isDirectory)
            .appending(path: name, directoryHint: .notDirectory)
    }
}
