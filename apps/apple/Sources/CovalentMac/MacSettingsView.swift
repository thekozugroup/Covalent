import AppKit
import SwiftUI
import UniformTypeIdentifiers

struct MacSettingsView: View {
    @ObservedObject var model: CovalentAppModel
    var compact = false
    @State private var deviceName = ""
    @State private var lanDiscoveryEnabled = false
    @State private var isSaving = false
    @State private var grantToRemove: SelectedDirectoryGrant?
    @State private var confirmRecoveryExport = false
    @State private var isExportingRecovery = false
    @State private var recoveryExportNotice: String?
    @State private var confirmRecoveryRetry = false
    @State private var recoveryExportSession: RecoveryKitExport?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 26) {
                if !compact {
                    Text("Settings")
                        .font(.largeTitle.weight(.semibold))
                        .accessibilityAddTraits(.isHeader)
                }
                general
                settingsTransfer
                ownerRecovery
                folderAccess
                connection
                platformLimits
            }
            .frame(maxWidth: compact ? 560 : 760, alignment: .leading)
            .padding(compact ? 24 : 32)
        }
        .background(Color(nsColor: .windowBackgroundColor))
        .navigationTitle(compact ? "" : "Settings")
        .onAppear { syncFields() }
        .onChange(of: model.settings) { _, _ in syncFields() }
        .onDisappear {
            if var recoveryExportSession {
                recoveryExportSession.discard()
            }
            recoveryExportSession = nil
        }
        .confirmationDialog(
            "Remove folder access?",
            isPresented: Binding(
                get: { grantToRemove != nil },
                set: { if !$0 { grantToRemove = nil } }
            ),
            titleVisibility: .visible
        ) {
            Button("Remove Access", role: .destructive) {
                guard let grantToRemove else { return }
                Task { await model.removeDirectoryGrant(id: grantToRemove.id) }
                self.grantToRemove = nil
            }
            Button("Cancel", role: .cancel) { grantToRemove = nil }
        } message: {
            Text("Future backups or restores using this folder will ask you to choose it again. Existing encrypted backups are unchanged.")
        }
        .confirmationDialog(
            "Create a recovery kit?",
            isPresented: $confirmRecoveryExport,
            titleVisibility: .visible
        ) {
            Button("Create Recovery Kit") { exportRecoveryKit() }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Save both recovery files in different safe places. You’ll need both if you lose this device.")
        }
        .confirmationDialog(
            "Retry recovery check?",
            isPresented: $confirmRecoveryRetry,
            titleVisibility: .visible
        ) {
            Button("Check Again") { Task { await model.retryRecovery() } }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Covalent will check your backup devices again. Keep them online while the check runs.")
        }
    }

    private var general: some View {
        settingsSection("General", systemImage: "slider.horizontal.3") {
            VStack(alignment: .leading, spacing: 14) {
                TextField("Device name", text: $deviceName)
                    .textFieldStyle(.roundedBorder)
                Toggle("Find devices on the local network", isOn: $lanDiscoveryEnabled)
                Text("Turning this off stops mDNS advertising and browsing. Manual addresses and Tailscale candidates remain available.")
                    .font(.caption)
                    .secondaryLabelStyle()
                HStack {
                    Spacer()
                    Button("Save Changes") {
                        isSaving = true
                        Task {
                            _ = await model.updateSettings(
                                deviceName: deviceName,
                                lanDiscoveryEnabled: lanDiscoveryEnabled
                            )
                            isSaving = false
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(!model.isAuthorized || isSaving || !fieldsChanged)
                }
            }
        }
    }

    private var settingsTransfer: some View {
        settingsSection("Settings transfer", systemImage: "arrow.left.arrow.right") {
            VStack(alignment: .leading, spacing: 12) {
                Text("Export only the device name, LAN discovery preference, and remembered backup list.")
                    .secondaryLabelStyle()
                HStack {
                    Button("Export Settings…") { exportSettings() }
                        .disabled(!model.isAuthorized)
                    Button("Import Settings…") { model.presentation = .importSettings }
                        .disabled(!model.isAuthorized)
                }
                Label("Private identity keys and folder permissions never leave this device.", systemImage: "lock.shield")
                    .font(.caption)
                    .secondaryLabelStyle()
            }
        }
    }

    private var ownerRecovery: some View {
        settingsSection("Recovery files", systemImage: "key.viewfinder") {
            VStack(alignment: .leading, spacing: 12) {
                Text("Save recovery files now so you can get your backups back if you lose this Mac.")
                    .secondaryLabelStyle()
                HStack {
                    Button("Create Recovery Kit…") { confirmRecoveryExport = true }
                        .disabled(!model.isAuthorized || isExportingRecovery)
                        .accessibilityIdentifier("recovery.export")
                    if isExportingRecovery { ProgressView().controlSize(.small) }
                    if let phase = model.recoveryStatus?.phase,
                       [.pending, .partial, .blocked, .noCatalogs].contains(phase) {
                        Button("Check Again") { confirmRecoveryRetry = true }
                        .accessibilityIdentifier("recovery.retry")
                    }
                }
                if let recoveryExportNotice {
                    Text(recoveryExportNotice)
                        .font(.caption)
                        .secondaryLabelStyle()
                }
                if let status = model.recoveryStatus {
                    Text(recoveryStatusCopy(status))
                        .font(.caption)
                        .secondaryLabelStyle()
                    if status.newerSnapshotMayExist {
                        Label("A newer backup may exist on a device that has not responded yet.", systemImage: "exclamationmark.triangle.fill")
                            .font(.caption.weight(.semibold))
                            .foregroundStyle(.orange)
                    }
                }
            }
        }
        .task { await model.loadRecoveryStatus() }
    }

    private var folderAccess: some View {
        settingsSection("Folder access", systemImage: "folder.badge.gearshape") {
            if model.directoryGrants.isEmpty {
                Text("No persistent folder permissions saved.")
                    .secondaryLabelStyle()
            } else {
                VStack(spacing: 0) {
                    ForEach(model.directoryGrants) { grant in
                        HStack {
                            Image(systemName: grant.purpose == .backupSource ? "folder" : "folder.badge.plus")
                                .foregroundStyle(MacLabelColor.accentGlyph)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(grant.displayName)
                                Text(grant.purpose == .backupSource ? "Backup source" : "Restore destination")
                                    .font(.caption)
                                    .secondaryLabelStyle()
                            }
                            Spacer()
                            Button("Remove", role: .destructive) { grantToRemove = grant }
                        }
                        .padding(.vertical, 9)
                        if grant.id != model.directoryGrants.last?.id { Divider() }
                    }
                }
            }
        }
    }

    private var connection: some View {
        settingsSection("Local service", systemImage: "point.3.connected.trianglepath.dotted") {
            VStack(alignment: .leading, spacing: 12) {
                LabeledContent("Address", value: model.currentConnectionAddress())
                LabeledContent("Status", value: model.phase == .ready ? "Ready" : "Attention required")
                HStack {
                    Button("Reconnect…") { model.presentation = .connection }
                    Button("Forget Connection", role: .destructive) {
                        Task { await model.disconnect() }
                    }
                }
                Text("Your access token is only ever sent over an encrypted connection. An unencrypted address is accepted only when the backup server runs on this Mac.")
                    .font(.caption)
                    .secondaryLabelStyle()
            }
        }
    }

    private var platformLimits: some View {
        settingsSection("Background work", systemImage: "clock.arrow.circlepath") {
            Text("Your backup server saves its progress and picks up interrupted work where it left off. Keep this Mac awake for long folder operations; Covalent does not run unrestricted in the background, only within what macOS allows.")
                .secondaryLabelStyle()
        }
    }

    private func settingsSection<Content: View>(
        _ title: String,
        systemImage: String,
        @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Label(title, systemImage: systemImage)
                .font(.title2.weight(.semibold))
            content()
                .padding(16)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 12))
        }
    }

    private var fieldsChanged: Bool {
        deviceName != model.settings?.deviceName || lanDiscoveryEnabled != model.settings?.lanDiscoveryEnabled
    }

    private func syncFields() {
        deviceName = model.settings?.deviceName ?? model.status?.deviceName ?? ""
        lanDiscoveryEnabled = model.settings?.lanDiscoveryEnabled ?? model.status?.lanDiscovery ?? false
    }

    private func exportSettings() {
        Task {
            guard let data = await model.exportSettingsData() else { return }
            let panel = NSSavePanel()
            panel.title = "Export Covalent Settings"
            panel.nameFieldStringValue = "Covalent Settings.json"
            panel.allowedContentTypes = [.json]
            panel.canCreateDirectories = true
            guard panel.runModal() == .OK, let url = panel.url else { return }
            do {
                try data.write(to: url, options: [.atomic])
            } catch {
                model.alert = AppAlert(
                    title: "Settings could not be saved",
                    message: ErrorPresenter.summary(for: error)
                )
            }
        }
    }

    private func exportRecoveryKit() {
        isExportingRecovery = true
        recoveryExportNotice = nil
        Task {
            defer { isExportingRecovery = false }
            guard let kitURL = chooseRecoveryDestination(
                title: "Save Encrypted Recovery Kit",
                name: "Covalent Recovery Kit.covalent-recovery",
                prompt: "Save Kit"
            ) else { return }
            guard let keyURL = chooseRecoveryDestination(
                title: "Save Separate Recovery Code",
                name: "Covalent Recovery Code.covalent-recovery-key",
                prompt: "Save Code"
            ) else { return }
            var export: RecoveryKitExport
            if let pending = recoveryExportSession {
                export = pending
                recoveryExportSession = nil
            } else {
                guard let fetched = await model.exportRecoveryKit() else { return }
                export = fetched
            }
            do {
                try RecoveryFilePair.write(export, kitURL: kitURL, keyURL: keyURL)
                recoveryExportNotice = "Both recovery files were saved. Keep them in different safe places."
                export.discard()
            } catch {
                // Retain this exact pair only for the next explicit save
                // attempt; never ask the server to generate a different kit.
                recoveryExportSession = export
                model.alert = AppAlert(
                    title: "Recovery files could not be saved",
                    message: ErrorPresenter.summary(for: error)
                )
            }
        }
    }

    private func chooseRecoveryDestination(title: String, name: String, prompt: String) -> URL? {
        let panel = NSSavePanel()
        panel.title = title
        panel.nameFieldStringValue = name
        panel.prompt = prompt
        panel.canCreateDirectories = true
        return panel.runModal() == .OK ? panel.url : nil
    }

    private func recoveryStatusCopy(_ status: RecoveryStatus) -> String {
        switch status.phase {
        case .notConfigured: "No device recovery is in progress."
        case .pending: "Recovery is waiting to check the selected backup devices."
        case .imported: "Recovered \(status.recoveredBackups.count) backup\(status.recoveredBackups.count == 1 ? "" : "s") and checked all your backup devices."
        case .partial: "Some backup devices have not confirmed recovery. Retry after they are reachable."
        case .blocked: "Covalent could not safely recover a backup yet. Bring your backup devices online and try again."
        case .noCatalogs: "All your backup devices were checked, but no backups were found."
        }
    }
}
