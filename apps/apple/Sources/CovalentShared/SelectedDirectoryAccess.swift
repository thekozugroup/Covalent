import Foundation

public enum DirectoryAccessPurpose: String, Codable, Sendable {
    case backupSource
    case restoreDestination
    case folderSync

    public var displayName: String {
        switch self {
        case .backupSource: "Backup source"
        case .restoreDestination: "Restore destination"
        case .folderSync: "Folder sync"
        }
    }
}

public struct SelectedDirectoryGrant: Codable, Equatable, Identifiable, Sendable {
    public let id: UUID
    public let displayName: String
    public let purpose: DirectoryAccessPurpose
    public let bookmarkData: Data
    public let capturedAt: Date
    /// The exact durable folder share that owns this capability. Older saved
    /// grants decode as unbound and are migrated only when the choice is
    /// unambiguous; a label or path is never used to guess a share identity.
    public let folderOfferId: UUID?

    public init(
        id: UUID = UUID(),
        displayName: String,
        purpose: DirectoryAccessPurpose,
        bookmarkData: Data,
        capturedAt: Date = Date(),
        folderOfferId: UUID? = nil
    ) {
        self.id = id
        self.displayName = displayName
        self.purpose = purpose
        self.bookmarkData = bookmarkData
        self.capturedAt = capturedAt
        self.folderOfferId = folderOfferId
    }

    public func bound(toFolderOfferId offerId: UUID) -> Self {
        Self(
            id: id,
            displayName: displayName,
            purpose: purpose,
            bookmarkData: bookmarkData,
            capturedAt: capturedAt,
            folderOfferId: offerId
        )
    }

    public static func capture(url: URL, purpose: DirectoryAccessPurpose) throws -> Self {
        guard url.isFileURL else {
            throw SelectedDirectoryError.notAFileURL
        }
        let didStartAccess = url.startAccessingSecurityScopedResource()
        defer {
            if didStartAccess {
                url.stopAccessingSecurityScopedResource()
            }
        }
        let values = try url.resourceValues(forKeys: [.isDirectoryKey, .nameKey])
        guard values.isDirectory == true else {
            throw SelectedDirectoryError.notADirectory
        }
        #if os(macOS)
        let options: URL.BookmarkCreationOptions = [.withSecurityScope]
        #else
        let options: URL.BookmarkCreationOptions = []
        #endif
        let data = try url.bookmarkData(
            options: options,
            includingResourceValuesForKeys: [.isDirectoryKey, .nameKey],
            relativeTo: nil
        )
        return Self(
            displayName: values.name ?? url.lastPathComponent,
            purpose: purpose,
            bookmarkData: data
        )
    }

    public func resolve() throws -> ResolvedSelectedDirectory {
        var isStale = false
        #if os(macOS)
        let options: URL.BookmarkResolutionOptions = [.withSecurityScope, .withoutUI]
        #else
        let options: URL.BookmarkResolutionOptions = [.withoutUI]
        #endif
        let url = try URL(
            resolvingBookmarkData: bookmarkData,
            options: options,
            relativeTo: nil,
            bookmarkDataIsStale: &isStale
        )
        guard !isStale else {
            throw SelectedDirectoryError.staleBookmark
        }
        // Resolving a security-scoped bookmark produces the URL but does not
        // grant access to it. Validate only while a temporary scope is active;
        // callers start their own operation or helper-lifetime scope afterward.
        let didStartAccess = url.startAccessingSecurityScopedResource()
        guard didStartAccess else {
            throw SelectedDirectoryError.accessDenied
        }
        defer { url.stopAccessingSecurityScopedResource() }
        let values = try url.resourceValues(forKeys: [.isDirectoryKey])
        guard values.isDirectory == true else {
            throw SelectedDirectoryError.permissionRevoked
        }
        return ResolvedSelectedDirectory(url: url)
    }
}

/// A replacement bookmark saved before the local node is asked to mutate its
/// folder journal. It remains until the exact repair is acknowledged and the
/// normal helper has restarted with the promoted grant.
public struct PendingFolderAccessRepair: Codable, Equatable, Identifiable, Sendable {
    public let offerId: UUID
    public let replacementGrant: SelectedDirectoryGrant
    /// Exact request value retained across response loss and app relaunch.
    /// The path is local-only and is never rendered or sent to a peer.
    public let selectedRoot: String
    public let preparedAt: Date

    public var id: UUID { offerId }

    public init(
        offerId: UUID,
        replacementGrant: SelectedDirectoryGrant,
        selectedRoot: String,
        preparedAt: Date = Date()
    ) {
        self.offerId = offerId
        self.replacementGrant = replacementGrant.bound(toFolderOfferId: offerId)
        self.selectedRoot = selectedRoot
        self.preparedAt = preparedAt
    }
}

