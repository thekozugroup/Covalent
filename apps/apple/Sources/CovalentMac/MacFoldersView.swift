import AppKit
import SwiftUI

struct MacFoldersView: View {
    @ObservedObject var model: CovalentAppModel

    @State private var chosenPeer: UUID?
    @State private var label = ""
    @State private var linkPolicy = FolderLinkPolicy()
    /// Kept until a successful offer so a network retry reuses the same folder
    /// identity and cannot create a second offer for the same selection.
    @State private var draftFolderId = UUID()
    @State private var pendingOffer: PendingOffer?
    @State private var folderBeingRemoved: FolderShare?
    @State private var destinationSourceOfferId: UUID?
    @State private var destinationPeer: UUID?
    @State private var settingsEditor: FolderLinkSettingsEditorContext?
    @State private var confirmsNewLinkConsequences = false
    @FocusState private var focusedField: ComposerField?

    var body: some View {
      Form {
        folderContent

        if let status = model.folderSyncStatus {
          let pending = status.shares.filter { $0.phase == .removed && $0.remoteRemovalPending }
          if !pending.isEmpty {
            Section("Removal Pending") {
              ForEach(pending) { share in
                VStack(alignment: .leading, spacing: 8) {
                  Label(share.label, systemImage: "folder")
                    .font(.headline)
                  Text("Stopped on This Mac")
                  Text("Covalent will confirm removal when \(peerName(for: share, status: status)) reconnects. Files stay on both devices.")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                }
                .padding(.vertical, 4)
              }
            }
          }
        }

        if let status = model.folderSyncStatus,
           status.availability == "available",
           status.issue == nil,
           !status.isInitialScanning {
          if let source = status.shares.first(where: {
            $0.offerId == destinationSourceOfferId && !$0.incoming && $0.phase != .removed
          }) {
            destinationComposer(source: source, status: status)
          } else {
            shareComposer(status: status)
          }
        }
      }
      .formStyle(.grouped)
      .navigationTitle("Links")
      .task { await refreshWhileVisible() }
      .sheet(item: $settingsEditor) { editor in
        MacFolderLinkSettingsEditor(editor: editor) { settings in
          Task {
            _ = await model.updateFolderLinkSettings(
              folderId: editor.folderId,
              expectedRevision: editor.expectedRevision,
              settings: settings
            )
          }
        }
      }
      .confirmationDialog(
        "Create Link With File Consequences?",
        isPresented: $confirmsNewLinkConsequences
      ) {
        Button("Continue to Choose Folder", role: .destructive) {
          chooseSourceFolderForOffer()
        }
        Button("Cancel", role: .cancel) {}
      } message: {
        Text(newLinkConsequenceConfirmation)
      }
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
        Text("Sync stops on this Mac now and on the other device when it reconnects. Files stay on both devices.")
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
      Section("Links") {
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
        if let policy = share.linkPolicy {
          Label(share.incoming ? "Receives files from the source" : "Sends files to the destination",
                systemImage: share.incoming ? "arrow.down.circle" : "arrow.up.circle")
            .font(.subheadline)
          Text(policy.sourceDeletionExplanation)
            .font(.callout)
            .foregroundStyle(.secondary)
          Text(policy.destinationDeletionExplanation)
            .font(.callout)
            .foregroundStyle(.secondary)
          if isSettingsRow(share, status: status), let settings = share.linkSettings {
            linkSettingsStatus(settings, share: share)
          }
        } else {
          Text("Existing two-way folder")
            .font(.callout)
            .foregroundStyle(.secondary)
        }
        if share.expired {
          Text(share.incoming
            ? "Ask the other device to send a new invitation, then choose your folder again."
            : "Send a new invitation so the other device can choose its folder and accept again.")
            .font(.subheadline)
            .foregroundStyle(.secondary)
        }
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
        .accessibilityLabel(
          "\(state == .invitationExpired ? "Remove" : "Decline") \(share.label) invitation"
        )
      } else {
        if !share.incoming, share.linkPolicy != nil, isSettingsRow(share, status: status) {
          if hasConfirmedSettings(for: share, status: status) {
            Button("Add Destination…") {
              destinationSourceOfferId = share.offerId
              destinationPeer = nil
            }
            .accessibilityLabel("Add a destination for \(share.label)")
          } else {
            Label("Add a destination after source settings are confirmed", systemImage: "clock")
              .foregroundStyle(.secondary)
          }
        }

        if isSettingsRow(share, status: status), let settings = share.linkSettings {
          linkSettingsActions(settings, share: share)
        }

        if state == .needsAttention {
          Button("Try Again") {
            Task { await model.retryFolderSync() }
          }
        }

        if share.expired && !share.incoming && (share.phase == .offered || share.phase == .paused) {
          Button("Send New Invitation") {
            Task { _ = await model.renewFolderInvitation(share.offerId) }
          }
          .accessibilityLabel("Send a new invitation for \(share.label)")
          .accessibilityHint("The other device must choose a folder and accept again.")
        } else {
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
        }

        Button("Remove…", role: .destructive) {
          folderBeingRemoved = share
        }
        .accessibilityLabel("Remove \(share.label) from sync")
      }
    }

