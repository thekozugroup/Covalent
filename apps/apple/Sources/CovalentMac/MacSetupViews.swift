import AppKit
import SwiftUI
import UniformTypeIdentifiers

struct MacConnectionView: View {
    @ObservedObject var model: CovalentAppModel
    @Environment(\.dismiss) private var dismiss
    @State private var serviceAddress = "http://127.0.0.1:8787"
    @State private var token = ""
    @State private var deviceName = ""
    @State private var lanDiscoveryEnabled = false
    @State private var trustedCertificateDER: Data?
    @State private var trustedCertificateName: String?
    @State private var revealToken = false
    @State private var isConnecting = false

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 16) {
                Image(systemName: "point.3.connected.trianglepath.dotted")
                    .font(.system(size: 30))
                    .foregroundStyle(.blue)
                    .frame(width: 54, height: 54)
                    .background(Color.blue.opacity(0.1), in: RoundedRectangle(cornerRadius: 13))
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 4) {
                    Text(model.phase == .ready ? "Service Connection" : "Connect Covalent")
                        .font(.title.weight(.semibold))
                    Text("The API token stays in this Mac's Keychain and is never included in settings exports.")
                        .foregroundStyle(.secondary)
                }
                Spacer()
            }
            .padding(24)

            Divider()

            Form {
                Section("Local node") {
                    TextField("Service address", text: $serviceAddress, prompt: Text("http://127.0.0.1:8787"))
                        .textContentType(.URL)
                        .accessibilityIdentifier("connection.address")
                    HStack {
                        if revealToken {
                            TextField("Local API token", text: $token)
                                .textContentType(.password)
                        } else {
                            SecureField("Local API token", text: $token)
                                .textContentType(.password)
                        }
                        Button(revealToken ? "Hide" : "Show") { revealToken.toggle() }
                            .buttonStyle(.plain)
                    }
                    .accessibilityIdentifier("connection.token")
                    HStack {
                        Text("Find it in the node data directory as local-api-token.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Spacer()
                        Button("Choose Token File…") { chooseTokenFile() }
                    }
                }
                Section("This device") {
                    TextField("Device name", text: $deviceName, prompt: Text("Home Mac"))
                        .accessibilityIdentifier("connection.deviceName")
                    Toggle("Find devices on the local network", isOn: $lanDiscoveryEnabled)
                    Text("Discovery advertises only a temporary service ID, protocol range, and port. It never advertises backup names or paths.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Section("HTTPS trust") {
                    HStack {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(trustedCertificateName ?? "System trust store")
                            Text("For the Docker or Unraid package, choose Caddy's exact root.crt. Hostname verification remains required.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        Spacer()
                        if trustedCertificateDER != nil {
                            Button("Clear") {
                                trustedCertificateDER = nil
                                trustedCertificateName = nil
                            }
                        }
                        Button("Choose Certificate…") { chooseCertificateFile() }
                    }
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
            .padding(.horizontal, 8)

            Divider()
            HStack {
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Spacer()
                if isConnecting {
                    ProgressView().controlSize(.small)
                }
                Button("Connect") {
                    isConnecting = true
                    Task {
                        if await model.connect(
                            serviceAddress: serviceAddress,
                            token: token,
                            trustedCertificateDER: trustedCertificateDER,
                            deviceName: deviceName,
                            lanDiscoveryEnabled: lanDiscoveryEnabled
                        ) {
                            dismiss()
                        }
                        isConnecting = false
                    }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(token.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || isConnecting)
                .accessibilityIdentifier("connection.connect")
            }
            .padding(18)
        }
        .frame(width: 620, height: 590)
        .onAppear {
            serviceAddress = model.currentConnectionAddress()
            deviceName = model.status?.deviceName ?? ""
            lanDiscoveryEnabled = model.status?.lanDiscovery ?? false
        }
    }

    private func chooseTokenFile() {
        let panel = NSOpenPanel()
        panel.title = "Choose Local API Token"
        panel.prompt = "Use Token"
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        do {
            token = try SecureNodeConnectionStore.parseTokenFile(Data(contentsOf: url))
        } catch {
            model.alert = AppAlert(
                title: "Token file is not valid",
                message: (error as? LocalizedError)?.errorDescription ?? String(describing: error)
            )
        }
    }

    private func chooseCertificateFile() {
        let panel = NSOpenPanel()
        panel.title = "Choose the Node TLS Certificate"
        panel.prompt = "Trust Exact Certificate"
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        do {
            trustedCertificateDER = try SecureNodeConnectionStore.parseCertificateFile(Data(contentsOf: url))
            trustedCertificateName = url.lastPathComponent
        } catch {
            model.alert = AppAlert(
                title: "Certificate is not valid",
                message: (error as? LocalizedError)?.errorDescription ?? String(describing: error)
            )
        }
    }
}

struct MacNewBackupView: View {
    @ObservedObject var model: CovalentAppModel
    @Environment(\.dismiss) private var dismiss
    @State private var displayName = ""
    @State private var sourceGrantId: UUID?
    @State private var existingBackupId: UUID?
    @State private var selectedProviderIds: Set<UUID> = []
    @State private var isCreating = false

    var body: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 5) {
                Text(existingBackupId == nil ? "New Backup" : "Add Snapshot")
                    .font(.title.weight(.semibold))
                Text("The local node encrypts and checkpoints this folder. Extra copies go only to devices you select below.")
                    .foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(24)

            Divider()

            Form {
                Section("What to protect") {
                    Picker("Backup", selection: $existingBackupId) {
                        Text("Create a new backup").tag(nil as UUID?)
                        ForEach(model.rememberedBackups) { backup in
                            Text("Add to \(backup.name)").tag(backup.backupId as UUID?)
                        }
                    }
                    TextField("Backup name", text: $displayName, prompt: Text("Family photos"))
                        .accessibilityIdentifier("backup.name")
                    Picker("Folder", selection: $sourceGrantId) {
                        Text("Choose a folder").tag(nil as UUID?)
                        ForEach(model.sourceGrants) { grant in
                            Text(grant.displayName).tag(grant.id as UUID?)
                        }
                    }
                    HStack {
                        Text("Folder access is stored as a security-scoped bookmark.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Spacer()
                        Button("Choose Folder…") { chooseFolder() }
                    }
                }

                Section("Extra copies") {
                    if model.providers.isEmpty {
                        Label("Local only", systemImage: "desktopcomputer")
                        Text("Pair and connect a storage provider to choose an extra copy. Local-only backups remain fully supported.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    } else {
                        ForEach(model.providers) { provider in
                            Toggle(isOn: providerSelection(provider.peerId)) {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(provider.address)
                                    Text(provider.peerId.uuidString)
                                        .font(.caption.monospaced())
                                        .foregroundStyle(.secondary)
                                        .lineLimit(1)
                                }
                            }
                        }
                        Text("This exact set is sent as replica intent. Covalent never substitutes another device.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)

            Divider()
            HStack {
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Spacer()
                if isCreating {
                    ProgressView().controlSize(.small)
                }
                Button(existingBackupId == nil ? "Create Backup" : "Add Snapshot") {
                    guard let sourceGrantId else { return }
                    isCreating = true
                    Task {
                        if await model.createBackup(
                            displayName: displayName,
                            existingBackupId: existingBackupId,
                            sourceGrantId: sourceGrantId,
                            selectedProviderIds: selectedProviderIds
                        ) != nil {
                            dismiss()
                        }
                        isCreating = false
                    }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(
                    displayName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                        || sourceGrantId == nil
                        || isCreating
                )
                .accessibilityIdentifier("backup.create")
            }
            .padding(18)
        }
        .frame(width: 650, height: 650)
        .onAppear { sourceGrantId = model.sourceGrants.first?.id }
        .onChange(of: existingBackupId) { _, backupId in
            guard let backupId,
                  let remembered = model.rememberedBackups.first(where: { $0.backupId == backupId })
            else { return }
            displayName = remembered.name
        }
    }

    private func providerSelection(_ id: UUID) -> Binding<Bool> {
        Binding {
            selectedProviderIds.contains(id)
        } set: { selected in
            if selected { selectedProviderIds.insert(id) } else { selectedProviderIds.remove(id) }
        }
    }

    private func chooseFolder() {
        let panel = NSOpenPanel()
        panel.title = "Choose Backup Folder"
        panel.prompt = "Authorize Folder"
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        Task {
            if let grant = await model.addDirectoryGrant(url: url, purpose: .backupSource) {
                sourceGrantId = grant.id
                if displayName.isEmpty { displayName = grant.displayName }
            }
        }
    }
}

struct MacSettingsImportView: View {
    @ObservedObject var model: CovalentAppModel
    @Environment(\.dismiss) private var dismiss
    @State private var data: Data?
    @State private var preview: ExportedDeviceSettings?
    @State private var isImporting = false

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            VStack(alignment: .leading, spacing: 5) {
                Text("Import Device Settings")
                    .font(.title.weight(.semibold))
                Text("This replaces the device name, discovery preference, and remembered backup descriptors after confirmation.")
                    .foregroundStyle(.secondary)
            }

            if let preview {
                GroupBox {
                    LabeledContent("Device name", value: preview.deviceName)
                    LabeledContent("LAN discovery", value: preview.lanDiscoveryEnabled ? "On" : "Off")
                    LabeledContent("Remembered backups", value: preview.rememberedBackups.count.formatted())
                }
                Label("Identity keys, backup keys, provider credentials, and folder grants are never imported by this workflow.", systemImage: "checkmark.shield")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            } else {
                ContentUnavailableView {
                    Label("Choose a settings file", systemImage: "doc.badge.arrow.up")
                } description: {
                    Text("Covalent validates the complete JSON contract before enabling import.")
                } actions: {
                    Button("Choose File…") { chooseFile() }
                }
                .frame(minHeight: 220)
            }

            Spacer()
            HStack {
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                if preview != nil {
                    Button("Choose Another…") { chooseFile() }
                }
                Spacer()
                Button("Replace Settings") {
                    guard let data else { return }
                    isImporting = true
                    Task {
                        if await model.importSettingsData(data) { dismiss() }
                        isImporting = false
                    }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(data == nil || isImporting)
            }
        }
        .padding(24)
        .frame(width: 560, height: 480)
    }

    private func chooseFile() {
        let panel = NSOpenPanel()
        panel.title = "Choose Covalent Settings"
        panel.allowedContentTypes = [.json]
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        do {
            let loaded = try Data(contentsOf: url, options: [.mappedIfSafe])
            guard loaded.count <= 2 * 1_024 * 1_024 else { throw AppModelError.settingsFileTooLarge }
            preview = try JSONDecoder().decode(ExportedDeviceSettings.self, from: loaded)
            data = loaded
        } catch {
            model.alert = AppAlert(
                title: "Settings file is not valid",
                message: (error as? LocalizedError)?.errorDescription ?? String(describing: error)
            )
        }
    }
}
