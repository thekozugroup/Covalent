import Foundation
import Testing

@testable import CovalentShared

@Test func renewalKeepsSenderBookmarkAndRetiresRecipientChoiceWithoutTouchingOtherGrants() throws {
  let oldSource = UUID()
  let oldTarget = UUID()
  let source = renewalGrant(oldSource)
  let recipient = renewalGrant(oldTarget)
  let unrelated = renewalGrant(UUID())
  let legacy = renewalGrant(nil)
  let sourceShare = renewalShare(incoming: false, superseded: [oldSource])
  let targetShare = renewalShare(incoming: true, superseded: [oldTarget])
  let result = try FolderSyncGrantReconciliation.reconcile(
    [source, recipient, unrelated, legacy],
    with: renewalStatus([sourceShare, targetShare])
  )
  #expect(result == [source.bound(toFolderOfferId: sourceShare.offerId), unrelated, legacy])
  #expect(result.first?.bookmarkData == source.bookmarkData)
  #expect(result.first?.id == source.id)
  #expect(!result.contains { $0.folderOfferId == targetShare.offerId })
}

@Test func missingRowsAndMatchingLabelsNeverRetireOrRebindGrants() throws {
  let grant = renewalGrant(UUID())
  let status = renewalStatus([renewalShare(incoming: true, superseded: [])])
  #expect(try FolderSyncGrantReconciliation.reconcile([grant], with: status) == [grant])
  #expect(try FolderSyncGrantReconciliation.reconcile([grant], with: renewalStatus([])) == [grant])
}

@Test func ambiguousAndOversizedRenewalRelationshipsFailBeforeGrantChanges() throws {
  let oldID = UUID()
  let grant = renewalGrant(oldID)
  let first = renewalShare(incoming: false, superseded: [oldID])
  let invalid = [
    [first, renewalShare(incoming: true, superseded: [oldID])],
    [renewalShare(incoming: true, superseded: [oldID, oldID])],
    [renewalShare(incoming: true, superseded: (0..<129).map { _ in UUID() })],
    [first, renewalShare(offerID: oldID, incoming: true, superseded: [])],
  ]
  for shares in invalid {
    #expect(throws: NodeClientError.self) {
      try FolderSyncGrantReconciliation.reconcile([grant], with: renewalStatus(shares))
    }
  }
}

@Test func reconciledRenewalBindingsSurvivePersistenceReopening() async throws {
  let directory = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
  defer { try? FileManager.default.removeItem(at: directory) }
  let sender = renewalGrant(UUID())
  let recipient = renewalGrant(UUID())
  let persistence = AppleAppPersistence(directoryURL: directory)
  try await persistence.saveDirectoryGrants([sender, recipient])
  let snapshot = renewalStatus([
    renewalShare(incoming: false, superseded: [sender.folderOfferId!]),
    renewalShare(incoming: true, superseded: [recipient.folderOfferId!]),
  ])
  let updated = try FolderSyncGrantReconciliation.reconcile(
    try await persistence.loadDirectoryGrants(), with: snapshot
  )
  try await persistence.saveDirectoryGrants(updated)
  let reopened = AppleAppPersistence(directoryURL: directory)
  #expect(try await reopened.loadDirectoryGrants() == updated)
  #expect(updated.count == 1)
  #expect(updated.first?.bookmarkData == sender.bookmarkData)
}

private func renewalGrant(_ offerID: UUID?) -> SelectedDirectoryGrant {
  SelectedDirectoryGrant(
    displayName: "Plans", purpose: .folderSync, bookmarkData: Data([1, 2, 3]),
    folderOfferId: offerID
  )
}

private func renewalShare(
  offerID: UUID = UUID(), incoming: Bool, superseded: [UUID]
) -> FolderShare {
  FolderShare(
    offerId: offerID, folderId: UUID(), label: "Plans", peerId: UUID(),
    incoming: incoming, phase: .offered, expiresAtUnixMs: 100, expired: false,
    supersededOfferIds: superseded
  )
}

private func renewalStatus(_ shares: [FolderShare]) -> FolderSyncStatus {
  FolderSyncStatus(
    availability: "available", lifecycle: "running", issue: nil,
    healthFreshness: "fresh", peers: [], shares: shares, folders: []
  )
}
