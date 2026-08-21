import Foundation

/// What a person can actually *do* about a failure.
///
/// The client boundary decides this once, from the transport error or the
/// engine's machine-readable `code`. Every alert surface renders the matching
/// button, so a recoverable failure never dead-ends on a lone "OK".
public enum RecoveryHint: String, Equatable, Sendable, CaseIterable {
    /// Nothing the person can do beyond acknowledging the message.
    case none
    /// The same operation is worth attempting again as-is.
    case retry
    /// The app needs to be pointed at (or re-authorized against) the server.
    case reconnect
    /// The device itself is offline; send them to system network settings.
    case checkNetworkSettings
    /// This peer will not work; send them back to the device list.
    case chooseAnotherDevice
    /// The folder grant is stale or unreadable; re-pick the folder.
    case chooseFolderAgain
    /// The signed restore preview is stale; build a fresh one.
    case previewRestoreAgain
    /// The server is out of room. Informational: the fix is off-device.
    case freeUpSpace
}

/// A failure rendered for a person, with the engineering detail kept aside.
///
/// `summary` is what leads the UI. `detail` is the raw underlying text —
/// `NSError` descriptions, decoder dumps, engine messages — which stays
/// available for a "Details" disclosure and for logs, but never leads.
public struct NodeClientFailure: Equatable, Sendable {
    public let summary: String
    public let detail: String?
    public let recovery: RecoveryHint

    public init(summary: String, detail: String? = nil, recovery: RecoveryHint = .none) {
        self.summary = summary
        self.detail = detail
        self.recovery = recovery
    }
}

extension NodeClientFailure: ExpressibleByStringLiteral {
    /// Lets the many already-plain-English throw sites read as plain strings.
    public init(stringLiteral value: String) {
        self.init(summary: value)
    }
}

/// The single place any Apple surface turns an `Error` into words.
///
/// Nothing in the app should call `String(describing:)` on an error: that
/// yields `Error Domain=NSURLErrorDomain Code=-1004 …`, which no one can act
/// on. This presenter always returns a plain-English lead, and keeps the raw
/// text separately so a "Details" disclosure or a log can still carry it.
public enum ErrorPresenter {
    public static func present(_ error: Error) -> NodeClientFailure {
        if let clientError = error as? NodeClientError {
            return NodeClientFailure(
                summary: clientError.errorDescription ?? genericSummary,
                detail: clientError.diagnosticDetail,
                recovery: clientError.recoveryHint
            )
        }
        if error is URLError {
            return NodeTransportCopy.describe(error)
        }
        if let described = (error as? LocalizedError)?.errorDescription {
            return NodeClientFailure(summary: described, detail: nil, recovery: .none)
        }
        // Last resort: a Foundation or POSIX error with no copy of its own.
        // `localizedDescription` is at least a sentence; the raw dump is not.
        return NodeClientFailure(
            summary: error.localizedDescription.isEmpty ? genericSummary : error.localizedDescription,
            detail: String(describing: error),
            recovery: .retry
        )
    }

    /// Plain-English lead text. Safe to show to anyone.
    public static func summary(for error: Error) -> String { present(error).summary }

    /// Technical detail for a disclosure or a log. Never lead with it.
    public static func detail(for error: Error) -> String? { present(error).detail }

    private static let genericSummary = "Covalent couldn't finish that. Try again in a moment."
}

/// Maps `URLError` and other transport failures to copy a non-technical
/// person can act on, keeping the underlying description as diagnostics.
public enum NodeTransportCopy {
    public static func describe(_ error: Error) -> NodeClientFailure {
        let detail = String(describing: error)
        guard let urlError = error as? URLError else {
            return NodeClientFailure(
                summary: "Covalent could not reach your backup server.",
                detail: detail,
                recovery: .retry
            )
        }
        let (summary, recovery) = copy(for: urlError.code)
        return NodeClientFailure(summary: summary, detail: detail, recovery: recovery)
    }

    /// A JSON body that did not match protocol 1. The decoder dump is useless
    /// to a person, so it becomes diagnostics and the summary explains the
    /// only realistic cause: the two sides are on different versions.
    public static func describeDecodingFailure(_ error: Error) -> NodeClientFailure {
        NodeClientFailure(
            summary: "Covalent could not understand your backup server's reply. "
                + "This usually means the app and the server are running different versions.",
            detail: String(describing: error),
            recovery: .none
        )
    }

