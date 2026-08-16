import Foundation
import SwiftUI
import UniformTypeIdentifiers

struct IOSConnectionView: View {
    @ObservedObject var model: CovalentAppModel
    @Environment(\.dismiss) private var dismiss
    @State private var serviceAddress = "http://127.0.0.1:8787"
    @State private var token = ""
    @State private var deviceName = ""
    @State private var lanDiscoveryEnabled = false
    @State private var trustedCertificateDER: Data?
    @State private var trustedCertificateName: String?
    @State private var isChoosingCertificate = false
    @State private var isConnecting = false

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Service address", text: $serviceAddress, prompt: Text("https://node.example:8787"))
                        .textInputAutocapitalization(.never)
                        .keyboardType(.URL)
                        .textContentType(.URL)
                        .accessibilityIdentifier("connection.address")
                    SecureField("Local API token", text: $token)
                        .textInputAutocapitalization(.never)
                        .textContentType(.password)
                        .accessibilityIdentifier("connection.token")
                } header: {
                    Text("Node connection")
                } footer: {
                    Text("Loopback HTTP is allowed for a node on this device or Simulator. Connections to another device must use HTTPS before Covalent sends the bearer token.")
                }

                Section("This device") {
                    TextField("Device name", text: $deviceName, prompt: Text("My iPhone"))
                        .textContentType(.name)
                    Toggle("Find devices on the local network", isOn: $lanDiscoveryEnabled)
                }

                Section {
                    LabeledContent("Trust", value: trustedCertificateName ?? "System certificates")
                    Button("Choose exact CA certificate…") { isChoosingCertificate = true }
                    if trustedCertificateDER != nil {
                        Button("Use system certificates", role: .destructive) {
                            trustedCertificateDER = nil
                            trustedCertificateName = nil
                        }
                    }
                } header: {
                    Text("HTTPS certificate")
                } footer: {
                    Text("Docker and Unraid use Caddy's root.crt. Covalent trusts only the selected certificate and still verifies the server hostname.")
                }

                Section {
                    Label("The token is stored in Keychain and excluded from settings exports.", systemImage: "lock.shield")
                        .foregroundStyle(.secondary)
                }
            }
            .navigationTitle(model.phase == .ready ? "Service Connection" : "Connect Covalent")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
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
                    .disabled(token.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || isConnecting)
                    .accessibilityIdentifier("connection.connect")
                }
            }
            .overlay {
                if isConnecting { ProgressView("Authenticating…").padding().background(.background, in: RoundedRectangle(cornerRadius: 12)) }
            }
        }
        .onAppear {
            serviceAddress = model.currentConnectionAddress()
            deviceName = model.status?.deviceName ?? ""
            lanDiscoveryEnabled = model.status?.lanDiscovery ?? false
        }
        .fileImporter(
            isPresented: $isChoosingCertificate,
            allowedContentTypes: [.data],
            allowsMultipleSelection: false
        ) { result in
            guard case let .success(urls) = result, let url = urls.first else {
                if case let .failure(error) = result {
                    model.alert = AppAlert(title: "Certificate could not be opened", message: error.localizedDescription)
                }
                return
            }
            let accessed = url.startAccessingSecurityScopedResource()
            defer { if accessed { url.stopAccessingSecurityScopedResource() } }
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
}

struct IOSNewBackupView: View {
    @ObservedObject var model: CovalentAppModel
    @Environment(\.dismiss) private var dismiss
    @State private var displayName = ""
    @State private var sourceGrantId: UUID?
    @State private var existingBackupId: UUID?
    @State private var selectedProviderIds: Set<UUID> = []
    @State private var isChoosingFolder = false
    @State private var isCreating = false