    @ViewBuilder
    private func linkSettingsStatus(
      _ state: FolderLinkSettingsState,
      share: FolderShare
    ) -> some View {
      if let pending = state.pendingChange {
        Label("Waiting for the source to apply this settings change", systemImage: "clock")
          .foregroundStyle(.secondary)
        Text(settingsSummary(pending.settings))
          .font(.callout)
          .foregroundStyle(.secondary)
      } else if let conflict = state.conflictedChange {
        Label("Settings change needs review", systemImage: "exclamationmark.triangle")
          .foregroundStyle(.orange)
        Text("Attempted: \(settingsSummary(conflict.settings))")
          .font(.callout)
          .foregroundStyle(.secondary)
      } else if let saved = model.pendingFolderLinkSettingsChanges.first(where: {
        $0.folderId == share.folderId
      }) {
        Label(
          saved.requiresReview ? "Saved settings change needs review" : "Settings change is not confirmed",
          systemImage: saved.requiresReview ? "exclamationmark.triangle" : "wifi.exclamationmark"
        )
        .foregroundStyle(saved.requiresReview ? .orange : .secondary)
        Text(settingsSummary(saved.settings))
          .font(.callout)
          .foregroundStyle(.secondary)
      } else if !state.confirmed {
        Label("Waiting for confirmed source settings", systemImage: "clock")
          .foregroundStyle(.secondary)
      }
    }

    @ViewBuilder
    private func linkSettingsActions(
      _ state: FolderLinkSettingsState,
      share: FolderShare
    ) -> some View {
      if let conflict = state.conflictedChange {
        Button("Review Settings…") {
          settingsEditor = FolderLinkSettingsEditorContext(
            folderId: share.folderId,
            label: share.label,
            expectedRevision: state.revision,
            current: state.settings,
            proposed: conflict.settings
          )
        }
      } else if let saved = model.pendingFolderLinkSettingsChanges.first(where: {
        $0.folderId == share.folderId
      }) {
        if saved.requiresReview {
          Button("Review Settings…") {
            Task {
              await model.reviewPendingFolderLinkSettingsChange(folderId: share.folderId)
              settingsEditor = FolderLinkSettingsEditorContext(
                folderId: share.folderId,
                label: share.label,
                expectedRevision: state.revision,
                current: state.settings,
                proposed: saved.settings
              )
            }
          }
        } else {
          Button("Try Sending Settings Again") {
            Task { _ = await model.retryFolderLinkSettingsChange(folderId: share.folderId) }
          }
        }
      } else if state.pendingChange == nil && state.confirmed {
        Button("Edit Link Settings…") {
          settingsEditor = FolderLinkSettingsEditorContext(
            folderId: share.folderId,
            label: share.label,
            expectedRevision: state.revision,
            current: state.settings,
            proposed: state.settings
          )
        }
      }
    }

    private func isSettingsRow(_ share: FolderShare, status: FolderSyncStatus) -> Bool {
      status.shares.first(where: {
        $0.folderId == share.folderId && $0.phase != .removed
      })?.offerId == share.offerId
    }

