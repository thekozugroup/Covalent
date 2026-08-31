#if os(macOS)
import Darwin
import Foundation
import Security

/// Versioned KEKs and the local API token used only by the bundled macOS node.
///
/// The serialized bytes are intentionally not Codable and never become text.
/// They are read from the data-protection Keychain, written to the child over
/// an inherited pipe, and overwritten as soon as the pipe handoff completes.
struct ManagedNodeKeyMaterial: ~Copyable {
    static let keyLength = 32
    static let maximumKeyCount = 16
    static let minimumTokenLength = 32
    static let maximumTokenLength = 512
    static let maximumSerializedLength = 1_104
    private static let legacyMagic = Array("CVKEK001".utf8)
    private static let magic = Array("CVSEC002".utf8)
    private static let legacyHeaderLength = legacyMagic.count + 4 + 2
    private static let headerLength = magic.count + 4 + 2 + 2
    private static let entryLength = 4 + keyLength

    private(set) var bytes: [UInt8]
    private(set) var apiToken: [UInt8]?

    init(serialized bytes: consuming [UInt8]) throws {
        apiToken = try Self.validate(bytes)
        self.bytes = bytes
    }

    init(
        currentVersion: UInt32,
        keys: [(version: UInt32, bytes: [UInt8])],
        apiToken: consuming [UInt8]
    ) throws {
        guard currentVersion > 0,
              !keys.isEmpty,
              keys.count <= Self.maximumKeyCount,
              keys.contains(where: { $0.version == currentVersion })
        else {
            throw ManagedNodeKeyStoreError.corruptHierarchy
        }
        let sorted = keys.sorted { $0.version < $1.version }
        guard Set(sorted.map(\.version)).count == sorted.count,
              sorted.allSatisfy({ $0.version > 0 && $0.bytes.count == Self.keyLength })
        else {
            throw ManagedNodeKeyStoreError.corruptHierarchy
        }

        guard Self.validToken(apiToken) else {
            throw ManagedNodeKeyStoreError.corruptHierarchy
        }
        var serialized = Self.magic
        serialized.append(contentsOf: Self.encode(currentVersion))
        serialized.append(contentsOf: Self.encode(UInt16(sorted.count)))
        serialized.append(contentsOf: Self.encode(UInt16(apiToken.count)))
        for entry in sorted {
            serialized.append(contentsOf: Self.encode(entry.version))
            serialized.append(contentsOf: entry.bytes)
        }
        serialized.append(contentsOf: apiToken)
        try self.init(serialized: serialized)
    }

    deinit {
        bytes.withUnsafeBytes { buffer in
            guard let baseAddress = buffer.baseAddress else { return }
            bzero(UnsafeMutableRawPointer(mutating: baseAddress), buffer.count)
        }
        apiToken?.withUnsafeBytes { buffer in
            guard let baseAddress = buffer.baseAddress else { return }
            bzero(UnsafeMutableRawPointer(mutating: baseAddress), buffer.count)
        }
    }

    var currentVersion: UInt32 {
        Self.decodeUInt32(bytes, at: Self.magic.count)
    }

    var keyCount: Int {
        Int(Self.decodeUInt16(bytes, at: Self.magic.count + 4))
    }

    var token: String {
        // `validate` already established strict UTF-8 visible ASCII.
        String(decoding: apiToken ?? [], as: UTF8.self)
    }

    func copyToken() -> [UInt8] {
        Array(apiToken ?? [])
    }

    func entries() -> [(version: UInt32, bytes: [UInt8])] {
        var result: [(UInt32, [UInt8])] = []
        result.reserveCapacity(keyCount)
        for index in 0..<keyCount {
            let offset = keyEntriesOffset + (index * Self.entryLength)
            result.append((
                Self.decodeUInt32(bytes, at: offset),
                Array(bytes[(offset + 4)..<(offset + Self.entryLength)])
            ))
        }
        return result
    }

    var isLegacy: Bool {
        Array(bytes.prefix(Self.legacyMagic.count)) == Self.legacyMagic
    }

    private var keyEntriesOffset: Int {
        isLegacy ? Self.legacyHeaderLength : Self.headerLength
    }

