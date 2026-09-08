import Foundation

/// A renewal keeps the sender's selected folder, but retires the recipient's
/// earlier choice. Only the authenticated node's explicit ID relationship may
/// change a binding; an absent row or a matching label is never sufficient.
enum FolderSyncGrantReconciliation {
  static func reconcile(
    _ grants: [SelectedDirectoryGrant],
    with status: FolderSyncStatus
  ) throws -> [SelectedDirectoryGrant] {
    let replacements = try status.invitationReplacements()
    var boundIDs = Set<UUID>()
    return try grants.compactMap { grant in
      guard grant.purpose == .folderSync, let oldID = grant.folderOfferId else { return grant }
      let updated: SelectedDirectoryGrant
      if let replacement = replacements[oldID] {
        // Never carry recipient consent or its bookmark into a new offer.
        if replacement.incoming { return nil }
        updated = grant.bound(toFolderOfferId: replacement.offerId)
      } else {
        updated = grant
      }
      guard let offerID = updated.folderOfferId, boundIDs.insert(offerID).inserted else {
        throw NodeClientError.invalidResponse
      }
      return updated
    }
  }
}
