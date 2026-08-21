import SwiftUI
import UniformTypeIdentifiers

struct IOSSettingsView: View {
    @ObservedObject var model: CovalentAppModel
    @State private var deviceName = ""
    @State private var lanDiscoveryEnabled = false
    @State private var isSaving = false
    @State private var grantToRemove: SelectedDirectoryGrant?
    @State private var exportDocument: IOSSettingsDocument?
    @State private var showingExporter = false

    var body: some View {
        Form {
            Section {
                TextField("Device name", text: $deviceName)
                    .textContentType(.name)
                Toggle("Find devices on the local network", isOn: $lanDiscoveryEnabled)
                Button("Save Changes") { save() }
                    .disabled(!model.isAuthorized || isSaving || !fieldsChanged)
            } header: {
                Text("General")
            } footer: {
                Text("Disabling LAN discovery stops mDNS advertising and browsing. Manual addresses and bounded Tailscale candidates remain available.")
            }

            Section {
                Button {
                    exportSettings()
                } label: {
                    Label("Export Settings", systemImage: "square.and.arrow.up")
                }
                .disabled(!model.isAuthorized)

                Button {
                    model.presentation = .importSettings
                } label: {
                    Label("Import Settings", systemImage: "square.and.arrow.down")
                }
                .disabled(!model.isAuthorized)

                Label("Private identity keys, API tokens, backup keys, and folder permissions never leave this device.", systemImage: "lock.shield")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } header: {
                Text("Settings transfer")
            } footer: {
                Text("Transfers contain only the device name, discovery preference, and remembered backup descriptors.")
            }

            Section {
                if model.directoryGrants.isEmpty {
                    Text("No persistent folder permissions saved.")
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(model.directoryGrants) { grant in
                        HStack(spacing: 12) {
                            Image(systemName: grant.purpose == .backupSource ? "folder" : "folder.badge.plus")
                                .foregroundStyle(.blue)
                                .frame(width: 25)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(grant.displayName)
                                Text(grant.purpose == .backupSource ? "Backup source" : "Restore destination")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                            Spacer()
                            Button("Remove", role: .destructive) { grantToRemove = grant }
                                .font(.subheadline)
                        }
                    }
                }
            } header: {
                Text("Folder access")
            } footer: {
                Text("Removing access does not delete any encrypted snapshot. Future work with that folder asks you to choose it again.")
            }

            Section {
                LabeledContent("Address", value: model.currentConnectionAddress())
                LabeledContent("Status", value: model.serviceStatusLabel)
                Button("Reconnect…") { model.presentation = .connection }
                Button("Forget Connection", role: .destructive) {
                    Task { await model.disconnect() }
                }
            } header: {
                Text("Node service")
            } footer: {
                Text("Covalent refuses to send the bearer token to another device over plain HTTP.")
            }

            Section {
                Label("Tier 2 selected-folder support", systemImage: "iphone")
                Text("iOS grants only finite background time. The app requests that allowance for backup, verify, and restore work, while the node owns durable job checkpoints. Reopen Covalent if the system suspends it.")
                    .foregroundStyle(.secondary)
                Text("Covalent does not claim full-device backup or unrestricted continuous execution.")
                    .font(.caption.weight(.medium))
                    .foregroundStyle(.secondary)
            } header: {
                Text("Background work")
            }

            Section("About") {
                LabeledContent("Client support", value: PlatformTier.tier2.label)
                LabeledContent("Protocol", value: covalentProtocolVersion.formatted())
            }
        }
        .navigationTitle("Settings")
        .onAppear { syncFields() }
        .onChange(of: model.settings) { _, _ in syncFields() }
        .confirmationDialog(
            "Remove folder access?",
            isPresented: Binding(
                get: { grantToRemove != nil },
                set: { if !$0 { grantToRemove = nil } }
            )
        ) {
            Button("Remove Access", role: .destructive) {
                guard let grantToRemove else { return }
                Task { await model.removeDirectoryGrant(id: grantToRemove.id) }
                self.grantToRemove = nil
            }
            Button("Cancel", role: .cancel) { grantToRemove = nil }
        } message: {
            Text("Future backups or restores using this folder will require fresh authorization.")
        }
        .fileExporter(
            isPresented: $showingExporter,
            document: exportDocument,
            contentType: .json,
            defaultFilename: "Covalent Settings"
        ) { result in
            if case let .failure(error) = result {
                model.alert = AppAlert(
                    title: "Settings couldn't be saved",
                    message: ErrorPresenter.summary(for: error),
                    detail: ErrorPresenter.detail(for: error)
                )
            }
            exportDocument = nil
        }
    }

    private var fieldsChanged: Bool {
        deviceName != model.settings?.deviceName
            || lanDiscoveryEnabled != model.settings?.lanDiscoveryEnabled
    }

    private func syncFields() {
        deviceName = model.settings?.deviceName ?? model.status?.deviceName ?? ""
        lanDiscoveryEnabled = model.settings?.lanDiscoveryEnabled ?? model.status?.lanDiscovery ?? false
    }

    private func save() {
        isSaving = true
        Task {
            _ = await model.updateSettings(
                deviceName: deviceName,
                lanDiscoveryEnabled: lanDiscoveryEnabled
            )
            isSaving = false
        }
    }

    private func exportSettings() {
        Task {
            guard let data = await model.exportSettingsData() else { return }
            exportDocument = IOSSettingsDocument(data: data)
            showingExporter = true
        }
    }
}
