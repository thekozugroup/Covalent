import Foundation
import Testing
@testable import CovalentShared

@Test func resolvedDirectoryValidatesInsideCoordinatedSecurityScope() async throws {
    let root = FileManager.default.temporaryDirectory
        .appending(path: UUID().uuidString, directoryHint: .isDirectory)
    let selected = root.appending(path: "selected", directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: root) }
    try FileManager.default.createDirectory(at: selected, withIntermediateDirectories: true)
    try Data("cold bookmark fixture".utf8).write(to: selected.appending(path: "note.txt"))

    let encoded = try JSONEncoder().encode(
        SelectedDirectoryGrant.capture(url: selected, purpose: .folderSync)
    )
    let coldGrant = try JSONDecoder().decode(SelectedDirectoryGrant.self, from: encoded)
    let resolved = try coldGrant.resolve()
    let value = try await resolved.withCoordinatedRead { coordinatedURL in
        try String(contentsOf: coordinatedURL.appending(path: "note.txt"), encoding: .utf8)
    }

    #expect(value == "cold bookmark fixture")
}

@Test func legacyDirectoryGrantDecodesWithoutGuessingAFolderShare() throws {
    let grantId = UUID()
    let json = "{\"id\":\"\(grantId.uuidString)\",\"displayName\":\"Legacy\",\"purpose\":\"folderSync\",\"bookmarkData\":\"AA==\",\"capturedAt\":0}"
    let decoder = JSONDecoder()
    decoder.dateDecodingStrategy = .secondsSince1970
    let grant = try decoder.decode(SelectedDirectoryGrant.self, from: Data(json.utf8))
    #expect(grant.id == grantId)
    #expect(grant.folderOfferId == nil)
}

@Test func malformedPendingFolderRepairFailsClosed() async throws {
    let root = FileManager.default.temporaryDirectory
        .appending(path: UUID().uuidString, directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: root) }
    let selected = root.appending(path: "selected", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: selected, withIntermediateDirectories: true)
    let wrongPurpose = try SelectedDirectoryGrant.capture(url: selected, purpose: .backupSource)
    let persistence = AppleAppPersistence(directoryURL: root.appending(path: "state"))
    try await persistence.savePendingFolderRepairs([
        PendingFolderAccessRepair(
            offerId: UUID(),
            replacementGrant: wrongPurpose,
            selectedRoot: selected.path
        )
    ])
    await #expect(throws: FolderAccessRepairError.invalidSavedRepair) {
        _ = try await persistence.loadPendingFolderRepairs()
    }
}

#if os(macOS)
@Test func macAppDeclaresPersistentSecurityScopeEntitlement() throws {
    let appleRoot = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()
    let data = try Data(contentsOf: appleRoot.appending(path: "Config/CovalentMac.entitlements"))
    let entitlements = try #require(
        PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any]
    )

    #expect(entitlements["com.apple.security.app-sandbox"] as? Bool == true)
    #expect(entitlements["com.apple.security.files.user-selected.read-write"] as? Bool == true)
    #expect(entitlements["com.apple.security.files.bookmarks.app-scope"] as? Bool == true)

    let project = try String(
        contentsOf: appleRoot.appending(path: "Project.yml"),
        encoding: .utf8
    )
    #expect(project.contains("com.apple.security.files.bookmarks.app-scope: true"))
}

@Test func bookmarkResolutionRejectsFileWhileSecurityScopeIsActive() throws {
    let root = FileManager.default.temporaryDirectory
        .appending(path: UUID().uuidString, directoryHint: .isDirectory)
    let selected = root.appending(path: "selected")
    defer { try? FileManager.default.removeItem(at: root) }
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
    try Data("replacement".utf8).write(to: selected)
    let bookmark = try selected.bookmarkData(
        options: [.withSecurityScope],
        includingResourceValuesForKeys: [.isDirectoryKey],
        relativeTo: nil
    )
    let grant = SelectedDirectoryGrant(
        displayName: "selected",
        purpose: .folderSync,
        bookmarkData: bookmark
    )
    #expect(throws: SelectedDirectoryError.permissionRevoked) {
        _ = try grant.resolve()
    }
}
#endif