    private func hasConfirmedSettings(
      for share: FolderShare,
      status: FolderSyncStatus
    ) -> Bool {
      let linkShares = status.shares.filter {
        $0.folderId == share.folderId && $0.phase != .removed
      }
      guard let state = share.linkSettings,
            state.confirmed,
            state.pendingChange == nil,
            state.conflictedChange == nil
      else { return false }
      return linkShares.allSatisfy { $0.linkSettings == state }
    }

    private func settingsSummary(_ settings: FolderLinkSettings) -> String {
      let pause = settings.paused ? "Paused" : "Running"
      return "\(pause). \(settings.deletionPolicy.sourceDeletionExplanation) \(settings.deletionPolicy.destinationDeletionExplanation)"
    }

    private func destinationComposer(source: FolderShare, status: FolderSyncStatus) -> some View {
      let existingPeers = Set(status.shares.filter {
        $0.folderId == source.folderId && $0.phase != .removed
      }.map(\.peerId))
      let availablePeers = status.peers.filter { !existingPeers.contains($0.peerId) }
      return Section("Add Destination") {
        LabeledContent("Source Link", value: source.label)
        Text("Covalent will reuse this link’s source folder and deletion settings.")
          .font(.callout)
          .foregroundStyle(.secondary)

        if availablePeers.isEmpty {
          Label("Every paired device is already a destination for this link.", systemImage: "checkmark.circle")
            .foregroundStyle(.secondary)
          Button("Done", role: .cancel) {
            destinationSourceOfferId = nil
            destinationPeer = nil
          }
        } else {
          Picker("Paired Device", selection: $destinationPeer) {
            Text("Choose a Device").tag(UUID?.none)
            ForEach(availablePeers) { peer in
              Text(peer.displayName).tag(Optional(peer.peerId))
            }
          }
          .accessibilityHint("Selects one additional destination for \(source.label).")

          HStack {
            Button("Cancel", role: .cancel) {
              destinationSourceOfferId = nil
              destinationPeer = nil
            }
            Spacer()
            Button("Add Destination") {
              guard let peerId = destinationPeer else { return }
              Task {
                if await model.addFolderDestination(from: source.offerId, to: peerId) {
                  destinationSourceOfferId = nil
                  destinationPeer = nil
                }
              }
            }
            .keyboardShortcut(.defaultAction)
            .disabled(
              destinationPeer == nil
                || !hasConfirmedSettings(for: source, status: status)
                || !model.isAuthorized
                || model.folderSyncMutationInFlight
            )
          }
        }
      }
    }

