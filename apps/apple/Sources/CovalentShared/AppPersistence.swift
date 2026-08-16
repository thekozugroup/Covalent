import Foundation
import Security

public final class SecureNodeConnectionStore: @unchecked Sendable {
    private let defaults: UserDefaults
    private let keychainService: String
    private let baseURLKey: String
    private let tokenAccount: String

    public init(
        defaults: UserDefaults = .standard,
        keychainService: String = "life.michaelwong.covalent.local-api",
        baseURLKey: String = "nodeBaseURL",
        tokenAccount: String = "local-api-token"
    ) {
        self.defaults = defaults
        self.keychainService = keychainService
        self.baseURLKey = baseURLKey
        self.tokenAccount = tokenAccount
    }

    public func load() throws -> NodeConnectionConfiguration {
        let url = defaults.string(forKey: baseURLKey)
            .flatMap(URL.init(string:))
            ?? NodeConnectionConfiguration.localDefault.baseURL
        return try NodeConnectionConfiguration(baseURL: url, apiToken: try readToken())
    }

    public func save(_ configuration: NodeConnectionConfiguration) throws {
        defaults.set(configuration.baseURL.absoluteString, forKey: baseURLKey)
        if let token = configuration.apiToken {
            try writeToken(token)
        } else {
            try deleteToken()
        }
    }

    public func clear() throws {
        defaults.removeObject(forKey: baseURLKey)
        try deleteToken()
    }

    public static func parseTokenFile(_ data: Data) throws -> String {
        guard data.count <= 1_024,
              let value = String(data: data, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines),
              (32...512).contains(value.utf8.count)
        else {
            throw NodeClientError.invalidToken
        }
        return value
    }

    private func readToken() throws -> String? {
        var query = baseQuery
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        switch status {
        case errSecSuccess:
            guard let data = item as? Data, let token = String(data: data, encoding: .utf8) else {
                throw ConnectionStoreError.invalidKeychainData
            }
            return token
        case errSecItemNotFound:
            return nil
        default:
            throw ConnectionStoreError.keychain(status)
        }
    }

    private func writeToken(_ token: String) throws {
        let tokenData = Data(token.utf8)
        let update = [kSecValueData as String: tokenData]
        let status = SecItemUpdate(baseQuery as CFDictionary, update as CFDictionary)
        if status == errSecItemNotFound {
            var item = baseQuery
            item[kSecValueData as String] = tokenData
            item[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
            let addStatus = SecItemAdd(item as CFDictionary, nil)
            guard addStatus == errSecSuccess else {
                throw ConnectionStoreError.keychain(addStatus)
            }
        } else if status != errSecSuccess {
            throw ConnectionStoreError.keychain(status)
        }
    }

    private func deleteToken() throws {
        let status = SecItemDelete(baseQuery as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw ConnectionStoreError.keychain(status)
        }
    }

    private var baseQuery: [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: keychainService,
            kSecAttrAccount as String: tokenAccount,
        ]
    }
}

public enum ConnectionStoreError: Error, Equatable, Sendable {
    case keychain(OSStatus)
    case invalidKeychainData
}

extension ConnectionStoreError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case let .keychain(status): "Keychain could not save the local service token (\(status))."
        case .invalidKeychainData: "The saved local service token is unreadable."
        }
    }
}

public actor AppleAppPersistence {
    private let directoryURL: URL
    private let grantsURL: URL
    private let snapshotsURL: URL
    private let decoder = JSONDecoder()
    private let encoder = JSONEncoder()

    public init(directoryURL: URL? = nil) {
        let directory = directoryURL ?? Self.defaultDirectoryURL
        self.directoryURL = directory
        grantsURL = directory.appending(path: "directory-grants.json")
        snapshotsURL = directory.appending(path: "snapshot-history.json")
        decoder.dateDecodingStrategy = .iso8601
        encoder.dateEncodingStrategy = .iso8601
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
    }

    public func loadDirectoryGrants() throws -> [SelectedDirectoryGrant] {
        try load([SelectedDirectoryGrant].self, from: grantsURL, fallback: [])
    }

    public func saveDirectoryGrants(_ grants: [SelectedDirectoryGrant]) throws {
        try save(grants, to: grantsURL)
    }

    public func loadSnapshots() throws -> [SnapshotRecord] {
        try load([SnapshotRecord].self, from: snapshotsURL, fallback: [])
    }

    public func saveSnapshots(_ snapshots: [SnapshotRecord]) throws {
        try save(snapshots, to: snapshotsURL)
    }

    private func load<Value: Decodable>(_ type: Value.Type, from url: URL, fallback: Value) throws -> Value {
        do {
            let data = try Data(contentsOf: url, options: [.mappedIfSafe])
            return try decoder.decode(type, from: data)
        } catch let error as CocoaError where error.code == .fileReadNoSuchFile {
            return fallback
        }
    }

    private func save<Value: Encodable>(_ value: Value, to url: URL) throws {
        try FileManager.default.createDirectory(at: directoryURL, withIntermediateDirectories: true)
        let data = try encoder.encode(value)
        try data.write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
    }

    private static var defaultDirectoryURL: URL {
        let root = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory
        return root.appending(path: "Covalent", directoryHint: .isDirectory)
    }
}