    private static func copy(for code: URLError.Code) -> (String, RecoveryHint) {
        switch code {
        case .notConnectedToInternet:
            (
                "This device isn't connected to a network. Reconnect to Wi-Fi, then try again.",
                .checkNetworkSettings
            )
        case .networkConnectionLost:
            (
                "The network connection dropped. Covalent saved its place, so you can pick up where it left off.",
                .retry
            )
        case .dataNotAllowed:
            (
                "Covalent isn't allowed to use cellular data. Join a Wi-Fi network, or allow cellular data for Covalent.",
                .checkNetworkSettings
            )
        case .internationalRoamingOff:
            ("Data roaming is turned off, so Covalent can't reach your backup server.", .checkNetworkSettings)
        case .callIsActive:
            ("A phone call is using the network. Try again once the call ends.", .retry)
        case .timedOut:
            (
                "Your backup server didn't answer in time. Make sure it's turned on and awake, then try again.",
                .retry
            )
        case .cannotConnectToHost, .cannotFindHost, .dnsLookupFailed:
            (
                "Covalent couldn't reach your backup server at this address. "
                    + "Check that it's running and on the same network as this device.",
                .reconnect
            )
        case .serverCertificateUntrusted,
             .serverCertificateHasUnknownRoot,
             .serverCertificateHasBadDate,
             .serverCertificateNotYetValid,
             .clientCertificateRejected,
             .clientCertificateRequired:
            (
                "Covalent doesn't trust this server's security certificate, so it stopped before sending anything. "
                    + "Choose the server's certificate again in setup.",
                .reconnect
            )
        case .secureConnectionFailed:
            (
                "Covalent couldn't open a secure connection to your backup server. "
                    + "Check that the server is reachable and its certificate is still valid.",
                .reconnect
            )
        case .appTransportSecurityRequiresSecureConnection:
            (
                "Covalent will only talk to another device over a secure connection. Use an HTTPS address.",
                .reconnect
            )
        case .userAuthenticationRequired:
            ("Your backup server asked Covalent to sign in again.", .reconnect)
        case .badServerResponse, .zeroByteResource, .cannotParseResponse:
            (
                "Your backup server sent back something Covalent couldn't read. Try again in a moment.",
                .retry
            )
        case .cancelled:
            ("Covalent stopped this request before it finished.", .retry)
        case .badURL, .unsupportedURL:
            ("That backup server address isn't a complete web address.", .reconnect)
        case .cannotCreateFile, .cannotWriteToFile, .cannotRemoveFile, .cannotMoveFile, .cannotOpenFile:
            (
                "Covalent ran out of room to stage this transfer on this device. Free up storage, then try again.",
                .freeUpSpace
            )
        case .fileDoesNotExist, .fileIsDirectory, .noPermissionsToReadFile:
            ("Covalent lost access to a file it was working with. Choose the folder again.", .chooseFolderAgain)
        case .resourceUnavailable:
            ("What Covalent asked for isn't available on your backup server right now.", .retry)
        case .redirectToNonExistentLocation, .httpTooManyRedirects:
            (
                "Something on your network redirected Covalent away from your backup server. "
                    + "Check the address in setup.",
                .reconnect
            )
        default:
            ("Covalent could not reach your backup server.", .retry)
        }
    }
}

/// Maps the engine's machine-readable error codes to plain-English copy.
///
/// The catalog is keyed on `code` — never on the server's free-text `message`,
/// which is written for operators and is kept only as diagnostics. Codes are
/// the ones emitted by `crates/covalent-node`; anything unrecognised falls
/// back to status-shaped copy so a newer server never leaks raw text.
public enum NodeAPIErrorCopy {
    public static func describe(
        status: Int,
        code: String,
        message: String,
        retryable: Bool
    ) -> NodeClientFailure {
        let detail = "HTTP \(status) · \(code) · \(message)"
        if let known = catalog[code] {
            return NodeClientFailure(summary: known.0, detail: detail, recovery: known.1)
        }
        let (summary, recovery) = fallback(status: status, retryable: retryable)
        return NodeClientFailure(summary: summary, detail: detail, recovery: recovery)
    }

