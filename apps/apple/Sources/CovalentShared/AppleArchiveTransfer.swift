import Foundation
import ZIPFoundation

enum AppleArchiveTransfer {
    static let backupContentType = "application/vnd.covalent.backup+zip"
    static let restoreContentType = "application/vnd.covalent.restore+zip"
    static let metadataHeader = "X-Covalent-Archive-Metadata"
    static let restoreResultHeader = "X-Covalent-Restore-Result"

    private static let bufferSize = 64 * 1_024
    private static let maximumEntries = 1_000_000
    private static let maximumDepth = 128
    private static let maximumPathBytes = 4_096
    private static let maximumComponentBytes = 255
    private static let maximumCompressedBytes: UInt64 = 1 << 40
    private static let maximumUncompressedBytes: UInt64 = 16 << 40

    static func makeBackupArchive(sourceURL: URL) throws -> URL {
        try Task.checkCancellation()
        try requireDirectory(sourceURL)
        let archiveURL = temporaryArchiveURL(prefix: "covalent-backup")
        let fileManager = FileManager.default
        do {
            let archive = try Archive(url: archiveURL, accessMode: .create)
            var entryCount = 0
            var totalBytes: UInt64 = 0

            func addDirectory(_ directory: URL, components: [String]) throws {
                try Task.checkCancellation()
                guard components.count <= maximumDepth else {
                    throw AppleArchiveTransferError.pathTooDeep
                }
                let children = try fileManager.contentsOfDirectory(
                    at: directory,
                    includingPropertiesForKeys: [
                        .isDirectoryKey,
                        .isRegularFileKey,
                        .isSymbolicLinkKey,
                        .fileSizeKey,
                    ],
                    options: []
                ).sorted {
                    ($0.lastPathComponent, $0.absoluteString) < ($1.lastPathComponent, $1.absoluteString)
                }
                for child in children {
                    try Task.checkCancellation()
                    let component = try validatedComponent(child.lastPathComponent)
                    let childComponents = components + [component]
                    let path = try validatedPath(components: childComponents)
                    entryCount += 1
                    guard entryCount <= maximumEntries else {
                        throw AppleArchiveTransferError.tooManyEntries
                    }
                    let values = try child.resourceValues(forKeys: [
                        .isDirectoryKey,
                        .isRegularFileKey,
                        .isSymbolicLinkKey,
                        .fileSizeKey,
                    ])
                    guard values.isSymbolicLink != true else {
                        throw AppleArchiveTransferError.unsupportedEntry(path)
                    }
                    if values.isDirectory == true {
                        try archive.addEntry(
                            with: path,
                            fileURL: child,
                            compressionMethod: .none,
                            bufferSize: bufferSize
                        )
                        try addDirectory(child, components: childComponents)
                    } else if values.isRegularFile == true {
                        let size = UInt64(values.fileSize ?? 0)
                        let (nextTotal, overflow) = totalBytes.addingReportingOverflow(size)
                        guard !overflow, nextTotal <= maximumUncompressedBytes else {
                            throw AppleArchiveTransferError.uncompressedSizeExceeded
                        }
                        totalBytes = nextTotal
                        try archive.addEntry(
                            with: path,
                            fileURL: child,
                            compressionMethod: .deflate,
                            bufferSize: bufferSize
                        )
                    } else {
                        throw AppleArchiveTransferError.unsupportedEntry(path)
                    }
                }
            }

            try addDirectory(sourceURL, components: [])
            try secureTemporaryFile(archiveURL)
            let compressedSize = try fileSize(archiveURL)
            guard compressedSize <= maximumCompressedBytes else {
                throw AppleArchiveTransferError.compressedSizeExceeded
            }
            return archiveURL
        } catch {
            try? fileManager.removeItem(at: archiveURL)
            throw error
        }
    }

    static func copyDownloadedArchive(_ sourceURL: URL) throws -> URL {
        try Task.checkCancellation()
        let size = try fileSize(sourceURL)
        guard size <= maximumCompressedBytes else {
            throw AppleArchiveTransferError.compressedSizeExceeded
        }
        let destination = temporaryArchiveURL(prefix: "covalent-restore")
        do {
            try FileManager.default.copyItem(at: sourceURL, to: destination)
            try secureTemporaryFile(destination)
            return destination
        } catch {
            try? FileManager.default.removeItem(at: destination)
            throw error
        }
    }

    static func requireEmptyDirectory(_ targetURL: URL) throws {
        try requireDirectory(targetURL)
        guard try FileManager.default.contentsOfDirectory(atPath: targetURL.path).isEmpty else {
            throw AppleArchiveTransferError.restoreDestinationMustBeEmpty
        }
    }