    /// Writes without constructing `Data`, avoiding another secret-bearing copy.
    mutating func writeAndErase(to descriptor: Int32) throws {
        defer { erase() }
        // A supervised helper can exit between `Process.run()` and this write.
        // Suppress SIGPIPE on this descriptor only so that race becomes a
        // normal EPIPE error instead of terminating the macOS app process.
        guard Darwin.fcntl(descriptor, F_SETNOSIGPIPE, 1) == 0 else {
            throw ManagedNodeKeyStoreError.pipeWriteFailed
        }
        try bytes.withUnsafeBytes { rawBuffer in
            guard var address = rawBuffer.baseAddress else {
                throw ManagedNodeKeyStoreError.pipeWriteFailed
            }
            var remaining = rawBuffer.count
            while remaining > 0 {
                let count = Darwin.write(descriptor, address, remaining)
                if count > 0 {
                    remaining -= count
                    address = address.advanced(by: count)
                } else if count < 0, errno == EINTR {
                    continue
                } else {
                    throw ManagedNodeKeyStoreError.pipeWriteFailed
                }
            }
        }
    }

    mutating func erase() {
        bytes.withUnsafeMutableBytes { buffer in
            guard let baseAddress = buffer.baseAddress else { return }
            bzero(baseAddress, buffer.count)
        }
        apiToken?.withUnsafeMutableBytes { buffer in
            guard let baseAddress = buffer.baseAddress else { return }
            bzero(baseAddress, buffer.count)
        }
    }

    private static func validate(_ bytes: [UInt8]) throws -> [UInt8]? {
        let isLegacy = Array(bytes.prefix(legacyMagic.count)) == legacyMagic
        let isV2 = Array(bytes.prefix(magic.count)) == magic
        guard isLegacy || isV2 else {
            throw ManagedNodeKeyStoreError.corruptHierarchy
        }
        let activeHeaderLength = isV2 ? headerLength : legacyHeaderLength
        guard bytes.count >= activeHeaderLength else { throw ManagedNodeKeyStoreError.corruptHierarchy }
        let currentVersion = decodeUInt32(bytes, at: magic.count)
        let count = Int(decodeUInt16(bytes, at: magic.count + 4))
        let tokenLength = isV2 ? Int(decodeUInt16(bytes, at: magic.count + 6)) : 0
        guard currentVersion > 0,
              (1...maximumKeyCount).contains(count),
              bytes.count == activeHeaderLength + (count * entryLength) + tokenLength,
              bytes.count <= maximumSerializedLength,
              !isV2 || (minimumTokenLength...maximumTokenLength).contains(tokenLength)
        else {
            throw ManagedNodeKeyStoreError.corruptHierarchy
        }
        var versions = Set<UInt32>()
        var previous: UInt32 = 0
        for index in 0..<count {
            let version = decodeUInt32(bytes, at: activeHeaderLength + (index * entryLength))
            guard version > previous, versions.insert(version).inserted else {
                throw ManagedNodeKeyStoreError.corruptHierarchy
            }
            previous = version
        }
        guard versions.contains(currentVersion) else {
            throw ManagedNodeKeyStoreError.corruptHierarchy
        }
        guard isV2 else { return nil }
        let tokenOffset = activeHeaderLength + (count * entryLength)
        let token = Array(bytes[tokenOffset..<(tokenOffset + tokenLength)])
        guard validToken(token) else { throw ManagedNodeKeyStoreError.corruptHierarchy }
        return token
    }

    private static func validToken(_ bytes: [UInt8]) -> Bool {
        (minimumTokenLength...maximumTokenLength).contains(bytes.count)
            && bytes.allSatisfy { (0x21...0x7e).contains($0) }
    }

    private static func encode(_ value: UInt32) -> [UInt8] {
        let bigEndian = value.bigEndian
        return withUnsafeBytes(of: bigEndian) { Array($0) }
    }

    private static func encode(_ value: UInt16) -> [UInt8] {
        let bigEndian = value.bigEndian
        return withUnsafeBytes(of: bigEndian) { Array($0) }
    }

    private static func decodeUInt32(_ bytes: [UInt8], at offset: Int) -> UInt32 {
        bytes[offset..<(offset + 4)].reduce(0) { ($0 << 8) | UInt32($1) }
    }

    private static func decodeUInt16(_ bytes: [UInt8], at offset: Int) -> UInt16 {
        bytes[offset..<(offset + 2)].reduce(0) { ($0 << 8) | UInt16($1) }
    }
}

