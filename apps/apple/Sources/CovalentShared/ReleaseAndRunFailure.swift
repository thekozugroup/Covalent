import Foundation

// MARK: - Release check, from `update` in GET /api/v1/status

/// What the service's once-a-day release check last found.
///
/// The service only reports; it never downloads or installs anything, and
/// neither does this app. Every field is optional on the wire in practice: a
/// service at 0.2.12 or earlier sends no `update` at all.
public struct NodeUpdateStatus: Codable, Equatable, Sendable {
    public enum State: String, Sendable {
        case disabled
        case unsupported
        case pending
        case upToDate
        case updateAvailable
        case checkFailed
    }

    public struct Failure: Codable, Equatable, Sendable {
        public let code: String
        public let message: String

        public init(code: String, message: String) {
            self.code = code
            self.message = message
        }
    }

    /// Kept raw so a state added by a newer service still decodes;
    /// ``knownState`` is `nil` for it and the app shows nothing new.
    public let state: String
    public let latestVersion: String?
    public let releaseUrl: String?
    public let checkedAtUnixMs: UInt64?
    public let failure: Failure?

    public init(
        state: String,
        latestVersion: String? = nil,
        releaseUrl: String? = nil,
        checkedAtUnixMs: UInt64? = nil,
        failure: Failure? = nil
    ) {
        self.state = state
        self.latestVersion = latestVersion
        self.releaseUrl = releaseUrl
        self.checkedAtUnixMs = checkedAtUnixMs
        self.failure = failure
    }

    public var knownState: State? { State(rawValue: state) }
}

/// A newer release, ready to offer as a link to its release page.
public struct AvailableUpdate: Equatable, Sendable {
    public let version: String
    public let releaseURL: URL
}

extension NodeStatus {
    /// The only release page the app will open is this prefix followed by
    /// exactly the reported version, the same rule as the web console. The
    /// service builds the URL that way itself; checking it again here means a
    /// surprising value (an extra path, a query, another version) is simply
    /// not offered.
    static let releasePagePrefix = "https://github.com/thekozugroup/Covalent/releases/tag/v"

    /// The service's version, when it is a plain short version string.
    public var displayVersion: String? {
        Self.plainVersion(version)
    }

    /// A newer release to offer, or `nil` for every other state, for an older
    /// service, and for anything that does not look exactly right.
    public var availableUpdate: AvailableUpdate? {
        guard let update, update.knownState == .updateAvailable,
              let version = Self.plainVersion(update.latestVersion),
              let link = update.releaseUrl,
              link == Self.releasePagePrefix + version,
              let url = URL(string: link),
              url.scheme == "https",
              url.host == "github.com"
        else { return nil }
        return AvailableUpdate(version: version, releaseURL: url)
    }

    /// One quiet line about the release check, or `nil` when there is nothing
    /// worth saying. An available update is shown as its own notice instead.
    /// None of these is an error: transfers do not depend on the check.
    public var updateCheckSummary: String? {
        guard let update else { return nil }
        switch update.knownState {
        case .upToDate:
            return "Covalent is up to date."
        case .disabled:
            return "Update checks are off."
        case .checkFailed:
            return "Couldn't check for updates. " + Self.updateCheckReason(code: update.failure?.code)
        case .pending, .unsupported, .updateAvailable, nil:
            return nil
        }
    }

    /// Plain words for why the release check had no answer, keyed on the
    /// stable code. An unknown code reads like `unexpectedResponse`.
    static func updateCheckReason(code: String?) -> String {
        switch code {
        case "network":
            "Covalent couldn't reach GitHub. Check this device's internet connection."
        case "timeout":
            "GitHub took too long to answer. Covalent tries again tomorrow."
        case "secureConnection":
            "Covalent couldn't make a secure connection to GitHub. On a filtered network, this check may not work."
        case "rateLimited":
            "GitHub is limiting requests from this network. Covalent tries again tomorrow."
        default:
            "GitHub sent an answer Covalent didn't recognize. Covalent tries again tomorrow."
        }
    }