    private static func fallback(status: Int, retryable: Bool) -> (String, RecoveryHint) {
        switch status {
        case 401, 403:
            ("Your backup server refused this request. Reconnect this app to it.", .reconnect)
        case 404:
            ("Covalent couldn't find that on your backup server.", .none)
        case 408, 429:
            ("Your backup server is busy. Try again in a moment.", .retry)
        case 413:
            ("That's larger than your backup server accepts in one go. Try a smaller folder.", .none)
        case 507:
            ("Your backup server is out of space.", .freeUpSpace)
        case 500...599:
            (
                "Something went wrong on your backup server. Try again; if it keeps happening, check its logs.",
                .retry
            )
        default:
            retryable
                ? ("Your backup server couldn't complete that request. Try again in a moment.", .retry)
                : ("Your backup server couldn't complete that request.", .none)
        }
    }

    private static let catalog: [String: (String, RecoveryHint)] = [
        // Authorization
        "authentication_required": (
            "This app is no longer signed in to your backup server. Reconnect it to continue.", .reconnect
        ),
        "not_authorized": (
            "Your backup server refused this request. Reconnect this app to it.", .reconnect
        ),
        "invalid_certificate": (
            "Covalent doesn't trust this server's security certificate. Choose the certificate again in setup.",
            .reconnect
        ),

        // Capacity
        "insufficient_storage": (
            "Your backup server is out of space. Free some up, or choose a different device to keep this copy.",
            .freeUpSpace
        ),
        "resource_limit": (
            "This is larger than your backup server can handle in one pass. Try backing up a smaller folder.",
            .none
        ),

        // Job lifecycle
        "job_paused": ("This job is paused. Resume it to carry on where it left off.", .none),
        "job_cancelled": ("This job was cancelled, and the progress it had saved was discarded.", .none),
        "job_active": ("Another job is already running. Wait for it to finish, then try again.", .retry),
        "job_conflict": ("This clashes with a job already running. Wait for it to finish, then try again.", .retry),
        "job_not_complete": ("This job hasn't finished yet. Wait for it to complete.", .none),
        "job_not_found": ("Covalent couldn't find this job. It may have already finished.", .none),
        "invalid_job_id": ("Covalent couldn't read this job's identifier. Start the operation again.", .retry),
        "node_busy": ("Your backup server is busy with something else. Try again in a moment.", .retry),
        "node_state_locked": ("Your backup server is applying another change. Try again in a moment.", .retry),
        "archive_processing_timeout": (
            "Your backup server took too long to work through this backup. "
                + "Try again, or choose a smaller folder.",
            .retry
        ),
        "archive_processing_too_slow": (
            "This transfer was running too slowly to continue safely. "
                + "Move closer to your network, then try again.",
            .retry
        ),
        "confirmation_required": ("This has to be confirmed on the other device before it can finish.", .none),

        // Source folder
        "source_changed": (
            "Files changed while Covalent was copying them. Try again once they stop changing.", .retry
        ),
        "source_unreadable": (
            "Covalent couldn't read part of the folder you chose. Choose the folder again.", .chooseFolderAgain
        ),
        "invalid_authorized_root": (
            "The folder you chose is no longer available to Covalent. Choose it again.", .chooseFolderAgain
        ),

        // Restore
        "unsafe_restore_path": (
            "This backup holds a file that would land outside the folder you chose, "
                + "so Covalent stopped the restore to keep your files safe.",
            .none
        ),
        "restore_conflict": (
            "Some files already exist in the folder you're restoring into. "
                + "Preview the restore again and choose how to handle them.",
            .previewRestoreAgain
        ),
        "restore_plan_mismatch": (
            "This restore changed after you previewed it. Preview it again before restoring.",
            .previewRestoreAgain
        ),
        "restore_plan_not_found": (
            "This restore preview has expired. Preview the restore again.", .previewRestoreAgain
        ),
        "invalid_restore_plan_id": (
            "Covalent couldn't read this restore plan. Preview the restore again.", .previewRestoreAgain
        ),
        "invalid_restore_execute_request": (
            "Covalent couldn't read this restore request. Preview the restore again.", .previewRestoreAgain
        ),
        "invalid_streamed_restore_plan": (
            "Covalent couldn't read the restore plan your server sent. Preview the restore again.",
            .previewRestoreAgain
        ),

        // Restore target inventory
        "invalid_target_inventory": (
            "Covalent couldn't finish checking the folder you're restoring into. Preview the restore again.",
            .previewRestoreAgain
        ),
        "target_inventory_required": (
            "Covalent needs to check the folder you're restoring into first. Preview the restore again.",
            .previewRestoreAgain
        ),
        "target_inventory_not_found": (
            "The check of your restore folder has expired. Preview the restore again.", .previewRestoreAgain
        ),
        "target_inventory_incomplete": (
            "Covalent didn't finish checking the folder you're restoring into. Preview the restore again.",
            .previewRestoreAgain
        ),
        "target_inventory_digest_mismatch": (
            "The folder you're restoring into changed while Covalent was checking it. Preview the restore again.",
            .previewRestoreAgain
        ),
        "target_inventory_job_mismatch": (
            "This restore check belongs to a different job. Preview the restore again.", .previewRestoreAgain
        ),
        "target_inventory_offset_mismatch": (
            "Covalent lost its place while checking your restore folder. Preview the restore again.",
            .previewRestoreAgain
        ),
        "target_inventory_page_mismatch": (
            "Covalent lost its place while checking your restore folder. Preview the restore again.",
            .previewRestoreAgain
        ),

        // Backup contents
        "backup_corrupt": (
            "Some of this backup's encrypted data is damaged. Verify the backup to see what can still be restored.",
            .none
        ),
        "backup_unavailable": ("This backup isn't available on your backup server right now.", .retry),
        "invalid_archive": ("Covalent couldn't verify this backup's contents. Start the backup again.", .retry),
        "invalid_archive_entry": (
            "Covalent couldn't verify one of the files in this backup. Start the backup again.", .retry
        ),
        "invalid_archive_metadata": (
            "Covalent couldn't verify this backup's details. Start the backup again.", .retry
        ),
        "archive_metadata_required": (
            "This backup arrived without its details. Start the backup again.", .retry
        ),
        "archive_upload_headers_required": (
            "This upload arrived incomplete. Start the backup again.", .retry
        ),
        "archive_digest_mismatch": (
            "The backup that arrived didn't match what this device sent. Start the backup again.", .retry
        ),
        "duplicate_archive_entry": (
            "This backup listed the same file twice. Start the backup again.", .retry
        ),
        "invalid_upload_digest": (
            "The upload didn't match what this device sent. Start the backup again.", .retry
        ),
        "invalid_upload_length": ("The upload lost its place. Start the backup again.", .retry),
        "invalid_upload_offset": ("The upload lost its place. Start the backup again.", .retry),

        // Pairing
        "invitation_unavailable": (
            "This pairing invitation has expired or was already used. Start pairing again.", .chooseAnotherDevice
        ),
        "protocol_incompatible": (
            "These two devices run versions of Covalent that can't work together. Update both, then try again.",
            .chooseAnotherDevice
        ),
        "pairing_endpoint_mismatch": (
            "That device answered from a different address than the one you paired with. Pair with it again.",
            .chooseAnotherDevice
        ),
        "provider_binding_mismatch": (
            "That device didn't match the identity it signed when you paired. Pair with it again.",
            .chooseAnotherDevice
        ),
        "invalid_provider_address": (
            "Covalent couldn't reach that device at the address given. Check the address, then try again.",
            .chooseAnotherDevice
        ),

        // Request contract — the app and the server disagree
        "invalid_contract": (
            "This app and your backup server don't agree on how to talk to each other. "
                + "Update both to the same version.",
            .none
        ),
        "invalid_json": (
            "This app and your backup server don't agree on how to talk to each other. "
                + "Update both to the same version.",
            .none
        ),
        "invalid_content_type": (
            "This app and your backup server don't agree on how to talk to each other. "
                + "Update both to the same version.",
            .none
        ),
        "method_not_allowed": (
            "This app asked for something your backup server doesn't offer. Update both to the same version.",
            .none
        ),
        "route_not_found": (
            "This app asked for something your backup server doesn't offer. Update both to the same version.",
            .none
        ),
        "invalid_page_cursor": ("Covalent lost its place while loading this list. Try again.", .retry),
        "invalid_page_limit": ("Covalent lost its place while loading this list. Try again.", .retry),
        "internal_error": (
            "Something went wrong on your backup server. Try again; if it keeps happening, check its logs.",
            .retry
        ),
    ]
}