protocol ManagedNodeKeyPersisting: Sendable {
    func read() throws -> Data?
    func insert(_ data: Data) throws -> Bool
    func update(_ data: Data) throws
}

struct SecurityManagedNodeKeyPersistence: ManagedNodeKeyPersisting {
    let service: String
    let account: String

    func read() throws -> Data? {
        var query = baseQuery
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        switch status {
        case errSecSuccess:
            guard let data = item as? Data else {
                throw ManagedNodeKeyStoreError.corruptHierarchy
            }
            return data
        case errSecItemNotFound:
            return nil
        case errSecInteractionNotAllowed, errSecNotAvailable, errSecAuthFailed:
            throw ManagedNodeKeyStoreError.keychainLocked
        default:
            throw ManagedNodeKeyStoreError.keychain(status)
        }
    }

    func insert(_ data: Data) throws -> Bool {
        var item = baseQuery
        item[kSecValueData as String] = data
        item[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        let status = SecItemAdd(item as CFDictionary, nil)
        switch status {
        case errSecSuccess:
            return true
        case errSecDuplicateItem:
            return false
        default:
            try throwForStatus(status)
        }
    }

    func update(_ data: Data) throws {
        let update = [kSecValueData as String: data]
        let status = SecItemUpdate(baseQuery as CFDictionary, update as CFDictionary)
        if status == errSecItemNotFound {
            throw ManagedNodeKeyStoreError.missingHierarchy
        }
        guard status == errSecSuccess else { try throwForStatus(status) }
    }

    private var baseQuery: [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecUseDataProtectionKeychain as String: true,
        ]
    }

    private func throwForStatus(_ status: OSStatus) throws -> Never {
        switch status {
        case errSecInteractionNotAllowed, errSecNotAvailable, errSecAuthFailed:
            throw ManagedNodeKeyStoreError.keychainLocked
        default:
            throw ManagedNodeKeyStoreError.keychain(status)
        }
    }
}

struct ManagedNodeKeyStore: Sendable {
    private let persistence: any ManagedNodeKeyPersisting
    private let randomBytes: @Sendable (Int) throws -> [UInt8]

    init(
        service: String = "life.michaelwong.covalent.node-key-encryption",
        account: String = "managed-node-kek-hierarchy"
    ) {
        persistence = SecurityManagedNodeKeyPersistence(service: service, account: account)
        randomBytes = Self.secureRandomBytes
    }

    init(
        persistence: any ManagedNodeKeyPersisting,
        randomBytes: @escaping @Sendable (Int) throws -> [UInt8]
    ) {
        self.persistence = persistence
        self.randomBytes = randomBytes
    }

    func loadOrCreate() throws -> ManagedNodeKeyMaterial {
        if let data = try persistence.read() {
            let existing = try decodeAndErase(data)
            return try migrateLegacySecretIfNeeded(existing)
        }
        var key = try generateKey()
        var token = try generateToken()
        defer {
            Self.erase(&key)
            Self.erase(&token)
        }
        let material = try ManagedNodeKeyMaterial(
            currentVersion: 1,
            keys: [(1, key)],
            apiToken: token
        )
        if try persistNew(material) {
            return material
        }
        // Another app instance provisioned the same account first. Adopt its
        // winner; never overwrite it with this process's independently random key.
        return try loadExisting()
    }

    func loadExisting() throws -> ManagedNodeKeyMaterial {
        guard let data = try persistence.read() else {
            throw ManagedNodeKeyStoreError.missingHierarchy
        }
        let existing = try decodeAndErase(data)
        return try migrateLegacySecretIfNeeded(existing)
    }

    /// Creates the next KEK while retaining every prior version needed to open
    /// already-wrapped state. Rotation fails closed instead of deleting history.
    func rotate() throws -> ManagedNodeKeyMaterial {
        guard let data = try persistence.read() else {
            throw ManagedNodeKeyStoreError.missingHierarchy
        }
        let existing = try decodeAndErase(data)
        let current = try migrateLegacySecretIfNeeded(existing)
        let version = current.currentVersion
        guard current.keyCount < ManagedNodeKeyMaterial.maximumKeyCount else {
            throw ManagedNodeKeyStoreError.historyLimitReached
        }
        guard version < UInt32.max else {
            throw ManagedNodeKeyStoreError.versionExhausted
        }
        var entries = current.entries()
        defer {
            for index in entries.indices {
                Self.erase(&entries[index].bytes)
            }
        }
        entries.append((version + 1, try generateKey()))
        var token = current.copyToken()
        defer { Self.erase(&token) }
        let rotated = try ManagedNodeKeyMaterial(
            currentVersion: version + 1,
            keys: entries,
            apiToken: token
        )
        try persistUpdate(rotated)
        return rotated
    }