    private func shareComposer(status: FolderSyncStatus) -> some View {
      Section("New One-Way Link") {
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
          .disabled(pendingOffer != nil)

          TextField("Folder Name", text: $label, prompt: Text("Shared Documents"))
            .focused($focusedField, equals: .label)
            .accessibilityHint("Names the folder for the paired device.")
            .disabled(pendingOffer != nil)

          Text("This Mac is the source. Changes on the destination never change files on this Mac.")
            .font(.callout)
            .foregroundStyle(.secondary)
          Picker("When Source Files Are Deleted", selection: $linkPolicy.propagateSourceDeletions) {
            Text("Keep Destination Copies").tag(false)
            Text("Delete Destination Copies Too").tag(true)
          }
          .disabled(pendingOffer != nil)
          Text(linkPolicy.sourceDeletionExplanation)
            .font(.callout)
            .foregroundStyle(.secondary)
          Picker("When Destination Files Are Deleted", selection: $linkPolicy.restoreLocalDeletions) {
            Text("Keep Them Deleted").tag(false)
            Text("Restore from Source").tag(true)
          }
          .disabled(pendingOffer != nil)
          Text(linkPolicy.destinationDeletionExplanation)
            .font(.callout)
            .foregroundStyle(.secondary)

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
            Button("Choose Source Folder…") {
              if linkPolicy.propagateSourceDeletions || linkPolicy.restoreLocalDeletions {
                confirmsNewLinkConsequences = true
              } else {
                chooseSourceFolderForOffer()
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
          grant: offer.grant,
          linkPolicy: offer.linkPolicy
        )
        if succeeded {
          pendingOffer = nil
          draftFolderId = UUID()
          label = ""
          focusedField = nil
        }
      }
    }

    private func chooseSourceFolderForOffer() {
      guard let peerId = chosenPeer else { return }
      let safeLabel = label.trimmingCharacters(in: .whitespacesAndNewlines)
      let selectedPolicy = linkPolicy
      chooseFolder(purpose: .folderSync) { grant in
        let offer = PendingOffer(
          peerId: peerId,
          folderId: draftFolderId,
          label: safeLabel.isEmpty ? grant.displayName : safeLabel,
          grant: grant,
          linkPolicy: selectedPolicy
        )
        pendingOffer = offer
        submit(offer)
      }
    }

    private var newLinkConsequenceConfirmation: String {
      var consequences: [String] = []
      if linkPolicy.propagateSourceDeletions {
        consequences.append("Deleting a source file will also delete this link’s destination copies.")
      }
      if linkPolicy.restoreLocalDeletions {
        consequences.append("A file deleted at a destination will download again if it still exists at the source.")
      }
      return consequences.joined(separator: " ")
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
      let linkPolicy: FolderLinkPolicy
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

private struct FolderLinkSettingsEditorContext: Identifiable {
  let folderId: UUID
  let label: String
  let expectedRevision: UInt64
  let current: FolderLinkSettings
  let proposed: FolderLinkSettings

  var id: UUID { folderId }
}

private struct MacFolderLinkSettingsEditor: View {
  @Environment(\.dismiss) private var dismiss
  let editor: FolderLinkSettingsEditorContext
  let save: (FolderLinkSettings) -> Void
  @State private var proposed: FolderLinkSettings
  @State private var confirmsConsequences = false

  init(
    editor: FolderLinkSettingsEditorContext,
    save: @escaping (FolderLinkSettings) -> Void
  ) {
    self.editor = editor
    self.save = save
    _proposed = State(initialValue: editor.proposed)
  }

  var body: some View {
    Form {
      Section("\(editor.label) Settings") {
        Toggle("Pause this link", isOn: $proposed.paused)
        Toggle(
          "Delete destination copies when source files are deleted",
          isOn: $proposed.deletionPolicy.propagateSourceDeletions
        )
        Text(proposed.deletionPolicy.sourceDeletionExplanation)
          .font(.callout)
          .foregroundStyle(.secondary)
        Toggle(
          "Restore files deleted at a destination",
          isOn: $proposed.deletionPolicy.restoreLocalDeletions
        )
        Text(proposed.deletionPolicy.destinationDeletionExplanation)
          .font(.callout)
          .foregroundStyle(.secondary)
      }
      HStack {
        Button("Cancel", role: .cancel) { dismiss() }
        Spacer()
        Button("Save") {
          if enablesConsequences {
            confirmsConsequences = true
          } else {
            commit()
          }
        }
        .keyboardShortcut(.defaultAction)
        .disabled(proposed == editor.current)
      }
    }
    .formStyle(.grouped)
    .frame(width: 520, height: 390)
    .confirmationDialog(
      "Apply Settings With File Consequences?",
      isPresented: $confirmsConsequences
    ) {
      Button("Apply Settings", role: .destructive) { commit() }
      Button("Cancel", role: .cancel) {}
    } message: {
      Text(consequenceConfirmation)
    }
  }

  private var enablesConsequences: Bool {
    (!editor.current.deletionPolicy.propagateSourceDeletions
      && proposed.deletionPolicy.propagateSourceDeletions)
      || (!editor.current.deletionPolicy.restoreLocalDeletions
        && proposed.deletionPolicy.restoreLocalDeletions)
  }

  private var consequenceConfirmation: String {
    var consequences: [String] = []
    if !editor.current.deletionPolicy.propagateSourceDeletions
      && proposed.deletionPolicy.propagateSourceDeletions {
      consequences.append("Deleting a source file will also delete every destination copy for this link.")
    }
    if !editor.current.deletionPolicy.restoreLocalDeletions
      && proposed.deletionPolicy.restoreLocalDeletions {
      consequences.append("A file deleted at a destination will download again if it still exists at the source.")
    }
    return consequences.joined(separator: " ")
  }

  private func commit() {
    save(proposed)
    dismiss()
  }
}
