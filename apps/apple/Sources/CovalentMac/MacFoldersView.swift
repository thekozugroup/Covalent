import AppKit
import SwiftUI

struct MacFoldersView: View {
    @ObservedObject var model: CovalentAppModel

    @State private var chosenPeer: UUID?
    @State private var label = ""
    /// Kept until a successful offer so a network retry reuses the same folder
    /// identity and cannot create a second offer for the same selection.
    @State private var draftFolderId = UUID()
    @State private var pendingOffer: PendingOffer?
    @State private var folderBeingRemoved: FolderShare?

    var body: some View {
      ScrollView {
        VStack(alignment: .leading, spacing: 20) {
          Text("Folders")
            .font(.largeTitle.bold())

          content

          if let status = model.folderSyncStatus, status.availability == "available" {
            shareComposer(status: status)
          }
        }
        .padding(32)
      }
      .navigationTitle("Folders")
      .task { await refreshWhileVisible() }
      .confirmationDialog(
        "Remove this folder from sync?",
        isPresented: Binding(
          get: { folderBeingRemoved != nil },
          set: { if !$0 { folderBeingRemoved = nil } }
        )
      ) {
        Button("Remove and keep local files", role: .destructive) {
          guard let share = folderBeingRemoved else { return }
          Task { await model.removeFolder(share.offerId) }
          folderBeingRemoved = nil
        }
        Button("Cancel", role: .cancel) {}
      } message: {
        Text("This stops sync on this Mac. Local files stay on this Mac.")
      }
    }

    @ViewBuilder
    private var content: some View {
      if model.folderSyncLoading && model.folderSyncStatus == nil {
        ProgressView("Checking folder sync")
      } else if let message = model.folderSyncError {
        ContentUnavailableView(
          "Folder sync is unavailable",
          systemImage: "exclamationmark.triangle",
          description: Text(message)
        )
        Button("Try Again") {
          Task { await model.refreshFolders() }
        }
      } else if let status = model.folderSyncStatus {
        if status.availability != "available" {
          ContentUnavailableView(
            "Folder sync is unavailable",
            systemImage: "exclamationmark.triangle",
            description: Text(unavailableCopy(for: status))
          )
          if status.issue != "folderAccess" {
            Button("Try Again") {
              Task { await model.refreshFolders() }
            }
          }
        } else if visibleShares(in: status).isEmpty {
          ContentUnavailableView(
            "Keep a folder in sync",
            systemImage: "folder.badge.plus",
            description: Text("Choose a paired device and a folder on this Mac.")
          )
        } else {
          shares(status)
        }
      } else {
        ContentUnavailableView(
          "Keep a folder in sync",
          systemImage: "folder.badge.plus",
          description: Text("Choose a paired device and a folder on this Mac.")
        )
      }
    }

    @ViewBuilder
    private func shares(_ status: FolderSyncStatus) -> some View {
      VStack(spacing: 12) {
        ForEach(visibleShares(in: status)) { share in
          shareRow(share, status: status)
        }
      }
    }

    private func shareRow(_ share: FolderShare, status: FolderSyncStatus) -> some View {
      let state = status.displayState(for: share)
      return HStack(alignment: .center, spacing: 16) {
        VStack(alignment: .leading, spacing: 4) {
          Text(share.label)
            .font(.headline)
          Text(peerName(for: share, status: status))
            .font(.subheadline)
            .foregroundStyle(.secondary)
          Text(state.label)
            .font(.subheadline)
            .foregroundStyle(
              state == .needsAttention || state == .invitationExpired ? .red : .secondary)
        }

        Spacer()

        shareActions(share, state: state)
      }
      .padding()
      .background(.quaternary, in: RoundedRectangle(cornerRadius: 12))
    }

