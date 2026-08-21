import Darwin
import CryptoKit
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
    static let uploadOffsetHeader = "X-Covalent-Upload-Offset"
    static let uploadLengthHeader = "X-Covalent-Upload-Length"
    static let uploadDigestHeader = "X-Covalent-Upload-Digest"

    private static let bufferSize = 64 * 1_024
    static let maximumEntries = 200_000
    private static let maximumDepth = 128
    private static let maximumPathBytes = 4_096
    private static let maximumComponentBytes = 255
    private static let maximumCompressedBytes: UInt64 = 8 << 30
    private static let maximumUncompressedBytes: UInt64 = 64 << 30
    private static let minimumFreeSpaceReserve: UInt64 = 256 << 20

    struct TargetInventoryDraft: Equatable, Sendable {
        let rootIdentity: String
        let totalBytes: UInt64
        let entries: [TargetInventoryEntry]
    }

    static func makeTargetInventory(
        targetURL: URL,
        beforeEntry: (([String]) throws -> Void)? = nil
    ) throws -> TargetInventoryDraft {
        try Task.checkCancellation()
        let root = try BackupRoot(url: targetURL)
        var entries: [TargetInventoryEntry] = []
        var totalBytes = UInt64(0)

        func inventoryDirectory(_ descriptor: Int32, components: [String]) throws {
            try root.revalidate()
            guard components.count <= maximumDepth else {
                throw AppleArchiveTransferError.pathTooDeep
            }
            for name in try directoryEntryNames(descriptor: descriptor) {
                try Task.checkCancellation()
                let component = try validatedComponent(name)
                let childComponents = components + [component]
                let path = try validatedPath(components: childComponents)
                guard entries.count < maximumEntries else {
                    throw AppleArchiveTransferError.tooManyEntries
                }
                try beforeEntry?(childComponents)
                try root.revalidate()
                let child = openat(
                    descriptor,
                    component,
                    O_RDONLY | O_NONBLOCK | O_NOFOLLOW | O_CLOEXEC
                )
                guard child >= 0 else { throw sourceEntryError(path) }
                defer { close(child) }
                let before = try sourceIdentity(descriptor: child, path: path)
                let kind: RestoreEntryKind
                let length: UInt64
                if before.isDirectory {
                    kind = .directory
                    length = 0
                } else if before.isRegularFile {
                    kind = .file
                    length = before.size
                    let (next, overflow) = totalBytes.addingReportingOverflow(length)
                    guard !overflow, next <= maximumUncompressedBytes else {
                        throw AppleArchiveTransferError.uncompressedSizeExceeded
                    }
                    totalBytes = next
                } else {
                    throw AppleArchiveTransferError.unsupportedEntry(path)
                }
                entries.append(TargetInventoryEntry(
                    path: path,
                    kind: kind,
                    length: length,
                    modifiedAtUnixMs: before.modifiedAtUnixMs,
                    identityToken: before.identityToken
                ))
                if before.isDirectory {
                    try inventoryDirectory(child, components: childComponents)
                }
                guard before == (try sourceIdentity(descriptor: child, path: path)) else {
                    throw AppleArchiveTransferError.sourceChanged(path)
                }
            }
            try root.revalidate()
        }

        try inventoryDirectory(root.descriptor, components: [])
        return TargetInventoryDraft(
            rootIdentity: "dev=\(root.identity.device);ino=\(root.identity.inode)",
            totalBytes: totalBytes,
            entries: entries
        )
    }

    static func makeBackupArchive(
        sourceURL: URL,
        beforeEntry: (([String]) throws -> Void)? = nil
    ) throws -> URL {
        try Task.checkCancellation()
        let root = try BackupRoot(url: sourceURL)
        let archiveURL = temporaryArchiveURL(prefix: "covalent-backup")
        let fileManager = FileManager.default
        do {
            let archive = try Archive(url: archiveURL, accessMode: .create)
            var entryCount = 0
            var totalBytes: UInt64 = 0

            func addDirectory(_ directoryDescriptor: Int32, components: [String]) throws {
                try Task.checkCancellation()
                try root.revalidate()
                guard components.count <= maximumDepth else {
                    throw AppleArchiveTransferError.pathTooDeep
                }
                for name in try directoryEntryNames(descriptor: directoryDescriptor) {
                    try Task.checkCancellation()
                    let component = try validatedComponent(name)
                    let childComponents = components + [component]
                    let path = try validatedPath(components: childComponents)
                    entryCount += 1
                    guard entryCount <= maximumEntries else {
                        throw AppleArchiveTransferError.tooManyEntries
                    }
                    try beforeEntry?(childComponents)
                    try root.revalidate()
                    let childDescriptor = openat(
                        directoryDescriptor,
                        component,
                        O_RDONLY | O_NONBLOCK | O_NOFOLLOW | O_CLOEXEC
                    )
                    guard childDescriptor >= 0 else {
                        throw sourceEntryError(path)
                    }
                    defer { close(childDescriptor) }
                    let before = try sourceIdentity(descriptor: childDescriptor, path: path)
                    if before.isDirectory {
                        try archive.addEntry(
                            with: path + "/",
                            type: .directory,
                            uncompressedSize: Int64(0),
                            modificationDate: before.modificationDate,
                            permissions: 0o700,
                            compressionMethod: .none,
                            bufferSize: bufferSize
                        ) { (_: Int64, _: Int) in Data() }
                        try addDirectory(childDescriptor, components: childComponents)
                    } else if before.isRegularFile {
                        let size = before.size
                        let (nextTotal, overflow) = totalBytes.addingReportingOverflow(size)
                        guard !overflow, nextTotal <= maximumUncompressedBytes else {
                            throw AppleArchiveTransferError.uncompressedSizeExceeded
                        }
                        try ensureAvailableCapacity(at: archiveURL, requiredBytes: nextTotal)
                        totalBytes = nextTotal
                        try archive.addEntry(
                            with: path,
                            type: .file,
                            uncompressedSize: Int64(size),
                            modificationDate: before.modificationDate,
                            permissions: 0o600,
                            compressionMethod: .deflate,
                            bufferSize: bufferSize
                        ) { position, count in
                            try Task.checkCancellation()
                            guard position >= 0,
                                  UInt64(position) <= size,
                                  UInt64(count) <= size - UInt64(position)
                            else { throw AppleArchiveTransferError.sourceChanged(path) }
                            return try readExactly(
                                descriptor: childDescriptor,
                                offset: position,
                                count: count,
                                path: path
                            )
                        }
                    } else {
                        throw AppleArchiveTransferError.unsupportedEntry(path)
                    }
                    let after = try sourceIdentity(descriptor: childDescriptor, path: path)
                    guard before == after else {
                        throw AppleArchiveTransferError.sourceChanged(path)
                    }
                    try root.revalidate()
                }
            }

            try addDirectory(root.descriptor, components: [])
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

    private static func directoryEntryNames(descriptor: Int32) throws -> [String] {
        let duplicate = dup(descriptor)
        guard duplicate >= 0, let directory = fdopendir(duplicate) else {
            if duplicate >= 0 { close(duplicate) }
            throw posixSourceError()
        }
        defer { closedir(directory) }
        var names: [String] = []
        errno = 0
        while let entry = readdir(directory) {
            let name = withUnsafePointer(to: &entry.pointee.d_name) {
                $0.withMemoryRebound(to: CChar.self, capacity: Int(MAXNAMLEN) + 1) {
                    String(cString: $0)
                }
            }
            if name != "." && name != ".." { names.append(name) }
        }
        guard errno == 0 else { throw posixSourceError() }
        return names.sorted()
    }

    private static func readExactly(
        descriptor: Int32,
        offset: Int64,
        count: Int,
        path: String
    ) throws -> Data {
        var data = Data(count: count)
        var completed = 0
        try data.withUnsafeMutableBytes { rawBuffer in
            guard let base = rawBuffer.baseAddress else { return }
            while completed < count {
                let read = pread(
                    descriptor,
                    base.advanced(by: completed),
                    count - completed,
                    off_t(offset + Int64(completed))
                )
                if read < 0, errno == EINTR { continue }
                guard read > 0 else { throw AppleArchiveTransferError.sourceChanged(path) }
                completed += read
            }
        }
        return data
    }

    private static func sourceIdentity(descriptor: Int32, path: String) throws -> SourceIdentity {
        var metadata = stat()
        guard fstat(descriptor, &metadata) == 0 else { throw sourceEntryError(path) }
        let kind = metadata.st_mode & S_IFMT
        guard kind == S_IFREG || kind == S_IFDIR, metadata.st_size >= 0 else {
            throw AppleArchiveTransferError.unsupportedEntry(path)
        }
        return SourceIdentity(
            device: UInt64(metadata.st_dev),
            inode: UInt64(metadata.st_ino),
            mode: UInt32(metadata.st_mode),
            size: UInt64(metadata.st_size),
            modifiedSeconds: Int64(metadata.st_mtimespec.tv_sec),
            modifiedNanoseconds: Int64(metadata.st_mtimespec.tv_nsec)
        )
    }

    private static func sourceEntryError(_ path: String) -> Error {
        if errno == ELOOP || errno == ENOTDIR || errno == ENOENT {
            return AppleArchiveTransferError.sourceChanged(path)
        }
        return posixSourceError()
    }

    private static func posixSourceError() -> Error {
        NSError(domain: NSPOSIXErrorDomain, code: Int(errno))
    }

    static func copyDownloadedArchive(_ sourceURL: URL) throws -> URL {
        try Task.checkCancellation()
        let size = try fileSize(sourceURL)
        guard size <= maximumCompressedBytes else {
            throw AppleArchiveTransferError.compressedSizeExceeded
        }
        let destination = temporaryArchiveURL(prefix: "covalent-restore")
        do {
            do {
                try FileManager.default.moveItem(at: sourceURL, to: destination)
            } catch {
                try ensureAvailableCapacity(at: destination, requiredBytes: size)
                try FileManager.default.copyItem(at: sourceURL, to: destination)
            }
            try secureTemporaryFile(destination)
            return destination
        } catch {
            try? FileManager.default.removeItem(at: destination)
            throw error
        }
    }

    static func uploadIdentity(for archiveURL: URL) throws -> (length: UInt64, digest: String) {
        try Task.checkCancellation()
        let length = try fileSize(archiveURL)
        guard length > 0, length <= maximumCompressedBytes else {
            throw AppleArchiveTransferError.compressedSizeExceeded
        }
        let handle = try FileHandle(forReadingFrom: archiveURL)
        defer { try? handle.close() }
        var digest = SHA256()
        while true {
            try Task.checkCancellation()
            guard let data = try handle.read(upToCount: bufferSize), !data.isEmpty else { break }
            digest.update(data: data)
        }
        return (length, digest.finalize().map { String(format: "%02x", $0) }.joined())
    }

    static func requireEmptyDirectory(_ targetURL: URL) throws {
        let root = try RestoreRoot(url: targetURL)
        try root.requireEmpty()
    }

    static func extractRestoreArchive(
        _ archiveURL: URL,
        to targetURL: URL,
        plan: RestorePlan,
        expectedInventory: TargetInventoryDraft? = nil,
        beforeWriting: (() throws -> Void)? = nil,
        beforeEntry: (([String]) throws -> Void)? = nil
    ) throws {
        try Task.checkCancellation()
        let root = try RestoreRoot(url: targetURL)
        if let binding = plan.targetInventory {
            guard let expectedInventory,
                  binding.schemaVersion == 1,
                  binding.rootIdentity == expectedInventory.rootIdentity,
                  binding.entryCount == UInt64(expectedInventory.entries.count),
                  binding.totalBytes == expectedInventory.totalBytes
            else { throw AppleArchiveTransferError.restorePlanMismatch("target inventory") }
            let current = try makeTargetInventory(targetURL: targetURL)
            guard current == expectedInventory else {
                throw AppleArchiveTransferError.destinationChanged
            }
        } else {
            try root.requireEmpty()
        }
        let archive = try Archive(url: archiveURL, accessMode: .read)

        var expected: [String: RestorePreviewEntry] = [:]
        var plannedPaths = Set<String>()
        for planned in plan.entries {
            let planPath = planned.kind == .directory ? planned.destinationPath + "/" : planned.destinationPath
            let components = try validatedArchivePath(planPath, isDirectory: planned.kind == .directory)
            let canonical = components.joined(separator: "/")
            guard plannedPaths.insert(canonical).inserted else {
                throw AppleArchiveTransferError.duplicateEntry(canonical)
            }
            switch (planned.kind, planned.action) {
            case (.directory, .createDirectory),
                 (.file, .createFile),
                 (.file, .replaceFile),
                 (.file, .renameFile):
                expected[canonical] = planned
            case (.directory, .keepDirectory), (.file, .skipFile):
                continue
            default:
                throw AppleArchiveTransferError.restorePlanMismatch(canonical)
            }
        }

        var seen = Set<String>()
        var directoryComponents: [[String]] = []
        var totalBytes: UInt64 = 0
        var entryCount = 0
        for entry in archive {
            try Task.checkCancellation()
            entryCount += 1
            guard entryCount <= maximumEntries else {
                throw AppleArchiveTransferError.tooManyEntries
            }
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
            guard expected[canonical]?.kind == expectedKind else {
                throw AppleArchiveTransferError.restorePlanMismatch(canonical)
            }
            let (nextTotal, overflow) = totalBytes.addingReportingOverflow(entry.uncompressedSize)
            guard !overflow, nextTotal <= maximumUncompressedBytes else {
                throw AppleArchiveTransferError.uncompressedSizeExceeded
            }
            totalBytes = nextTotal
            if isDirectory {
                directoryComponents.append(components)
            }
        }
        guard seen == Set(expected.keys) else {
            throw AppleArchiveTransferError.restorePlanMismatch("missing signed entry")
        }
        try ensureAvailableCapacity(at: targetURL, requiredBytes: totalBytes)
        try beforeWriting?()
        try root.revalidate()
        if let expectedInventory,
           try makeTargetInventory(targetURL: targetURL) != expectedInventory
        {
            throw AppleArchiveTransferError.destinationChanged
        }

        var knownDirectories = ["": root.identity]
        if let expectedInventory {
            try captureExistingDirectories(
                root: root,
                inventory: expectedInventory,
                knownDirectories: &knownDirectories
            )
        }
        var createdDirectories: [[String]] = []
        var createdFiles: [[String]] = []
        var replacements: [ReplacementBackup] = []
        do {
            for components in directoryComponents.sorted(by: { $0.count < $1.count }) {
                try Task.checkCancellation()
                try root.revalidate()
                let descriptor = try openDirectory(
                    root: root,
                    components: components,
                    create: true,
                    knownDirectories: &knownDirectories,
                    createdDirectories: &createdDirectories
                )
                close(descriptor)
            }

            for entry in archive where entry.type == .file {
                try Task.checkCancellation()
                let components = try validatedArchivePath(entry.path, isDirectory: false)
                try beforeEntry?(components)
                try root.revalidate()
                let parentDescriptor = try openDirectory(
                    root: root,
                    components: Array(components.dropLast()),
                    create: false,
                    knownDirectories: &knownDirectories,
                    createdDirectories: &createdDirectories
                )
                defer { close(parentDescriptor) }
                guard let name = components.last else {
                    throw AppleArchiveTransferError.destinationChanged
                }
                let canonical = components.joined(separator: "/")
                guard let planned = expected[canonical], planned.kind == .file else {
                    throw AppleArchiveTransferError.restorePlanMismatch(canonical)
                }
                let writingName: String
                if planned.action == .replaceFile {
                    guard let expectedEntry = expectedInventory?.entries.first(where: { $0.path == canonical }) else {
                        throw AppleArchiveTransferError.restorePlanMismatch(canonical)
                    }
                    try validateExistingFile(
                        parentDescriptor: parentDescriptor,
                        name: name,
                        expected: expectedEntry,
                        path: canonical
                    )
                    writingName = ".covalent-\(UUID().uuidString.lowercased()).tmp"
                } else {
                    guard planned.action == .createFile || planned.action == .renameFile else {
                        throw AppleArchiveTransferError.restorePlanMismatch(canonical)
                    }
                    writingName = name
                }
                let descriptor = openat(
                    parentDescriptor,
                    writingName,
                    O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC,
                    mode_t(0o600)
                )
                guard descriptor >= 0 else {
                    throw posixDestinationError()
                }
                if planned.action != .replaceFile { createdFiles.append(components) }
                defer { close(descriptor) }
                do {
                    _ = try archive.extract(entry, bufferSize: bufferSize) { data in
                        try Task.checkCancellation()
                        try writeAll(data, to: descriptor)
                    }
                    guard fsync(descriptor) == 0 else { throw posixDestinationError() }
                } catch {
                    _ = unlinkat(parentDescriptor, writingName, 0)
                    throw error
                }
                if planned.action == .replaceFile {
                    let expectedEntry = expectedInventory!.entries.first { $0.path == canonical }!
                    try validateExistingFile(
                        parentDescriptor: parentDescriptor,
                        name: name,
                        expected: expectedEntry,
                        path: canonical
                    )
                    let backupName = ".covalent-\(UUID().uuidString.lowercased()).original"
                    let backedUp = linkat(parentDescriptor, name, parentDescriptor, backupName, 0) == 0
                    if backedUp {
                        try validateExistingFile(
                            parentDescriptor: parentDescriptor,
                            name: backupName,
                            expected: expectedEntry,
                            path: canonical
                        )
                    }
                    guard renameat(parentDescriptor, writingName, parentDescriptor, name) == 0 else {
                        if backedUp { _ = unlinkat(parentDescriptor, backupName, 0) }
                        _ = unlinkat(parentDescriptor, writingName, 0)
                        throw posixDestinationError()
                    }
                    replacements.append(ReplacementBackup(
                        components: components,
                        backupName: backedUp ? backupName : nil
                    ))
                }
            }
            try root.revalidate()
            guard fsync(root.descriptor) == 0 else { throw posixDestinationError() }
            cleanupReplacementBackups(
                root: root,
                replacements: replacements,
                knownDirectories: &knownDirectories
            )
        } catch {
            rollback(
                root: root,
                files: createdFiles,
                directories: createdDirectories,
                replacements: replacements,
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

    private static func captureExistingDirectories(
        root: RestoreRoot,
        inventory: TargetInventoryDraft,
        knownDirectories: inout [String: DirectoryIdentity]
    ) throws {
        var ignoredCreated: [[String]] = []
        for entry in inventory.entries where entry.kind == .directory {
            let components = try validatedArchivePath(entry.path + "/", isDirectory: true)
            guard let name = components.last else { throw AppleArchiveTransferError.destinationChanged }
            let parent = try openDirectory(
                root: root,
                components: Array(components.dropLast()),
                create: false,
                knownDirectories: &knownDirectories,
                createdDirectories: &ignoredCreated
            )
            let descriptor = openat(parent, name, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
            guard descriptor >= 0 else {
                close(parent)
                throw AppleArchiveTransferError.destinationChanged
            }
            let identity: SourceIdentity
            let capturedIdentity: DirectoryIdentity
            do {
                identity = try sourceIdentity(descriptor: descriptor, path: entry.path)
                capturedIdentity = try directoryIdentity(descriptor: descriptor)
            } catch {
                close(descriptor)
                close(parent)
                throw error
            }
            close(descriptor)
            close(parent)
            guard identity.isDirectory,
                  identity.identityToken == entry.identityToken,
                  identity.modifiedAtUnixMs == entry.modifiedAtUnixMs
            else { throw AppleArchiveTransferError.destinationChanged }
            knownDirectories[entry.path] = capturedIdentity
        }
    }

    private static func validateExistingFile(
        parentDescriptor: Int32,
        name: String,
        expected: TargetInventoryEntry,
        path: String
    ) throws {
        let descriptor = openat(
            parentDescriptor,
            name,
            O_RDONLY | O_NONBLOCK | O_NOFOLLOW | O_CLOEXEC
        )
        guard descriptor >= 0 else { throw AppleArchiveTransferError.destinationChanged }
        defer { close(descriptor) }
        let identity = try sourceIdentity(descriptor: descriptor, path: path)
        guard expected.kind == .file,
              identity.isRegularFile,
              identity.size == expected.length,
              identity.modifiedAtUnixMs == expected.modifiedAtUnixMs,
              identity.identityToken == expected.identityToken
        else { throw AppleArchiveTransferError.destinationChanged }
    }

    private static func cleanupReplacementBackups(
        root: RestoreRoot,
        replacements: [ReplacementBackup],
        knownDirectories: inout [String: DirectoryIdentity]
    ) {
        var ignoredCreated: [[String]] = []
        for replacement in replacements {
            guard let backupName = replacement.backupName,
                  let parent = try? openDirectory(
                      root: root,
                      components: Array(replacement.components.dropLast()),
                      create: false,
                      knownDirectories: &knownDirectories,
                      createdDirectories: &ignoredCreated
                  )
            else { continue }
            _ = unlinkat(parent, backupName, 0)
            _ = fsync(parent)
            close(parent)
        }
    }

    private static func rollback(
        root: RestoreRoot,
        files: [[String]],
        directories: [[String]],
        replacements: [ReplacementBackup] = [],
        knownDirectories: inout [String: DirectoryIdentity]
    ) {
        var ignoredDirectories: [[String]] = []
        for replacement in replacements.reversed() {
            guard let backupName = replacement.backupName,
                  let name = replacement.components.last,
                  let parent = try? openDirectory(
                      root: root,
                      components: Array(replacement.components.dropLast()),
                      create: false,
                      knownDirectories: &knownDirectories,
                      createdDirectories: &ignoredDirectories
                  )
            else { continue }
            _ = unlinkat(parent, name, 0)
            _ = renameat(parent, backupName, parent, name)
            _ = fsync(parent)
            close(parent)
        }
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

    private struct ReplacementBackup {
        let components: [String]
        let backupName: String?
    }

    private struct SourceIdentity: Equatable {
        let device: UInt64
        let inode: UInt64
        let mode: UInt32
        let size: UInt64
        let modifiedSeconds: Int64
        let modifiedNanoseconds: Int64

        var isDirectory: Bool { mode & UInt32(S_IFMT) == UInt32(S_IFDIR) }
        var isRegularFile: Bool { mode & UInt32(S_IFMT) == UInt32(S_IFREG) }
        var modificationDate: Date {
            Date(
                timeIntervalSince1970: TimeInterval(modifiedSeconds)
                    + TimeInterval(modifiedNanoseconds) / 1_000_000_000
            )
        }
        var modifiedAtUnixMs: UInt64? {
            guard modifiedSeconds >= 0, modifiedNanoseconds >= 0 else { return nil }
            let (seconds, secondsOverflow) = UInt64(modifiedSeconds).multipliedReportingOverflow(by: 1_000)
            let (value, valueOverflow) = seconds.addingReportingOverflow(UInt64(modifiedNanoseconds) / 1_000_000)
            return secondsOverflow || valueOverflow ? nil : value
        }
        var identityToken: String {
            "dev=\(device);ino=\(inode);mode=\(mode);size=\(size);mtime=\(modifiedSeconds).\(modifiedNanoseconds)"
        }
    }

    private final class BackupRoot {
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

        deinit { close(descriptor) }

        func revalidate() throws {
            guard try AppleArchiveTransfer.directoryIdentity(descriptor: descriptor) == identity else {
                throw AppleArchiveTransferError.sourceChanged("selected root")
            }
            let current = Darwin.open(
                url.path,
                O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC
            )
            guard current >= 0 else {
                throw AppleArchiveTransferError.sourceChanged("selected root")
            }
            defer { close(current) }
            guard try AppleArchiveTransfer.directoryIdentity(descriptor: current) == identity else {
                throw AppleArchiveTransferError.sourceChanged("selected root")
            }
        }
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
    case sourceChanged(String)
}

extension AppleArchiveTransferError: LocalizedError {
    var errorDescription: String? {
        switch self {
        case .notFileURL: "Choose a folder on this device, or one from a connected cloud drive."
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
            "Your backup server sent a restore plan that could change files already in that folder."
        case let .restorePlanMismatch(path): "The restore archive did not match its signed plan at \(path)."
        case .destinationChanged: "The restore folder changed after preview. Choose an empty folder and preview again."
        case let .sourceChanged(path): "The backup source changed while reading \(path). Retry after writes stop."
        }
    }
}
