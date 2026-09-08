import Foundation
import Testing
@testable import CovalentShared

@Test func recoveryPairWritesExactOwnerOnlyFilesWithoutOverwrite() throws {
    let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: root) }
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    let kit = root.appending(path: "kit")
    let code = root.appending(path: "code")
    let export = RecoveryKitExport(protocolVersion: 1, kit: Data("kit".utf8), recoveryKey: Data(String(repeating: "A", count: 43).utf8))
    try RecoveryFilePair.write(export, kitURL: kit, keyURL: code)
    #expect(try Data(contentsOf: kit) == Data("kit".utf8))
    #expect(try Data(contentsOf: code) == Data(String(repeating: "A", count: 43).utf8))
    let mode = try #require(
        FileManager.default.attributesOfItem(atPath: kit.path)[.posixPermissions] as? NSNumber
    )
    #expect(mode.uint16Value & 0o077 == 0)
    #expect(throws: RecoveryFilePairError.destinationExists) {
        try RecoveryFilePair.write(export, kitURL: kit, keyURL: code)
    }
}

@Test func recoveryPairRejectsEquivalentDestinations() throws {
    let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: root) }
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    let target = root.appending(path: "same")
    let export = RecoveryKitExport(protocolVersion: 1, kit: Data("kit".utf8), recoveryKey: Data(String(repeating: "A", count: 43).utf8))
    #expect(throws: RecoveryFilePairError.invalidDestination) {
        try RecoveryFilePair.write(export, kitURL: target, keyURL: target)
    }
}