    @ViewBuilder
    private func shareActions(_ share: FolderShare, state: FolderShareDisplayState) -> some View {
      if state == .needsAttention {
        Button("Try Again") {
          Task { await model.retryFolderSync() }
        }
        .disabled(!model.isAuthorized || model.folderSyncMutationInFlight)
      }

      if share.incoming && share.phase == .offered {
        Button("Choose folder") {
          chooseFolder(purpose: .folderSync) { grant in
            Task { _ = await model.acceptFolder(offerId: share.offerId, grant: grant) }
          }
        }
        .disabled(!model.isAuthorized || model.folderSyncMutationInFlight || state == .invitationExpired)

        Button(state == .invitationExpired ? "Remove" : "Decline", role: .destructive) {
          Task { await model.removeFolder(share.offerId) }
        }
        .disabled(!model.isAuthorized || model.folderSyncMutationInFlight)
      } else {
        Button(share.phase == .paused ? "Resume" : "Pause") {
          Task {
            await model.setFolderPaused(
              share.offerId,
              paused: share.phase != .paused
            )
          }
        }
        .disabled(!model.isAuthorized || model.folderSyncMutationInFlight || state == .invitationExpired)

        Button("Remove…", role: .destructive) {
          folderBeingRemoved = share
        }
        .disabled(!model.isAuthorized || model.folderSyncMutationInFlight)
      }
    }

    private func shareComposer(status: FolderSyncStatus) -> some View {
      VStack(alignment: .leading, spacing: 12) {
        Text("Keep a folder in sync")
          .font(.title2.bold())

        if status.peers.isEmpty {
          Text("Pair a device before sharing a folder.")
            .foregroundStyle(.secondary)
        } else {
          Picker("Paired device", selection: $chosenPeer) {
            Text("Choose a device").tag(UUID?.none)
            ForEach(status.peers) { peer in
              Text(peer.displayName).tag(Optional(peer.peerId))
            }
          }

          TextField("Folder name", text: $label, prompt: Text("Shared documents"))

          if let pendingOffer {
            Text("Retry the folder you already chose.")
              .font(.caption)
              .foregroundStyle(.secondary)
            Button("Try sharing chosen folder again") {
              submit(pendingOffer)
            }
            .disabled(!model.isAuthorized || model.folderSyncMutationInFlight)
          } else {
            Button("Choose folder and share") {
              guard let peerId = chosenPeer else { return }
              let safeLabel = label.trimmingCharacters(in: .whitespacesAndNewlines)
              chooseFolder(purpose: .folderSync) { grant in
                let offer = PendingOffer(
                  peerId: peerId,
                  folderId: draftFolderId,
                  label: safeLabel.isEmpty ? "Shared folder" : safeLabel,
                  grant: grant
                )
                pendingOffer = offer
                submit(offer)
              }
            }
            .disabled(chosenPeer == nil || !model.isAuthorized || model.folderSyncMutationInFlight)
          }
        }
      }
      .padding()
      .background(.quaternary, in: RoundedRectangle(cornerRadius: 12))
    }

    private func submit(_ offer: PendingOffer) {
      Task {
        let succeeded = await model.offerFolder(
          peerId: offer.peerId,
          folderId: offer.folderId,
          label: offer.label,
          grant: offer.grant
        )
        if succeeded {
          pendingOffer = nil
          draftFolderId = UUID()
          label = ""
        }
      }
    }

    private func refreshWhileVisible() async {
      while !Task.isCancelled {
        await model.refreshFolders()
        do {
          try await Task.sleep(for: .seconds(5))
        } catch {
          return
        }
      }
    }

    private func visibleShares(in status: FolderSyncStatus) -> [FolderShare] {
      status.shares.filter { $0.phase != .removed }
    }

    private func peerName(for share: FolderShare, status: FolderSyncStatus) -> String {
      status.peers.first(where: { $0.peerId == share.peerId })?.displayName ?? "Paired device"
    }

    private func unavailableCopy(for status: FolderSyncStatus) -> String {
      if status.issue == "folderAccess" {
        return "Saved folder access needs to be granted again. Remove the affected sharing, then choose the folder again."
      }
      if status.lifecycle == "needsAttention" {
        return "Folder sync needs attention. Try again after the local service is ready."
      }
      return "The local folder service is not ready. Try again shortly."
    }

    private struct PendingOffer {
      let peerId: UUID
      let folderId: UUID
      let label: String
      let grant: SelectedDirectoryGrant
    }

    private func chooseFolder(
      purpose: DirectoryAccessPurpose,
      completion: @escaping (SelectedDirectoryGrant) -> Void
    ) {
      let panel = NSOpenPanel()
      panel.canChooseDirectories = true
      panel.canChooseFiles = false
      panel.allowsMultipleSelection = false
      guard panel.runModal() == .OK,
        let url = panel.url,
        let grant = try? SelectedDirectoryGrant.capture(url: url, purpose: purpose)
      else {
        return
      }
      completion(grant)
    }
}