    private func decodeAndErase(_ source: consuming Data) throws -> ManagedNodeKeyMaterial {
        var data = source
        defer { data.resetBytes(in: 0..<data.count) }
        return try ManagedNodeKeyMaterial(serialized: Array(data))
    }

    private func persistNew(_ material: borrowing ManagedNodeKeyMaterial) throws -> Bool {
        var data = Data(material.bytes)
        defer { data.resetBytes(in: 0..<data.count) }
        return try persistence.insert(data)
    }

    private func persistUpdate(_ material: borrowing ManagedNodeKeyMaterial) throws {
        var data = Data(material.bytes)
        defer { data.resetBytes(in: 0..<data.count) }
        try persistence.update(data)
    }

    private func generateKey() throws -> [UInt8] {
        let key = try randomBytes(ManagedNodeKeyMaterial.keyLength)
        guard key.count == ManagedNodeKeyMaterial.keyLength else {
            throw ManagedNodeKeyStoreError.randomGenerationFailed
        }
        return key
    }

    private func generateToken() throws -> [UInt8] {
        var random = try randomBytes(48)
        defer { Self.erase(&random) }
        guard random.count == 48 else { throw ManagedNodeKeyStoreError.randomGenerationFailed }
        let alphabet = Array("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_".utf8)
        return random.map { alphabet[Int($0 & 0x3f)] }
    }

    /// Legacy `CVKEK001` records contain only the KEK hierarchy. Update the
    /// same Keychain item once with `CVSEC002`: no key rotation, no split
    /// records, and no interval where a restarted child can observe a token
    /// that does not match its KEKs.
    private func migrateLegacySecretIfNeeded(
        _ source: consuming ManagedNodeKeyMaterial
    ) throws -> ManagedNodeKeyMaterial {
        var legacy = source
        guard legacy.isLegacy else { return legacy }
        var entries = legacy.entries()
        var token = try generateToken()
        defer {
            for index in entries.indices { Self.erase(&entries[index].bytes) }
            Self.erase(&token)
        }
        let upgraded = try ManagedNodeKeyMaterial(
            currentVersion: legacy.currentVersion,
            keys: entries,
            apiToken: token
        )
        try persistUpdate(upgraded)
        legacy.erase()
        return upgraded
    }

    private static func secureRandomBytes(count: Int) throws -> [UInt8] {
        var bytes = [UInt8](repeating: 0, count: count)
        guard SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) == errSecSuccess else {
            throw ManagedNodeKeyStoreError.randomGenerationFailed
        }
        return bytes
    }

    private static func erase(_ bytes: inout [UInt8]) {
        bytes.withUnsafeMutableBytes { buffer in
            guard let baseAddress = buffer.baseAddress else { return }
            bzero(baseAddress, buffer.count)
        }
    }
}

enum ManagedNodeKeyStoreError: Error, Equatable, Sendable {
    case keychainLocked
    case keychain(OSStatus)
    case missingHierarchy
    case corruptHierarchy
    case historyLimitReached
    case versionExhausted
    case randomGenerationFailed
    case pipeWriteFailed
}

extension ManagedNodeKeyStoreError: LocalizedError {
    var errorDescription: String? {
        switch self {
        case .keychainLocked:
            "The Mac Keychain is locked. Unlock this Mac, then start the local Covalent service again."
        case let .keychain(status):
            "The Mac Keychain could not provide the local encryption key (\(status))."
        case .missingHierarchy:
            "The saved local encryption key is missing. Restore its Keychain item before opening existing backups."
        case .corruptHierarchy:
            "The saved local encryption keys in Keychain are unreadable. Restore the correct Keychain item before opening existing backups."
        case .historyLimitReached:
            "The saved encryption-key history is full. Older keys cannot be discarded while existing backups still use them."
        case .versionExhausted:
            "The local encryption-key version cannot be advanced."
        case .randomGenerationFailed:
            "The Mac could not securely generate a local encryption key."
        case .pipeWriteFailed:
            "The local encryption key could not be handed to the bundled Covalent service."
        }
    }
}
#endif
