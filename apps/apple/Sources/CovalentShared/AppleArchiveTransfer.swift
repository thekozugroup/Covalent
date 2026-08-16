import Darwin
import Foundation
import ZIPFoundation

enum AppleArchiveTransfer {
    static let backupContentType = "application/vnd.covalent.backup+zip"
    static let restoreContentType = "application/vnd.covalent.restore+zip"
    static let metadataHeader = "X-Covalent-Archive-Metadata"
    static let restoreResultHeader = "X-Covalent-Restore-Result"
    static let restorePlanIdHeader = "X-Covalent-Restore-Plan-Id"
    static let restorePlanDigestHeader = "X-Covalent-Restore-Plan-Digest"
    static let jobAcknowledgementRequiredHeader = "X-Covalent-Job-Ack-Required"

    private static let bufferSize = 64 * 1_024
    static let maximumEntries = 200_000
    private static let maximumDepth = 128
    private static let maximumPathBytes = 4_096
    private static let maximumComponentBytes = 255
    private static let maximumCompressedBytes: UInt64 = 8 << 30
    private static let maximumUncompressedBytes: UInt64 = 64 << 30
    private static let minimumFreeSpaceReserve: UInt64 = 256 << 20

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
                        try ensureAvailableCapacity(at: archiveURL, requiredBytes: nextTotal)
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
        try ensureAvailableCapacity(at: destination, requiredBytes: size)
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
        let root = try RestoreRoot(url: targetURL)
        try root.requireEmpty()
    }

    static func extractRestoreArchive(
        _ archiveURL: URL,
        to targetURL: URL,
        plan: RestorePlan,
        beforeWriting: (() throws -> Void)? = nil,
        beforeEntry: (([String]) throws -> Void)? = nil
    ) throws {
        try Task.checkCancellation()
        let root = try RestoreRoot(url: targetURL)
        try root.requireEmpty()
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
        try ensureAvailableCapacity(at: targetURL, requiredBytes: totalBytes)
        try beforeWriting?()
        try root.revalidate()

        var knownDirectories = ["": root.identity]
        var createdDirectories: [[String]] = []
        var createdFiles: [[String]] = []
        do {
            let directories = validatedEntries
                .filter { $0.entry.type == .directory }
                .sorted { $0.components.count < $1.components.count }
            for item in directories {
                try Task.checkCancellation()
                try root.revalidate()
                let descriptor = try openDirectory(
                    root: root,
                    components: item.components,
                    create: true,
                    knownDirectories: &knownDirectories,
                    createdDirectories: &createdDirectories
                )
                close(descriptor)
                _ = try archive.extract(item.entry, bufferSize: bufferSize) { _ in }
            }

            for item in validatedEntries where item.entry.type == .file {
                try Task.checkCancellation()
                try beforeEntry?(item.components)
                try root.revalidate()
                let parentDescriptor = try openDirectory(
                    root: root,
                    components: Array(item.components.dropLast()),
                    create: false,
                    knownDirectories: &knownDirectories,
                    createdDirectories: &createdDirectories
                )
                defer { close(parentDescriptor) }
                guard let name = item.components.last else {
                    throw AppleArchiveTransferError.destinationChanged
                }
                let descriptor = openat(
                    parentDescriptor,
                    name,
                    O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC,
                    mode_t(0o600)
                )
                guard descriptor >= 0 else {
                    throw posixDestinationError()
                }
                createdFiles.append(item.components)
                defer { close(descriptor) }
                _ = try archive.extract(item.entry, bufferSize: bufferSize) { data in
                    try Task.checkCancellation()
                    try writeAll(data, to: descriptor)
                }
                guard fsync(descriptor) == 0 else { throw posixDestinationError() }
            }
            try root.revalidate()
            guard fsync(root.descriptor) == 0 else { throw posixDestinationError() }
        } catch {
            rollback(
                root: root,
                files: createdFiles,
                directories: createdDirectories,
                knownDirectories: &knownDirectories
            )
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

    private static func openDirectory(
        root: RestoreRoot,
        components: [String],
        create: Bool,
        knownDirectories: inout [String: DirectoryIdentity],
        createdDirectories: inout [[String]]
    ) throws -> Int32 {
        var currentDescriptor = dup(root.descriptor)
        guard currentDescriptor >= 0 else { throw posixDestinationError() }
        var traversed: [String] = []
        do {
            for component in components {
                traversed.append(component)
                let canonical = traversed.joined(separator: "/")
                var nextDescriptor = openat(
                    currentDescriptor,
                    component,
                    O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC
                )
                if nextDescriptor < 0, errno == ENOENT, create {
                    guard mkdirat(currentDescriptor, component, mode_t(0o700)) == 0 else {
                        throw posixDestinationError()
                    }
                    nextDescriptor = openat(
                        currentDescriptor,
                        component,
                        O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC
                    )
                    guard nextDescriptor >= 0 else { throw posixDestinationError() }
                    let identity = try directoryIdentity(descriptor: nextDescriptor)
                    guard knownDirectories[canonical] == nil else {
                        close(nextDescriptor)
                        throw AppleArchiveTransferError.destinationChanged
                    }
                    knownDirectories[canonical] = identity
                    createdDirectories.append(traversed)
                } else {
                    guard nextDescriptor >= 0,
                          let expectedIdentity = knownDirectories[canonical],
                          try directoryIdentity(descriptor: nextDescriptor) == expectedIdentity
                    else {
                        if nextDescriptor >= 0 { close(nextDescriptor) }
                        throw AppleArchiveTransferError.destinationChanged
                    }
                }
                close(currentDescriptor)
                currentDescriptor = nextDescriptor
            }
            return currentDescriptor
        } catch {
            close(currentDescriptor)
            throw error
        }
    }

    private static func rollback(
        root: RestoreRoot,
        files: [[String]],
        directories: [[String]],
        knownDirectories: inout [String: DirectoryIdentity]
    ) {
        var ignoredDirectories: [[String]] = []
        for components in files.reversed() {
            guard let name = components.last,
                  let parent = try? openDirectory(
                      root: root,
                      components: Array(components.dropLast()),
                      create: false,
                      knownDirectories: &knownDirectories,
                      createdDirectories: &ignoredDirectories
                  )
            else { continue }
            _ = unlinkat(parent, name, 0)
            close(parent)
        }
        for components in directories.sorted(by: { $0.count > $1.count }) {
            guard let name = components.last,
                  let parent = try? openDirectory(
                      root: root,
                      components: Array(components.dropLast()),
                      create: false,
                      knownDirectories: &knownDirectories,
                      createdDirectories: &ignoredDirectories
                  )
            else { continue }
            _ = unlinkat(parent, name, AT_REMOVEDIR)
            close(parent)
        }
    }

    private static func writeAll(_ data: Data, to descriptor: Int32) throws {
        try data.withUnsafeBytes { rawBuffer in
            guard var address = rawBuffer.baseAddress else { return }
            var remaining = rawBuffer.count
            while remaining > 0 {
                let written = Darwin.write(descriptor, address, remaining)
                if written < 0, errno == EINTR { continue }
                guard written > 0 else { throw posixDestinationError() }
                address = address.advanced(by: written)
                remaining -= written
            }
        }
    }

    private static func directoryIdentity(descriptor: Int32) throws -> DirectoryIdentity {
        var metadata = stat()
        guard fstat(descriptor, &metadata) == 0,
              (metadata.st_mode & S_IFMT) == S_IFDIR
        else {
            throw posixDestinationError()
        }
        return DirectoryIdentity(device: UInt64(metadata.st_dev), inode: UInt64(metadata.st_ino))
    }

    private static func posixDestinationError() -> Error {
        if errno == EEXIST || errno == ELOOP || errno == ENOTDIR || errno == ENOENT {
            return AppleArchiveTransferError.destinationChanged
        }
        return NSError(domain: NSPOSIXErrorDomain, code: Int(errno))
    }

    private static func ensureAvailableCapacity(at url: URL, requiredBytes: UInt64) throws {
        let location = url.hasDirectoryPath ? url : url.deletingLastPathComponent()
        let values = try location.resourceValues(forKeys: [
            .volumeAvailableCapacityForImportantUsageKey,
            .volumeAvailableCapacityKey,
        ])
        let available = values.volumeAvailableCapacityForImportantUsage
            ?? values.volumeAvailableCapacity.map(Int64.init)
        let (reservedRequirement, overflow) = requiredBytes.addingReportingOverflow(minimumFreeSpaceReserve)
        guard !overflow,
              let available,
              available >= 0,
              UInt64(available) >= reservedRequirement
        else {
            throw AppleArchiveTransferError.insufficientFreeSpace
        }
    }

    private struct DirectoryIdentity: Equatable {
        let device: UInt64
        let inode: UInt64
    }

    private final class RestoreRoot {
        let url: URL
        let descriptor: Int32
        let identity: DirectoryIdentity

        init(url: URL) throws {
            guard url.isFileURL else { throw AppleArchiveTransferError.notFileURL }
            let descriptor = Darwin.open(
                url.path,
                O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC
            )
            guard descriptor >= 0 else { throw AppleArchiveTransferError.notDirectory }
            do {
                self.identity = try AppleArchiveTransfer.directoryIdentity(descriptor: descriptor)
            } catch {
                close(descriptor)
                throw AppleArchiveTransferError.notDirectory
            }
            self.url = url
            self.descriptor = descriptor
        }

        deinit {
            close(descriptor)
        }

        func revalidate() throws {
            guard try AppleArchiveTransfer.directoryIdentity(descriptor: descriptor) == identity else {
                throw AppleArchiveTransferError.destinationChanged
            }
            let currentDescriptor = Darwin.open(
                url.path,
                O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC
            )
            guard currentDescriptor >= 0 else { throw AppleArchiveTransferError.destinationChanged }
            defer { close(currentDescriptor) }
            guard try AppleArchiveTransfer.directoryIdentity(descriptor: currentDescriptor) == identity else {
                throw AppleArchiveTransferError.destinationChanged
            }
        }

        func requireEmpty() throws {
            try revalidate()
            let duplicatedDescriptor = dup(descriptor)
            guard duplicatedDescriptor >= 0,
                  let directory = fdopendir(duplicatedDescriptor)
            else {
                if duplicatedDescriptor >= 0 { close(duplicatedDescriptor) }
                throw AppleArchiveTransfer.posixDestinationError()
            }
            defer { closedir(directory) }
            errno = 0
            while let entry = readdir(directory) {
                let name = withUnsafePointer(to: &entry.pointee.d_name) {
                    $0.withMemoryRebound(to: CChar.self, capacity: Int(MAXNAMLEN) + 1) {
                        String(cString: $0)
                    }
                }
                if name != "." && name != ".." {
                    throw AppleArchiveTransferError.restoreDestinationMustBeEmpty
                }
            }
            guard errno == 0 else { throw AppleArchiveTransfer.posixDestinationError() }
            try revalidate()
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
    case insufficientFreeSpace
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
        case .compressedSizeExceeded: "The transfer archive exceeds the 8 GiB compressed limit."
        case .uncompressedSizeExceeded: "The transfer archive exceeds the 64 GiB expanded limit."
        case .insufficientFreeSpace: "The selected volume does not have enough free space plus Covalent's 256 MiB safety reserve."
        case .restoreDestinationMustBeEmpty:
            "Choose an empty restore folder so the signed no-write preview remains exact."
        case .nonEmptyRestorePlan:
            "The node returned a streamed restore plan that could modify existing files."
        case let .restorePlanMismatch(path): "The restore archive did not match its signed plan at \(path)."
        case .destinationChanged: "The restore folder changed after preview. Choose an empty folder and preview again."
        }
    }
}
