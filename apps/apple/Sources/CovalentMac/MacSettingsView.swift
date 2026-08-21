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
}
