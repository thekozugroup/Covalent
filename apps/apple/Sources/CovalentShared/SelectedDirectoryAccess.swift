import Foundation

public struct SelectedDirectoryGrant: Sendable {
    public let bookmarkData: Data

    public init(bookmarkData: Data) {
        self.bookmarkData = bookmarkData
    }

    public static func capture(url: URL) throws -> Self {
        guard url.isFileURL else {
            throw SelectedDirectoryError.notAFileURL
        }
        #if os(macOS)
        let options: URL.BookmarkCreationOptions = [.withSecurityScope]
        #else
        let options: URL.BookmarkCreationOptions = []
        #endif
        let data = try url.bookmarkData(
            options: options,
            includingResourceValuesForKeys: [.isDirectoryKey],
            relativeTo: nil
        )
        return Self(bookmarkData: data)
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
        return ResolvedSelectedDirectory(url: url)
    }
}

public struct ResolvedSelectedDirectory: Sendable {
    public let url: URL

    public func withCoordinatedRead(_ operation: @Sendable (URL) throws -> Void) throws {
        let didStartAccess = url.startAccessingSecurityScopedResource()
        guard didStartAccess else {
            throw SelectedDirectoryError.accessDenied
        }
        defer { url.stopAccessingSecurityScopedResource() }

        var coordinationError: NSError?
        var operationResult: Result<Void, Error>?
        NSFileCoordinator().coordinate(
            readingItemAt: url,
            options: [],
            error: &coordinationError
        ) { coordinatedURL in
            operationResult = Result { try operation(coordinatedURL) }
        }
        if let coordinationError {
            throw coordinationError
        }
        guard let operationResult else {
            throw SelectedDirectoryError.coordinationFailed
        }
        try operationResult.get()
    }
}

public enum SelectedDirectoryError: Error, Equatable {
    case notAFileURL
    case staleBookmark
    case accessDenied
    case coordinationFailed
}