    private static func plainVersion(_ value: String?) -> String? {
        guard let value, !value.isEmpty, value.count <= 32,
              value.range(of: #"^[0-9A-Za-z.+-]+$"#, options: .regularExpression) != nil
        else { return nil }
        return value
    }
}

// MARK: - Failure causes, from GET /api/v1/sync/run-failures

/// Why this device's own most recent attempt at each link failed.
///
/// Only this device's attempts are listed: a cause is never sent between
/// devices. So on a source, a recipient that failed has no entry here.
public struct FolderRunFailures: Codable, Equatable, Sendable {
    public let schemaVersion: Int
    public let failures: [FolderRunFailure]

    public init(schemaVersion: Int = 1, failures: [FolderRunFailure]) {
        self.schemaVersion = schemaVersion
        self.failures = failures
    }

    /// The cause recorded for the run `share` is showing now, in the given
    /// role. An entry for an earlier run is ignored: a newer run has started.
    public func failure(for share: FolderShare, role: FolderRunFailure.Role) -> FolderRunFailure? {
        guard let generation = share.linkRun?.generation else { return nil }
        return failures.last {
            $0.folderId == share.folderId && $0.generation == generation && $0.role == role.rawValue
        }
    }
}

public struct FolderRunFailure: Codable, Equatable, Sendable {
    public enum Role: String, Sendable {
        /// This device was receiving, and its copy failed.
        case destination
        /// This device holds the source folder and could not prepare the run.
        case source
    }

    public let folderId: UUID
    public let generation: UInt64
    /// Raw, so a role added later decodes and is simply never matched.
    public let role: String
    public let code: String
    public let message: String
    public let endedAtUnixMs: UInt64

    public init(
        folderId: UUID,
        generation: UInt64,
        role: String,
        code: String,
        message: String,
        endedAtUnixMs: UInt64
    ) {
        self.folderId = folderId
        self.generation = generation
        self.role = role
        self.code = code
        self.message = message
        self.endedAtUnixMs = endedAtUnixMs
    }

    /// A cause that needs no action: the next run picks up the new files.
    public var isInformational: Bool { code == "sourceChanged" }

    /// What happened and what to do, in the app's own words for every known
    /// code. An unrecognized code falls back to the service's sentence, which
    /// it guarantees is safe to show, and then to a generic line.
    public var plainCause: String {
        switch code {
        case "noSpace":
            "This device ran out of storage space. Free up space, then try again."
        case "permissionDenied":
            "Some files or folders couldn't be read or written. Check the folder permissions on both devices, then try again."
        case "readOnly":
            "The destination folder is read-only. Make it writable, or choose another folder."
        case "integrityMismatch":
            "A copied file didn't match the source. Try again. If this keeps happening, check the disk."
        case "notFound":
            "A file or folder went missing during the copy. Check that both folders still exist, then try again."
        case "network":
            "The connection to the source dropped. Check that both devices are online and can reach each other, then try again."
        case "timeout":
            "The source took too long to respond. Check the connection, then try again."
        case "sourceChanged":
            "Files at the source changed during the copy. The next run copies the new versions."
        case "folderUnavailable":
            "This device couldn't open the link's folder. Reconnect the drive or choose the folder again."
        case "recoveryRequired":
            "An earlier copy on this device was interrupted and needs review before the next run."
        case "workerUnavailable":
            "Covalent's transfer engine couldn't start. Quit and reopen Covalent, then try again."
        case "unknown":
            Self.genericCause
        default:
            Self.serviceSentence(message) ?? Self.genericCause
        }
    }

    static let genericCause = "The copy didn't finish, and Covalent couldn't tell why. Try again."

    private static func serviceSentence(_ message: String) -> String? {
        let trimmed = message.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, trimmed.count <= 400, !trimmed.contains(where: \.isNewline) else { return nil }
        return trimmed
    }
}