public struct ResolvedSelectedDirectory: Sendable {
    public let url: URL

    public func withCoordinatedRead<Value: Sendable>(
        _ operation: @escaping @Sendable (URL) async throws -> Value
    ) async throws -> Value {
        try await withCoordinatedAccess(writing: false, operation)
    }

    public func withCoordinatedWrite<Value: Sendable>(
        _ operation: @escaping @Sendable (URL) async throws -> Value
    ) async throws -> Value {
        try await withCoordinatedAccess(writing: true, operation)
    }

    private func withCoordinatedAccess<Value: Sendable>(
        writing: Bool,
        _ operation: @escaping @Sendable (URL) async throws -> Value
    ) async throws -> Value {
        let didStartAccess = url.startAccessingSecurityScopedResource()
        guard didStartAccess else {
            throw SelectedDirectoryError.accessDenied
        }
        defer { url.stopAccessingSecurityScopedResource() }
        let values = try url.resourceValues(forKeys: [.isDirectoryKey])
        guard values.isDirectory == true else {
            throw SelectedDirectoryError.permissionRevoked
        }
        try Task.checkCancellation()

        let intent = writing
            ? NSFileAccessIntent.writingIntent(with: url, options: .forMerging)
            : NSFileAccessIntent.readingIntent(with: url, options: .withoutChanges)
        let coordinator = NSFileCoordinator()
        let queue = OperationQueue()
        queue.name = "life.michaelwong.covalent.file-coordination"
        queue.maxConcurrentOperationCount = 1
        queue.qualityOfService = .userInitiated

        return try await withCheckedThrowingContinuation { continuation in
            coordinator.coordinate(with: [intent], queue: queue) { coordinationError in
                if let coordinationError {
                    continuation.resume(throwing: coordinationError)
                    return
                }
                let resultBox = CoordinatedResultBox<Value>()
                let semaphore = DispatchSemaphore(value: 0)
                let coordinatedURL = intent.url
                Task.detached(priority: .userInitiated) {
                    do {
                        try Task.checkCancellation()
                        resultBox.store(.success(try await operation(coordinatedURL)))
                    } catch {
                        resultBox.store(.failure(error))
                    }
                    semaphore.signal()
                }
                semaphore.wait()
                guard let result = resultBox.take() else {
                    continuation.resume(throwing: SelectedDirectoryError.coordinationFailed)
                    return
                }
                continuation.resume(with: result)
            }
        }
    }
}

private final class CoordinatedResultBox<Value: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private var result: Result<Value, Error>?

    func store(_ result: Result<Value, Error>) {
        lock.lock()
        self.result = result
        lock.unlock()
    }

    func take() -> Result<Value, Error>? {
        lock.lock()
        defer { lock.unlock() }
        return result
    }
}

public enum SelectedDirectoryError: Error, Equatable, Sendable {
    case notAFileURL
    case notADirectory
    case staleBookmark
    case permissionRevoked
    case accessDenied
    case coordinationFailed
    case tooManyFolderSyncGrants
}

extension SelectedDirectoryError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .notAFileURL: "Choose a folder on this device, or one from a connected cloud drive."
        case .notADirectory: "Choose a folder, not an individual file."
        case .staleBookmark: "Folder access changed. Choose the folder again to continue."
        case .permissionRevoked: "This folder is no longer available. Choose it again to restore access."
        case .accessDenied: "Covalent could not open this folder. Check its permission and try again."
        case .coordinationFailed: "The system could not coordinate safe access to this folder."
        case .tooManyFolderSyncGrants: "Remove a shared folder before adding another one."
        }
    }
}

public enum FolderAccessRepairError: Error, Equatable, Sendable {
    case repairNotRequired
    case shareMissing
    case pendingRepairMissing
    case differentRepairAlreadyPending
    case ambiguousSavedAccess
    case folderAlreadyUsed
    case savedGrantMissing
    case invalidSavedRepair
}

extension FolderAccessRepairError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .repairNotRequired:
            "Folder access is already available. Refresh Folders before making another change."
        case .shareMissing:
            "This shared folder is no longer available. Refresh Folders and choose the current share."
        case .pendingRepairMissing:
            "Choose this folder again before retrying access repair."
        case .differentRepairAlreadyPending:
            "Finish the saved folder repair before choosing a different folder."
        case .ambiguousSavedAccess:
            "Covalent can't safely match the saved access to this share. Repair or remove one shared folder at a time."
        case .folderAlreadyUsed:
            "That folder is already used by another shared folder. Choose a different folder."
        case .savedGrantMissing:
            "The saved folder access is missing. Choose the folder again."
        case .invalidSavedRepair:
            "The saved folder repair could not be verified. Choose the affected folder again."
        }
    }
}