    var body: some View {
        NavigationStack {
            Form {
                Section {
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
                    Button("Choose Folder…") { isChoosingFolder = true }
                } header: {
                    Text("What to protect")
                } footer: {
                    Text("Covalent stores a persistent security-scoped bookmark only for the folder you choose.")
                }

                Section {
                    if model.providers.isEmpty {
                        Label("Local only", systemImage: "iphone")
                        Text("Pair and connect a storage provider before selecting an extra copy.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    } else {
                        ForEach(model.providers) { provider in
                            Toggle(isOn: providerSelection(provider.peerId)) {
                                VStack(alignment: .leading, spacing: 3) {
                                    Text(provider.address)
                                    Text(provider.certificateFingerprint)
                                        .font(.caption.monospaced())
                                        .foregroundStyle(.secondary)
                                        .lineLimit(1)
                                }
                            }
                        }
                    }
                } header: {
                    Text("Exact extra copies")
                } footer: {
                    Text("Only this exact set is sent to the node. Covalent never substitutes another provider.")
                }
            }
            .navigationTitle(existingBackupId == nil ? "New Backup" : "Add Snapshot")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(existingBackupId == nil ? "Create" : "Add") { createBackup() }
                        .disabled(!canCreate)
                        .accessibilityIdentifier("backup.create")
                }
            }
            .overlay {
                if isCreating { ProgressView("Creating encrypted snapshot…").padding().background(.background, in: RoundedRectangle(cornerRadius: 12)) }
            }
            .fileImporter(
                isPresented: $isChoosingFolder,
                allowedContentTypes: [.folder],
                allowsMultipleSelection: false
            ) { result in
                guard case let .success(urls) = result, let url = urls.first else {
                    if case let .failure(error) = result { report(error, title: "Folder could not be opened") }
                    return
                }
                Task {
                    if let grant = await model.addDirectoryGrant(url: url, purpose: .backupSource) {
                        sourceGrantId = grant.id
                        if displayName.isEmpty { displayName = grant.displayName }
                    }
                }
            }
        }
        .onAppear { sourceGrantId = model.sourceGrants.first?.id }
        .onChange(of: existingBackupId) { _, backupId in
            guard let backupId,
                  let remembered = model.rememberedBackups.first(where: { $0.backupId == backupId })
            else { return }
            displayName = remembered.name
        }
    }

    private var canCreate: Bool {
        !displayName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && sourceGrantId != nil
            && !isCreating
    }

    private func providerSelection(_ id: UUID) -> Binding<Bool> {
        Binding {
            selectedProviderIds.contains(id)
        } set: { enabled in
            if enabled { selectedProviderIds.insert(id) }
            else { selectedProviderIds.remove(id) }
        }
    }

    private func createBackup() {
        guard let sourceGrantId else { return }
        isCreating = true
        Task {
            let record = await IOSBackgroundExecution.perform(
                named: "Covalent backup",
                onExpiration: { await model.pauseActiveTaskForBackgroundExpiration() }
            ) {
                await model.createBackup(
                    displayName: displayName,
                    existingBackupId: existingBackupId,
                    sourceGrantId: sourceGrantId,
                    selectedProviderIds: selectedProviderIds
                )
            }
            if record != nil { dismiss() }
            isCreating = false
        }
    }

    private func report(_ error: Error, title: String) {
        model.alert = AppAlert(
            title: title,
            message: (error as? LocalizedError)?.errorDescription ?? String(describing: error)
        )
    }
}

struct IOSSettingsImportView: View {
    @ObservedObject var model: CovalentAppModel
    @Environment(\.dismiss) private var dismiss
    @State private var isChoosingFile = false
    @State private var data: Data?
    @State private var preview: ExportedDeviceSettings?
    @State private var isImporting = false

    var body: some View {
        NavigationStack {
            List {
                if let preview {
                    Section("Validated settings") {
                        LabeledContent("Device", value: preview.deviceName)
                        LabeledContent("LAN discovery", value: preview.lanDiscoveryEnabled ? "On" : "Off")
                        LabeledContent("Remembered backups", value: preview.rememberedBackups.count.formatted())
                    }
                    Section {
                        Label("Identity keys, backup keys, API tokens, provider credentials, and folder grants are not imported.", systemImage: "checkmark.shield")
                            .foregroundStyle(.secondary)
                    }
                } else {
                    ContentUnavailableView {
                        Label("Choose settings JSON", systemImage: "doc.badge.arrow.up")
                    } description: {
                        Text("Covalent validates the complete versioned contract before enabling replacement.")
                    } actions: {
                        Button("Choose File") { isChoosingFile = true }
                            .buttonStyle(.borderedProminent)
                    }
                    .listRowBackground(Color.clear)
                }
            }
            .navigationTitle("Import Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                if preview != nil {
                    ToolbarItem(placement: .topBarLeading) {
                        Button("Choose Another") { isChoosingFile = true }
                    }
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Replace") { importSettings() }
                            .disabled(data == nil || isImporting)
                    }
                }
            }
            .fileImporter(isPresented: $isChoosingFile, allowedContentTypes: [.json]) { result in
                do {
                    let url = try result.get()
                    let importedData = try IOSImportedFile.read(url)
                    let decoded = try JSONDecoder().decode(ExportedDeviceSettings.self, from: importedData)
                    guard decoded.schemaVersion == 1 else { throw AppModelError.unsupportedSettings }
                    data = importedData
                    preview = decoded
                } catch {
                    report(error)
                }
            }
        }
    }

    private func importSettings() {
        guard let data else { return }
        isImporting = true
        Task {
            if await model.importSettingsData(data) { dismiss() }
            isImporting = false
        }
    }

    private func report(_ error: Error) {
        model.alert = AppAlert(
            title: "Settings file is not valid",
            message: (error as? LocalizedError)?.errorDescription ?? String(describing: error)
        )
    }
}

struct IOSSettingsDocument: FileDocument {
    static var readableContentTypes: [UTType] { [.json] }
    var data: Data

    init(data: Data) {
        self.data = data
    }

    init(configuration: ReadConfiguration) throws {
        guard let data = configuration.file.regularFileContents else {
            throw CocoaError(.fileReadCorruptFile)
        }
        self.data = data
    }

    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper {
        FileWrapper(regularFileWithContents: data)
    }
}

private enum IOSImportedFile {
    static func read(_ url: URL) throws -> Data {
        let didStart = url.startAccessingSecurityScopedResource()
        defer {
            if didStart { url.stopAccessingSecurityScopedResource() }
        }
        var coordinationError: NSError?
        var result: Result<Data, Error>?
        NSFileCoordinator().coordinate(
            readingItemAt: url,
            options: .withoutChanges,
            error: &coordinationError
        ) { coordinatedURL in
            result = Result { try Data(contentsOf: coordinatedURL, options: .mappedIfSafe) }
        }
        if let coordinationError { throw coordinationError }
        guard let result else { throw SelectedDirectoryError.coordinationFailed }
        return try result.get()
    }
}
