import Foundation
#if canImport(Darwin)
import Darwin
#else
import Glibc
#endif

/// Owner-directed recovery exports are intentionally separate from settings.
/// Both files are private regular files before their first byte is committed.
enum RecoveryFilePair {
    static func write(_ export: RecoveryKitExport, kitURL: URL, keyURL: URL) throws {
        let kitURL = kitURL.standardizedFileURL
        let keyURL = keyURL.standardizedFileURL
        guard kitURL.isFileURL, keyURL.isFileURL,
              kitURL.resolvingSymlinksInPath().path != keyURL.resolvingSymlinksInPath().path else {
            throw RecoveryFilePairError.invalidDestination
        }
        guard !FileManager.default.fileExists(atPath: kitURL.path),
              !FileManager.default.fileExists(atPath: keyURL.path)
        else { throw RecoveryFilePairError.destinationExists }
        let stagedKit = try stagePrivate(export.kit, beside: kitURL)
        var stagedKey: String?
        defer {
            unlink(stagedKit)
            if let stagedKey { unlink(stagedKey) }
        }
        stagedKey = try stagePrivate(export.recoveryKey, beside: keyURL)
        do {
            try publishExclusive(stagedKit, to: kitURL)
            try publishExclusive(stagedKey!, to: keyURL)
        } catch {
            // Never remove a published path: another process could have
            // replaced it. An incomplete encrypted kit is harmless without
            // its separately staged code and is reported for an explicit retry.
            throw RecoveryFilePairError.partialSave
        }
    }

    private static func stagePrivate(_ data: Data, beside destination: URL) throws -> String {
        var template = Array(destination.deletingLastPathComponent().path.utf8)
            + Array("/.covalent-recovery.XXXXXX".utf8) + [0]
        let descriptor = mkstemp(&template)
        guard descriptor >= 0 else { throw RecoveryFilePairError.writeFailed }
        let temporary = String(decoding: template.dropLast(), as: UTF8.self)
        var staged = false
        defer { close(descriptor) }
        defer { if !staged { unlink(temporary) } }
        guard fchmod(descriptor, mode_t(0o600)) == 0 else {
            throw RecoveryFilePairError.writeFailed
        }
        try data.withUnsafeBytes { raw in
            guard var address = raw.baseAddress else { throw RecoveryFilePairError.writeFailed }
            var remaining = raw.count
            while remaining > 0 {
                let written = posixWrite(descriptor, address, remaining)
                if written > 0 {
                    remaining -= written
                    address = address.advanced(by: written)
                } else if written < 0, errno == EINTR {
                    continue
                } else {
                    throw RecoveryFilePairError.writeFailed
                }
            }
        }
        guard fsync(descriptor) == 0 else { throw RecoveryFilePairError.writeFailed }
        staged = true
        return temporary
    }

    private static func publishExclusive(_ temporary: String, to destination: URL) throws {
        guard link(temporary, destination.path) == 0 else {
            if errno == EEXIST { throw RecoveryFilePairError.destinationExists }
            throw RecoveryFilePairError.writeFailed
        }
        guard unlink(temporary) == 0 else { throw RecoveryFilePairError.writeFailed }
        let parent = destination.deletingLastPathComponent().path
        let directory = open(parent, O_RDONLY | O_DIRECTORY | O_CLOEXEC)
        guard directory >= 0 else { throw RecoveryFilePairError.writeFailed }
        defer { close(directory) }
        guard fsync(directory) == 0 else { throw RecoveryFilePairError.writeFailed }
    }

    /// Foundation also has a `write` member. Keep the POSIX call explicitly
    /// qualified so this shared source remains valid on Apple platforms.
    private static func posixWrite(
        _ descriptor: Int32,
        _ address: UnsafeRawPointer,
        _ count: Int
    ) -> Int {
        #if canImport(Darwin)
        Darwin.write(descriptor, address, count)
        #else
        Glibc.write(descriptor, address, count)
        #endif
    }
}

enum RecoveryFilePairError: LocalizedError, Equatable {
    case invalidDestination
    case destinationExists
    case partialSave
    case writeFailed

    var errorDescription: String? {
        switch self {
        case .invalidDestination: "Choose two different local file locations for the recovery kit and code."
        case .destinationExists: "Choose new file names. Covalent will not overwrite an existing recovery file."
        case .partialSave: "Covalent saved only part of the recovery pair. It left that encrypted file in place and did not overwrite anything; choose new file names and export again."
        case .writeFailed: "Covalent could not save the owner-only recovery files."
        }
    }
}