    static func extractRestoreArchive(_ archiveURL: URL, to targetURL: URL, plan: RestorePlan) throws {
        try Task.checkCancellation()
        try requireEmptyDirectory(targetURL)
        let archive = try Archive(url: archiveURL, accessMode: .read)
        let entries = Array(archive)
        guard entries.count <= maximumEntries else {
            throw AppleArchiveTransferError.tooManyEntries
        }

        var expected: [String: RestoreEntryKind] = [:]
        for planned in plan.entries {
            let planPath = planned.kind == .directory ? planned.destinationPath + "/" : planned.destinationPath
            let components = try validatedArchivePath(planPath, isDirectory: planned.kind == .directory)
            let canonical = components.joined(separator: "/")
            guard expected.updateValue(planned.kind, forKey: canonical) == nil else {
                throw AppleArchiveTransferError.duplicateEntry(canonical)
            }
            switch (planned.kind, planned.action) {
            case (.directory, .createDirectory), (.file, .createFile):
                break
            default:
                throw AppleArchiveTransferError.nonEmptyRestorePlan
            }
        }

        var seen = Set<String>()
        var validatedEntries: [(entry: Entry, components: [String])] = []
        var totalBytes: UInt64 = 0
        for entry in entries {
            try Task.checkCancellation()
            let isDirectory = entry.type == .directory
            guard entry.type == .file || isDirectory else {
                throw AppleArchiveTransferError.unsupportedEntry(entry.path)
            }
            let components = try validatedArchivePath(entry.path, isDirectory: isDirectory)
            let canonical = components.joined(separator: "/")
            guard seen.insert(canonical).inserted else {
                throw AppleArchiveTransferError.duplicateEntry(canonical)
            }
            let expectedKind: RestoreEntryKind = isDirectory ? .directory : .file
            guard expected[canonical] == expectedKind else {
                throw AppleArchiveTransferError.restorePlanMismatch(canonical)
            }
            let (nextTotal, overflow) = totalBytes.addingReportingOverflow(entry.uncompressedSize)
            guard !overflow, nextTotal <= maximumUncompressedBytes else {
                throw AppleArchiveTransferError.uncompressedSizeExceeded
            }
            totalBytes = nextTotal
            validatedEntries.append((entry, components))
        }
        guard seen == Set(expected.keys) else {
            throw AppleArchiveTransferError.restorePlanMismatch("missing signed entry")
        }

        let fileManager = FileManager.default
        var createdURLs: [URL] = []
        var createdDirectoryPaths = Set<String>()
        do {
            for item in validatedEntries where item.entry.type == .file {
                try Task.checkCancellation()
                let parent = try ensureDirectoryPath(
                    root: targetURL,
                    components: Array(item.components.dropLast()),
                    fileManager: fileManager,
                    createdURLs: &createdURLs,
                    createdDirectoryPaths: &createdDirectoryPaths
                )
                let destination = parent.appending(path: item.components.last!, directoryHint: .notDirectory)
                guard !fileManager.fileExists(atPath: destination.path) else {
                    throw AppleArchiveTransferError.destinationChanged
                }
                let temporary = parent.appending(
                    path: ".covalent-restore-\(UUID().uuidString.lowercased())",
                    directoryHint: .notDirectory
                )
                do {
                    _ = try archive.extract(item.entry, to: temporary, bufferSize: bufferSize)
                    guard !fileManager.fileExists(atPath: destination.path) else {
                        throw AppleArchiveTransferError.destinationChanged
                    }
                    try fileManager.moveItem(at: temporary, to: destination)
                    createdURLs.append(destination)
                } catch {
                    try? fileManager.removeItem(at: temporary)
                    throw error
                }
            }

            let directories = validatedEntries
                .filter { $0.entry.type == .directory }
                .sorted { $0.components.count > $1.components.count }
            for item in directories {
                try Task.checkCancellation()
                let destination = try ensureDirectoryPath(
                    root: targetURL,
                    components: item.components,
                    fileManager: fileManager,
                    createdURLs: &createdURLs,
                    createdDirectoryPaths: &createdDirectoryPaths
                )
                _ = try archive.extract(item.entry, to: destination, bufferSize: bufferSize)
            }
        } catch {
            for createdURL in createdURLs.reversed() {
                if (try? createdURL.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) == true {
                    if (try? fileManager.contentsOfDirectory(atPath: createdURL.path).isEmpty) == true {
                        try? fileManager.removeItem(at: createdURL)
                    }
                } else {
                    try? fileManager.removeItem(at: createdURL)
                }
            }
            throw error
        }
    }

    private static func requireDirectory(_ url: URL) throws {
        guard url.isFileURL else { throw AppleArchiveTransferError.notFileURL }
        let values = try url.resourceValues(forKeys: [.isDirectoryKey, .isSymbolicLinkKey])
        guard values.isDirectory == true, values.isSymbolicLink != true else {
            throw AppleArchiveTransferError.notDirectory
        }
    }

