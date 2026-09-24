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
                    .scaledSymbolFont(size: 30)
                    .foregroundStyle(MacLabelColor.accentGlyph)
                    .scaledSymbolFrame(54)
                    .background(Color.blue.opacity(0.1), in: RoundedRectangle(cornerRadius: 13))
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 4) {
                    Text(model.phase == .ready ? "Service Connection" : "Connect Covalent")
                        .font(.title.weight(.semibold))
                    Text("The API token stays in this Mac's Keychain and is never included in settings exports.")
                        .secondaryLabelStyle()
                }
                Spacer()
            }
            .padding(24)

            Divider()

            Form {
                Section("Backup server") {
                    TextField("Service address", text: $serviceAddress, prompt: Text("http://127.0.0.1:8787"))
                        .textContentType(.URL)
                        .accessibilityIdentifier("connection.address")
                    HStack {
                        if revealToken {
                            TextField("Server access token", text: $token)
                                .textContentType(.password)
                        } else {
                            SecureField("Server access token", text: $token)
                                .textContentType(.password)
                        }
                        Button(revealToken ? "Hide" : "Show") { revealToken.toggle() }
                            .buttonStyle(.plain)
                    }
                    .accessibilityIdentifier("connection.token")
                    HStack {
                Text("Choose the access-token file created by the Covalent claim command on your trusted computer, or paste its exact contents.")
                            .font(.caption)
                            .secondaryLabelStyle()
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
                        .secondaryLabelStyle()
                }
                Section("HTTPS trust") {
                    HStack {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(trustedCertificateName ?? "System trust store")
                            Text("For the Docker or Unraid package, choose Caddy's exact root.crt. Hostname verification remains required.")
                                .font(.caption)
                                .secondaryLabelStyle()
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
        panel.title = "Choose Server Access Token"
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
                message: ErrorPresenter.summary(for: error)
            )
        }
    }

    private func chooseCertificateFile() {
        let panel = NSOpenPanel()
        panel.title = "Choose Security Certificate"
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
                message: ErrorPresenter.summary(for: error)
            )
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
                Text("This replaces the device name, discovery preference, and remembered backup list after confirmation.")
                    .secondaryLabelStyle()
            }

            if let preview {
                GroupBox {
                    LabeledContent("Device name", value: preview.deviceName)
                    LabeledContent("LAN discovery", value: preview.lanDiscoveryEnabled ? "On" : "Off")
                    LabeledContent("Remembered backups", value: preview.rememberedBackups.count.formatted())
                }
                Label("Identity keys, backup keys, storage device credentials, and folder permissions are never imported.", systemImage: "checkmark.shield")
                    .font(.subheadline)
                    .secondaryLabelStyle()
            } else {
                MacEmptyState(
                    systemImage: "doc.badge.arrow.up",
                    title: "Choose a settings file",
                    message: "Covalent checks the whole file before allowing the import."
                ) {
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
                message: ErrorPresenter.summary(for: error)
            )
        }
    }
}
