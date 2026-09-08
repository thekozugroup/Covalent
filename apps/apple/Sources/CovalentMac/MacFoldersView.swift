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
    @FocusState private var focusedField: ComposerField?

    var body: some View {
      Form {
        folderContent

        if let status = model.folderSyncStatus,
           status.availability == "available",
           status.issue == nil,
           !status.isInitialScanning {
          shareComposer(status: status)
        }
      }
      .formStyle(.grouped)
      .navigationTitle("Folders")
      .task { await refreshWhileVisible() }
      .confirmationDialog(
        "Remove Shared Folder?",
        isPresented: Binding(
          get: { folderBeingRemoved != nil },
          set: { if !$0 { folderBeingRemoved = nil } }
        )
      ) {
        Button("Remove and Keep Files", role: .destructive) {
          guard let share = folderBeingRemoved else { return }
          Task { await model.removeFolder(share.offerId) }
          folderBeingRemoved = nil
        }
        Button("Cancel", role: .cancel) {}
      } message: {
        Text("Sync stops on this Mac. Files already in the folder stay where they are.")
      }
    }

    @ViewBuilder
    private var folderContent: some View {
      if model.folderSyncLoading && model.folderSyncStatus == nil {
        Section {
          ProgressView("Checking Folder Sync…")
        }
      } else if let message = model.folderSyncError {
        unavailableSection(
          title: "Folder Sync Is Unavailable",
          message: message,
          offersRetry: true
        )
      } else if let status = model.folderSyncStatus {
        let shares = visibleShares(in: status)
        if status.issue == "folderAccess", !shares.isEmpty {
          Section {
            Label(
              "Choose each affected folder again. Covalent keeps the existing share and files while restoring access.",
              systemImage: "exclamationmark.folder"
            )
            .foregroundStyle(.secondary)
            .accessibilityLabel("Folder access needs attention")
            .accessibilityValue("Choose the affected folder again to restore access.")
          } header: {
            Text("Folder Access")
          }
          shareSection(shares, status: status)
        } else if status.availability != "available" {
          unavailableSection(
            title: "Folder Sync Is Unavailable",
            message: unavailableCopy(for: status),
            offersRetry: status.issue != "folderAccess"
          )
        } else if status.isInitialScanning && shares.isEmpty {
          Section {
            ContentUnavailableView(
              "Checking Folders",
              systemImage: "folder.badge.gearshape",
              description: Text("Covalent is checking local folder contents before sync starts.")
            )
          }
        } else if shares.isEmpty {
          Section {
            ContentUnavailableView(
              "Keep a Folder in Sync",
              systemImage: "folder.badge.plus",
              description: Text("Choose a paired device and a folder on this Mac.")
            )
          }
        } else {
          shareSection(shares, status: status)
        }
      } else {
        Section {
          ContentUnavailableView(
            "Keep a Folder in Sync",
            systemImage: "folder.badge.plus",
            description: Text("Choose a paired device and a folder on this Mac.")
          )
        }
      }
    }

    private func unavailableSection(
      title: String,
      message: String,
      offersRetry: Bool
    ) -> some View {
      Section {
        ContentUnavailableView(
          title,
          systemImage: "exclamationmark.triangle",
          description: Text(message)
        )
        if offersRetry {
          Button("Try Again") {
            Task { await model.refreshFolders() }
          }
          .disabled(model.folderSyncLoading)
          .accessibilityHint("Checks the local folder service again.")
        }
      }
    }

    private func shareSection(_ shares: [FolderShare], status: FolderSyncStatus) -> some View {
      Section("Shared Folders") {
        ForEach(shares) { share in
          shareRow(share, status: status)
        }
      }
    }

    private func shareRow(_ share: FolderShare, status: FolderSyncStatus) -> some View {
      let state = status.displayState(for: share)
      return VStack(alignment: .leading, spacing: 8) {
        HStack(alignment: .firstTextBaseline) {
          Label(share.label, systemImage: "folder")
            .font(.headline)
          Spacer()
          Text(status.displayLabel(for: share))
            .font(.subheadline)
            .foregroundStyle(
              state == .needsAttention || state == .invitationExpired
                ? AnyShapeStyle(.red) : AnyShapeStyle(.secondary)
            )
        }
        LabeledContent("Paired Device", value: peerName(for: share, status: status))
          .font(.subheadline)
        HStack {
          Spacer()
          shareActions(share, state: state, status: status)
            .disabled(!model.isAuthorized || model.folderSyncMutationInFlight)
        }
      }
      .padding(.vertical, 4)
    }

    @ViewBuilder
    private func shareActions(
      _ share: FolderShare,
      state: FolderShareDisplayState,
      status: FolderSyncStatus
    ) -> some View {
      if status.issue == "folderAccess" {
        if model.hasPendingFolderAccessRepair(for: share.offerId) {
          Button("Finish Repair") {
            Task { _ = await model.retryFolderAccessRepair(offerId: share.offerId) }
          }
          .keyboardShortcut(.defaultAction)
          .accessibilityHint("Retries the saved replacement for \(share.label).")
        }
        Button("Choose Again…") {
          chooseFolder(purpose: .folderSync) { grant in
            Task { _ = await model.repairFolderAccess(offerId: share.offerId, grant: grant) }
          }
        }
        .accessibilityLabel("Choose folder again for \(share.label)")
        .accessibilityHint("Opens the system folder picker and preserves the share identity.")

        Button("Remove…", role: .destructive) {
          folderBeingRemoved = share
        }
        .accessibilityLabel("Remove \(share.label) from sync")
      } else if share.incoming && share.phase == .offered {
        Button("Choose Folder…") {
          chooseFolder(purpose: .folderSync) { grant in
            Task { _ = await model.acceptFolder(offerId: share.offerId, grant: grant) }
          }
        }
        .keyboardShortcut(.defaultAction)
        .disabled(state == .invitationExpired)
        .accessibilityLabel("Choose a folder for \(share.label)")

        Button(state == .invitationExpired ? "Remove" : "Decline", role: .destructive) {
          Task { await model.removeFolder(share.offerId) }
        }
      } else {
        if state == .needsAttention {
          Button("Try Again") {
            Task { await model.retryFolderSync() }
          }
        }

        Button(share.phase == .paused ? "Resume" : "Pause") {
          Task {
            await model.setFolderPaused(
              share.offerId,
              paused: share.phase != .paused
            )
          }
        }
        .disabled(state == .invitationExpired)
        .accessibilityLabel("\(share.phase == .paused ? "Resume" : "Pause") \(share.label)")

        Button("Remove…", role: .destructive) {
          folderBeingRemoved = share
        }
        .accessibilityLabel("Remove \(share.label) from sync")
      }
    }

    private func shareComposer(status: FolderSyncStatus) -> some View {
      Section("Share a Folder") {
        if status.peers.isEmpty {
          Label("Pair a device before sharing a folder.", systemImage: "laptopcomputer.and.iphone")
            .foregroundStyle(.secondary)
        } else {
          Picker("Paired Device", selection: $chosenPeer) {
            Text("Choose a Device").tag(UUID?.none)
            ForEach(status.peers) { peer in
              Text(peer.displayName).tag(Optional(peer.peerId))
            }
          }
          .accessibilityHint("Selects the paired device that will receive this folder offer.")

          TextField("Folder Name", text: $label, prompt: Text("Shared Documents"))
            .focused($focusedField, equals: .label)
            .accessibilityHint("Names the folder for the paired device.")

          if let pendingOffer {
            Label("The previous result was uncertain. Retry the same folder offer.", systemImage: "arrow.clockwise")
              .font(.callout)
              .foregroundStyle(.secondary)
            Button("Try Sharing Again") {
              submit(pendingOffer)
            }
            .keyboardShortcut(.defaultAction)
            .disabled(!model.isAuthorized || model.folderSyncMutationInFlight)
          } else {
            Button("Choose Folder and Share…") {
              guard let peerId = chosenPeer else { return }
              let safeLabel = label.trimmingCharacters(in: .whitespacesAndNewlines)
              chooseFolder(purpose: .folderSync) { grant in
                let offer = PendingOffer(
                  peerId: peerId,
                  folderId: draftFolderId,
                  label: safeLabel.isEmpty ? grant.displayName : safeLabel,
                  grant: grant
                )
                pendingOffer = offer
                submit(offer)
              }
            }
            .keyboardShortcut(.defaultAction)
            .disabled(chosenPeer == nil || !model.isAuthorized || model.folderSyncMutationInFlight)
            .accessibilityHint("Opens the system folder picker, then sends one folder offer.")
          }
        }
      }
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
          focusedField = nil
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
      status.peers.first(where: { $0.peerId == share.peerId })?.displayName ?? "Paired Device"
    }

    private func unavailableCopy(for status: FolderSyncStatus) -> String {
      if status.issue == "folderAccess" {
        return "Saved folder access needs attention. Choose the affected folder again to preserve its existing share."
      }
      if status.lifecycle == "needsAttention" {
        return "Folder sync needs attention. Try again after the local service is ready."
      }
      return "The local folder service is not ready. Try again shortly."
    }

    private enum ComposerField: Hashable {
      case label
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
      panel.prompt = "Choose"
      panel.message = "Choose a folder on this Mac. Covalent will save access only for that folder."
      guard panel.runModal() == .OK,
        let url = panel.url,
        let grant = try? SelectedDirectoryGrant.capture(url: url, purpose: purpose)
      else {
        return
      }
      completion(grant)
    }
}