    private static func ensureDirectoryPath(
        root: URL,
        components: [String],
        fileManager: FileManager,
        createdURLs: inout [URL],
        createdDirectoryPaths: inout Set<String>
    ) throws -> URL {
        var current = root
        for component in components {
            current.append(path: component, directoryHint: .isDirectory)
            let standardizedPath = current.standardizedFileURL.path
            if fileManager.fileExists(atPath: current.path) {
                guard createdDirectoryPaths.contains(standardizedPath) else {
                    throw AppleArchiveTransferError.destinationChanged
                }
                try assertSafeDirectory(current)
            } else {
                try fileManager.createDirectory(at: current, withIntermediateDirectories: false)
                try assertSafeDirectory(current)
                createdURLs.append(current)
                createdDirectoryPaths.insert(standardizedPath)
            }
        }
        return current
    }

    private static func assertSafeDirectory(_ url: URL) throws {
        let values = try url.resourceValues(forKeys: [.isDirectoryKey, .isSymbolicLinkKey])
        guard values.isDirectory == true, values.isSymbolicLink != true else {
            throw AppleArchiveTransferError.destinationChanged
        }
    }

    private static func validatedArchivePath(_ rawPath: String, isDirectory: Bool) throws -> [String] {
        var path = rawPath
        if isDirectory {
            guard path.hasSuffix("/") else { throw AppleArchiveTransferError.unsafePath(rawPath) }
            path.removeLast()
        } else if path.hasSuffix("/") {
            throw AppleArchiveTransferError.unsafePath(rawPath)
        }
        guard !path.isEmpty,
              !path.hasPrefix("/"),
              !path.contains("\\"),
              !path.contains("\0"),
              path.utf8.count <= maximumPathBytes
        else {
            throw AppleArchiveTransferError.unsafePath(rawPath)
        }
        let components = path.split(separator: "/", omittingEmptySubsequences: false).map(String.init)
        guard components.count <= maximumDepth else { throw AppleArchiveTransferError.pathTooDeep }
        for component in components {
            _ = try validatedComponent(component)
        }
        return components
    }

    private static func validatedPath(components: [String]) throws -> String {
        guard !components.isEmpty, components.count <= maximumDepth else {
            throw AppleArchiveTransferError.pathTooDeep
        }
        let path = components.joined(separator: "/")
        guard path.utf8.count <= maximumPathBytes else {
            throw AppleArchiveTransferError.unsafePath(path)
        }
        return path
    }

    private static func validatedComponent(_ component: String) throws -> String {
        guard !component.isEmpty,
              component != ".",
              component != "..",
              !component.contains("/"),
              !component.contains("\\"),
              !component.contains("\0"),
              component.utf8.count <= maximumComponentBytes
        else {
            throw AppleArchiveTransferError.unsafePath(component)
        }
        return component
    }

    private static func temporaryArchiveURL(prefix: String) -> URL {
        FileManager.default.temporaryDirectory.appending(
            path: "\(prefix)-\(UUID().uuidString.lowercased()).zip",
            directoryHint: .notDirectory
        )
    }

    private static func secureTemporaryFile(_ url: URL) throws {
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
        var mutableURL = url
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? mutableURL.setResourceValues(values)
    }

    private static func fileSize(_ url: URL) throws -> UInt64 {
        let values = try url.resourceValues(forKeys: [.fileSizeKey])
        return UInt64(values.fileSize ?? 0)
    }
}

enum AppleArchiveTransferError: Error, Equatable, Sendable {
    case notFileURL
    case notDirectory
    case pathTooDeep
    case unsafePath(String)
    case unsupportedEntry(String)
    case duplicateEntry(String)
    case tooManyEntries
    case compressedSizeExceeded
    case uncompressedSizeExceeded
    case restoreDestinationMustBeEmpty
    case nonEmptyRestorePlan
    case restorePlanMismatch(String)
    case destinationChanged
}

extension AppleArchiveTransferError: LocalizedError {
    var errorDescription: String? {
        switch self {
        case .notFileURL: "Choose a folder on this device or a connected file provider."
        case .notDirectory: "The selected folder is unavailable or is a symbolic link."
        case .pathTooDeep: "The selected folder is nested beyond the supported transfer depth."
        case let .unsafePath(path): "The transfer contains an unsafe path: \(path)"
        case let .unsupportedEntry(path): "Only regular files and folders can be transferred: \(path)"
        case let .duplicateEntry(path): "The transfer contains a duplicate path: \(path)"
        case .tooManyEntries: "The selected folder contains too many entries."
        case .compressedSizeExceeded: "The transfer archive exceeds the 1 TiB compressed limit."
        case .uncompressedSizeExceeded: "The transfer archive exceeds the 16 TiB expanded limit."
        case .restoreDestinationMustBeEmpty:
            "Choose an empty restore folder so the signed no-write preview remains exact."
        case .nonEmptyRestorePlan:
            "The node returned a streamed restore plan that could modify existing files."
        case let .restorePlanMismatch(path): "The restore archive did not match its signed plan at \(path)."
        case .destinationChanged: "The restore folder changed after preview. Choose an empty folder and preview again."
        }
    }
}
