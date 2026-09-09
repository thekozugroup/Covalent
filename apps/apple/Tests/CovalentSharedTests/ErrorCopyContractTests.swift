import Foundation
import Testing

@testable import CovalentShared

/// `NodeErrorCopy.swift` states a rule about itself: nothing in the app may
/// call `String(describing:)` on an error, because that produces
/// `Error Domain=NSURLErrorDomain Code=-1004 …` and no one can act on it.
/// The file then broke its own rule in three places, and `detail` is rendered
/// to people — behind a "Details" button, but rendered. A comment is not a
/// gate, so this suite is the gate.
@Suite struct ErrorCopyContractTests {
    // MARK: - The rule the file states about itself

    @Test func noSourceFileDumpsARawErrorIntoUserVisibleText() throws {
        var offenders: [String] = []
        for file in try Self.appleSources() {
            let contents = try String(contentsOf: file, encoding: .utf8)
            for offender in Self.rawErrorDumps(in: contents) {
                offenders.append("\(file.lastPathComponent):\(offender)")
            }
        }
        #expect(
            offenders.isEmpty,
            """
            `String(describing:)` on an error yields a struct dump that reaches the user through
            `NodeClientFailure.detail`. Use `ErrorDiagnostic.describe(_:)` instead:
            \(offenders.joined(separator: "\n"))
            """
        )
    }

    /// Negative control for the scan above. A scanner nobody has watched fail
    /// cannot be trusted to have failed for the right reason — and this one
    /// has to skip the doc comment in `NodeErrorCopy.swift` that *names* the
    /// forbidden call, which is exactly the kind of exemption that quietly
    /// grows into "matches nothing at all".
    @Test func theRawErrorScanCatchesAViolationAndSkipsTheCommentThatNamesIt() {
        let violating = """
            func present(_ error: Error) -> String {
                return String(describing: error)
            }
            """
        #expect(Self.rawErrorDumps(in: violating) == ["2"])

        let commentOnly = """
            /// Nothing in the app should call `String(describing:)` on an error.
            // String(describing: error) is banned here.
            func present(_ error: Error) -> String { ErrorDiagnostic.describe(error) }
            """
        #expect(Self.rawErrorDumps(in: commentOnly).isEmpty)
    }

    // MARK: - Transport and Foundation failures

    @Test func aFoundationErrorBecomesAnAuthoredLineRatherThanADump() {
        let failure = ErrorPresenter.present(
            NSError(
                domain: NSCocoaErrorDomain,
                code: 260,
                userInfo: [
                    NSFilePathErrorKey: "/Users/someone/Secret Project/notes.txt",
                    NSLocalizedDescriptionKey: "The file could not be opened.",
                ]
            )
        )
        let detail = try? #require(failure.detail)
        #expect(detail == "\(NSCocoaErrorDomain), code 260.")
        // The dump would have carried the file path into a panel a person can
        // screenshot. The authored line must not.
        #expect(failure.detail?.contains("Secret Project") == false)
        #expect(failure.detail?.contains("Error Domain=") == false)
        #expect(failure.detail?.contains("UserInfo") == false)
    }

    @Test func aTransportErrorBecomesAnAuthoredLineRatherThanADump() {
        let failure = NodeTransportCopy.describe(
            URLError(.cannotConnectToHost, userInfo: [NSURLErrorFailingURLStringErrorKey: "https://box.local:8443/api/v1/status"])
        )
        #expect(failure.detail == "Network error \(URLError.Code.cannotConnectToHost.rawValue).")
        #expect(failure.detail?.contains("box.local") == false)
        #expect(failure.recovery == .reconnect)
    }

    @Test func aDecodingFailureNamesTheFieldAndNotThePayload() throws {
        struct Reply: Decodable {
            let deviceName: String
            let protocolVersion: Int
        }
        let body = Data(#"{"deviceName":"Box","protocolVersion":"one"}"#.utf8)
        do {
            _ = try JSONDecoder().decode(Reply.self, from: body)
            Issue.record("Expected the decode to fail")
        } catch {
            let failure = NodeTransportCopy.describeDecodingFailure(error)
            let detail = try #require(failure.detail)
            #expect(detail.contains("protocolVersion"))
            // The decoder's own debugDescription quotes the payload and names
            // Swift types; neither belongs in front of a person.
            #expect(!detail.contains("Expected to decode"))
            #expect(!detail.contains("Swift."))
            #expect(!failure.summary.isEmpty)
        }
    }

    // MARK: - The engine's error-code catalog

    /// Every code the engine can emit must have authored copy.
    ///
    /// The set is reconciled against the web console's catalog in
    /// `packaging/web/app.js`, which tracks `crates/covalent-node`, and against
    /// `ApiError` in `crates/covalent-node/src/lib.rs` itself. It is a
    /// checked-in copy rather than a live read, so an edit in another tree
    /// cannot fail this suite out of nowhere — but a divergence still surfaces
    /// the moment someone adds a code to one platform and not the other.
    ///
    /// `pairing_endpoint_unavailable` is deliberately absent: the web console
    /// still maps it, but the node no longer emits it — the condition is
    /// `peer_endpoint_unavailable` now.
    @Test func everyEngineErrorCodeHasAuthoredCopy() {
        var unmapped: [String] = []
        for code in Self.engineErrorCodes where !Self.isMapped(code) {
            unmapped.append(code)
        }
        #expect(
            unmapped.isEmpty,
            "These codes fall through to status-shaped copy instead of being authored: \(unmapped)"
        )
        #expect(Self.engineErrorCodes.count == 77, "The reference set changed; reconcile it deliberately.")
    }

    /// Negative control for the mapping probe: a code that is definitely not
    /// in the catalog must be reported as unmapped, or the check above passes
    /// for every possible input.
    @Test func theMappingProbeReportsAnAbsentCodeAsUnmapped() {
        #expect(!Self.isMapped("a_code_no_engine_will_ever_emit"))
        #expect(Self.isMapped("insufficient_storage"))
    }

    @Test func anUnknownCodeDegradesToAnAuthoredSentenceRatherThanServerText() {
        let operatorText = "provider lease 7f3c-… expired while draining shard 4"
        let failure = NodeAPIErrorCopy.describe(
            status: 409,
            code: "some_code_added_after_this_build",
            message: operatorText,
            retryable: false
        )
        #expect(!failure.summary.isEmpty)
        #expect(!failure.summary.contains(operatorText))
        #expect(!failure.summary.contains("some_code_added_after_this_build"))
        // The lead is authored; the machine-readable half stays in diagnostics
        // where a bug report can still reach it.
        #expect(failure.detail?.contains("some_code_added_after_this_build") == true)
    }

    @Test func theNewPairingAndClaimCodesCarryTheRightRecovery() {
        // Deliberately not asserting `.recovery == .none` anywhere here: it is
        // the fallback's own value for these statuses, so such an assertion
        // passes just as happily when the mapping has been deleted.
        #expect(
            NodeAPIErrorCopy.describe(status: 502, code: "pairing_peer_unreachable", message: "", retryable: false)
                .recovery == .chooseAnotherDevice
        )
        #expect(
            NodeAPIErrorCopy.describe(status: 403, code: "pairing_rejected", message: "", retryable: false)
                .recovery == .chooseAnotherDevice
        )
        #expect(
            NodeAPIErrorCopy.describe(status: 503, code: "claim_certificate_unavailable", message: "", retryable: true)
                .recovery == .retry
        )
    }

    @Test func folderSyncAndAddressRefreshFailuresGiveExactNextSteps() {
        let cases: [(Int, String, Bool, String, RecoveryHint)] = [
            (
                400,
                "invalid_peer_address",
                false,
                "Enter the device address as a numeric IP address and port, such as 192.168.1.20:8787, then try again.",
                .none
            ),
            (
                409,
                "peer_address_changed",
                false,
                "The saved device address changed before this update finished. Refresh the saved device, then try again.",
                .retry
            ),
            (
                502,
                "peer_address_unreachable",
                true,
                "Covalent could not authenticate the trusted device at the new address. Check the address and network connection, then try again.",
                .retry
            ),
            (
                409,
                "folder_sync_unavailable",
                false,
                "Folder sync is unavailable on this device. Check that folder sync is installed and the shared folder is available, then try again.",
                .retry
            ),
            (
                503,
                "folder_sync_busy",
                true,
                "Another folder change is still in progress. Try again shortly.",
                .retry
            ),
            (
                409,
                "folder_sync_needs_attention",
                false,
                "Folder sync needs attention before it can continue. Check the folder status, then try again.",
                .retry
            ),
        ]
        for (status, code, retryable, summary, recovery) in cases {
            let failure = NodeAPIErrorCopy.describe(
                status: status,
                code: code,
                message: "untrusted machine detail",
                retryable: retryable
            )
            #expect(failure.summary == summary)
            #expect(failure.recovery == recovery)
            #expect(!failure.summary.contains("untrusted machine detail"))
        }
    }

    @Test func folderSyncAPIProductionCodesStayInTheAuthoredCatalog() throws {
        let testFile = URL(fileURLWithPath: #filePath)
        let repository = testFile
            .deletingLastPathComponent()   // CovalentSharedTests
            .deletingLastPathComponent()   // Tests
            .deletingLastPathComponent()   // apps/apple
            .deletingLastPathComponent()   // apps
            .deletingLastPathComponent()   // repository
        let source = try String(
            contentsOf: repository.appending(path: "crates/covalent-node/src/sync_api.rs"),
            encoding: .utf8
        )
        let direct = try Self.captures(#"code:\s*"([a-z0-9_]+)""#, in: source)
        var emitted = Set(direct)
        for block in try Self.captures(#"code:\s*if\b([\s\S]*?),\s*message:"#, in: source) {
            emitted.formUnion(try Self.captures(#""([a-z0-9_]+)""#, in: block))
        }
        let expected: Set<String> = [
            "folder_sync_unavailable",
            "folder_sync_busy",
            "folder_sync_needs_attention",
            "invalid_peer_address",
            "peer_address_changed",
            "peer_address_unreachable",
        ]
        #expect(emitted == expected, "Reconcile new folder-sync API error codes deliberately.")
        #expect(emitted.allSatisfy(Self.isMapped))
    }

    /// The first-run codes are what a person meets before anything else works,
    /// so each says something different about what to do next. Asserting the
    /// distinction, not just the presence, is what stops them collapsing into
    /// one another.
    @Test func theFirstRunClaimCodesAreToldApart() {
        func summary(_ code: String, _ status: Int) -> String {
            NodeAPIErrorCopy.describe(status: status, code: code, message: "", retryable: false).summary
        }
        let incorrect = summary("claim_code_incorrect", 401)
        let expired = summary("claim_window_expired", 410)
        let exhausted = summary("claim_window_exhausted", 410)
        let alreadyOwned = summary("claim_unavailable", 409)

        #expect(Set([incorrect, expired, exhausted, alreadyOwned]).count == 4)
        // A mistyped code invites another attempt; a closed window must not.
        #expect(incorrect.localizedCaseInsensitiveContains("again"))
        #expect(expired.localizedCaseInsensitiveContains("restart"))
        #expect(exhausted.localizedCaseInsensitiveContains("restart"))
        #expect(!exhausted.localizedCaseInsensitiveContains("try again"))
        // Someone else may have set this server up; say so rather than
        // implying the person did something wrong.
        #expect(alreadyOwned.localizedCaseInsensitiveContains("already"))
    }

    @Test func aBareSwiftErrorDoesNotBecomeFoundationsSynthesizedSentence() {
        struct UnexpectedState: Error {}
        let failure = ErrorPresenter.present(UnexpectedState())
        // Foundation renders this one as "The operation couldn't be
        // completed. (… error 1.)" — a struct dump in sentence clothing.
        #expect(!failure.summary.contains("error 1"))
        #expect(!failure.summary.contains("UnexpectedState"))
        #expect(failure.summary == "Covalent couldn't finish that. Try again in a moment.")
        #expect(failure.detail?.contains("UnexpectedState") == true)
    }

    @Test func aFoundationErrorKeepsItsOwnSentence() {
        let failure = ErrorPresenter.present(
            NSError(
                domain: NSCocoaErrorDomain,
                code: 640,
                userInfo: [NSLocalizedDescriptionKey: "The file is too large."]
            )
        )
        #expect(failure.summary == "The file is too large.")
    }

    // MARK: - Helpers

    /// Returns the 1-based line numbers on which `String(describing:` appears
    /// outside a comment.
    private static func rawErrorDumps(in contents: String) -> [String] {
        contents.components(separatedBy: .newlines).enumerated().compactMap { index, line in
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            guard !trimmed.hasPrefix("//") else { return nil }
            guard line.contains("String(describing:") else { return nil }
            return String(index + 1)
        }
    }

    /// `catalog` is private, so mapping is probed through the public surface.
    /// Status 418 is used because its fallback text appears nowhere in the
    /// catalog, so "summary equals the fallback" means "not mapped".
    private static func isMapped(_ code: String) -> Bool {
        let probe = NodeAPIErrorCopy.describe(status: 418, code: code, message: "", retryable: false)
        let fallback = NodeAPIErrorCopy.describe(
            status: 418,
            code: "a_code_no_engine_will_ever_emit",
            message: "",
            retryable: false
        )
        return probe.summary != fallback.summary
    }

    private static func captures(_ pattern: String, in source: String) throws -> [String] {
        let expression = try NSRegularExpression(pattern: pattern)
        let range = NSRange(source.startIndex..., in: source)
        return expression.matches(in: source, range: range).compactMap { result in
            guard let capture = Range(result.range(at: 1), in: source) else { return nil }
            return String(source[capture])
        }
    }

    private static func appleSources() throws -> [URL] {
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
        #expect(found.count > 10, "Could not locate the Apple sources at \(sources.path)")
        return found
    }

    private static let engineErrorCodes = [
        "archive_digest_mismatch",
        "archive_metadata_required",
        "archive_processing_timeout",
        "archive_processing_too_slow",
        "archive_upload_headers_required",
        "authentication_required",
        "backup_corrupt",
        "backup_unavailable",
        "claim_certificate_unavailable",
        "claim_code_incorrect",
        "claim_rate_limited",
        "claim_unavailable",
        "claim_window_exhausted",
        "claim_window_expired",
        "confirmation_required",
        "duplicate_archive_entry",
        "folder_sync_busy",
        "folder_sync_needs_attention",
        "folder_sync_unavailable",
        "insufficient_storage",
        "internal_error",
        "invalid_archive",
        "invalid_archive_entry",
        "invalid_archive_metadata",
        "invalid_authorized_root",
        "invalid_certificate",
        "invalid_content_type",
        "invalid_contract",
        "invalid_job_id",
        "invalid_json",
        "invalid_page_cursor",
        "invalid_page_limit",
        "invalid_peer_address",
        "invalid_provider_address",
        "invalid_restore_execute_request",
        "invalid_restore_plan_id",
        "invalid_streamed_restore_plan",
        "invalid_target_inventory",
        "invalid_upload_digest",
        "invalid_upload_length",
        "invalid_upload_offset",
        "invitation_unavailable",
        "job_active",
        "job_cancelled",
        "job_conflict",
        "job_not_complete",
        "job_not_found",
        "job_paused",
        "method_not_allowed",
        "node_busy",
        "node_state_locked",
        "not_authorized",
        "pairing_endpoint_mismatch",
        "pairing_peer_unreachable",
        "pairing_rejected",
        "peer_endpoint_unavailable",
        "peer_address_changed",
        "peer_address_unreachable",
        "protocol_incompatible",
        "provider_binding_mismatch",
        "recovery_confirmation_required",
        "recovery_not_configured",
        "resource_limit",
        "restore_conflict",
        "restore_plan_mismatch",
        "restore_plan_not_found",
        "route_not_found",
        "source_changed",
        "source_unreadable",
        "target_inventory_digest_mismatch",
        "target_inventory_incomplete",
        "target_inventory_job_mismatch",
        "target_inventory_not_found",
        "target_inventory_offset_mismatch",
        "target_inventory_page_mismatch",
        "target_inventory_required",
        "unsafe_restore_path",
    ]
}
