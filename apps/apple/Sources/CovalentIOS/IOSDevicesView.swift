import SwiftUI
import UIKit

struct IOSDevicesView: View {
    @ObservedObject var model: CovalentAppModel
    @State private var showingProviderConnection = false
    @State private var providerToRevoke: ProviderConnection?

    var body: some View {
        List {
            Section {
                if model.discoveryCandidates.isEmpty {
                    ContentUnavailableView {
                        Label("No candidates found", systemImage: "dot.radiowaves.left.and.right")
                    } description: {
                        Text("Discovery is only a hint. Manual signed pairing remains available.")
                    }
                    .listRowBackground(Color.clear)
                } else {
                    ForEach(model.discoveryCandidates) { candidate in
                        VStack(alignment: .leading, spacing: 5) {
                            HStack {
                                Label(candidate.source.label, systemImage: candidate.source == .tailscale ? "network.badge.shield.half.filled" : "dot.radiowaves.left.and.right")
                                Spacer()
                                Text(candidate.isCompatible ? "Protocol 1" : "Incompatible")
                                    .font(.caption)
                                    .foregroundStyle(candidate.isCompatible ? Color.secondary : Color.orange)
                            }
                            Text(candidate.endpoint)
                                .font(.caption.monospaced())
                            Text("Untrusted service \(candidate.serviceId)")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                        }
                        .padding(.vertical, 4)
                    }
                }
            } header: {
                Text("Nearby and Tailnet candidates")
            } footer: {
                Text("A candidate receives no access until both devices sign matching roles and the same comparison code.")
            }

            Section {
                if model.providers.isEmpty {
                    ContentUnavailableView {
                        Label("No connected providers", systemImage: "server.rack")
                    } description: {
                        Text("Backups stay local until you connect a confirmed provider and explicitly select it.")
                    } actions: {
                        Button("Connect Confirmed Provider") { showingProviderConnection = true }
                    }
                    .listRowBackground(Color.clear)
                } else {
                    ForEach(model.providers) { provider in
                        HStack(spacing: 12) {
                            Image(systemName: "server.rack")
                                .foregroundStyle(.blue)
                                .frame(width: 28)
                            VStack(alignment: .leading, spacing: 3) {
                                Text(provider.address).font(.headline)
                                Text("Certificate \(provider.certificateFingerprint.prefix(16))…")
                                    .font(.caption.monospaced())
                                    .foregroundStyle(.secondary)
                            }
                            Spacer()
                            Menu {
                                Button("Disconnect") { Task { await model.disconnectProvider(provider) } }
                                Button("Revoke Access…", role: .destructive) { providerToRevoke = provider }
                            } label: {
                                Label("Provider actions", systemImage: "ellipsis.circle")
                                    .labelStyle(.iconOnly)
                            }
                        }
                        .padding(.vertical, 5)
                    }
                    Button("Connect Confirmed Provider…") { showingProviderConnection = true }
                }
            } header: {
                Text("Connected storage providers")
            }

            Section("Trust model") {
                Label("Signed roles and matching code", systemImage: "checkmark.shield")
                Label("Pinned transport certificate", systemImage: "lock.square")
                Label("Provider selected separately per backup", systemImage: "checklist")
            }
        }
        .navigationTitle("Devices")
        .toolbar {
            ToolbarItemGroup(placement: .topBarTrailing) {
                Button {
                    Task { await model.refreshDiscovery() }
                } label: {
                    Label("Refresh Candidates", systemImage: "arrow.clockwise")
                }
                .disabled(!model.isAuthorized)

                Button {
                    model.presentation = .pairDevice
                } label: {
                    Label("Pair Device", systemImage: "plus")
                }
                .disabled(!model.isAuthorized)
                .accessibilityIdentifier("devices.pair")
            }
        }
        .sheet(isPresented: $showingProviderConnection) {
            IOSProviderConnectionView(model: model)
        }
        .confirmationDialog(
            "Revoke this device?",
            isPresented: Binding(
                get: { providerToRevoke != nil },
                set: { if !$0 { providerToRevoke = nil } }
            )
        ) {
            Button("Revoke Device", role: .destructive) {
                guard let providerToRevoke else { return }
                Task { await model.revokeProvider(providerToRevoke) }
                self.providerToRevoke = nil
            }
            Button("Cancel", role: .cancel) { providerToRevoke = nil }
        } message: {
            Text("Revocation records a durable tombstone and prevents future access. Existing replica bytes remain encrypted.")
        }
    }
}

private struct IOSProviderConnectionView: View {
    @ObservedObject var model: CovalentAppModel
    @Environment(\.dismiss) private var dismiss
    @State private var peerId = ""
    @State private var address = ""
    @State private var certificateDer = ""
    @State private var isConnecting = false

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Peer device ID", text: $peerId)
                        .textInputAutocapitalization(.never)
                    TextField("Numeric host and port", text: $address, prompt: Text("192.0.2.10:8787"))
                        .textInputAutocapitalization(.never)
                    TextField("Base64url certificate DER", text: $certificateDer, axis: .vertical)
                        .textInputAutocapitalization(.never)
                        .font(.caption.monospaced())
                        .lineLimit(4...10)
                } header: {
                    Text("Confirmed transport details")
                } footer: {
                    Text("Use the exact peer identity, numeric QUIC address, and certificate transferred through the mutually confirmed pairing channel.")
                }

                Section {
                    Label("The node validates the confirmed storage-provider role and pins the certificate fingerprint.", systemImage: "lock.shield")
                        .foregroundStyle(.secondary)
                }
            }
            .navigationTitle("Connect Provider")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Connect") { connect() }
                        .disabled(!canConnect || isConnecting)
                }
            }
        }
    }

    private var canConnect: Bool {
        UUID(uuidString: peerId.trimmingCharacters(in: .whitespacesAndNewlines)) != nil
            && !address.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !certificateDer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func connect() {
        guard let id = UUID(uuidString: peerId.trimmingCharacters(in: .whitespacesAndNewlines)) else { return }
        isConnecting = true
        Task {
            if await model.connectProvider(peerId: id, address: address, certificateDer: certificateDer) {
                dismiss()
            }
            isConnecting = false
        }
    }
}

struct IOSPairingView: View {
    private enum Mode: String, CaseIterable, Identifiable {
        case invite = "Invite"
        case join = "Join"
        var id: String { rawValue }
    }

    @ObservedObject var model: CovalentAppModel
    @Environment(\.dismiss) private var dismiss
    @State private var mode: Mode = .invite
    @State private var endpoint = ""
    @State private var invitationJSON = ""
    @State private var sessionJSON = ""
    @State private var responderRoles: Set<PeerRole> = [.storageProvider]
    @State private var inviterRoles: Set<PeerRole> = []
    @State private var comparedCode = false
    @State private var completionMessage: String?
    @State private var isWorking = false

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    if let completionMessage {
                        completion(completionMessage)
                    } else {
                        Picker("Pairing direction", selection: $mode) {
                            ForEach(Mode.allCases) { Text($0.rawValue).tag($0) }
                        }
                        .pickerStyle(.segmented)

                        Text("Transfer signed JSON with Share, then compare the code on both physical devices. Nearby advertisements alone remain untrusted.")
                            .font(.subheadline)
                            .foregroundStyle(.secondary)

                        if mode == .invite { inviteFlow } else { joinFlow }
                    }
                }
                .padding()
            }
            .navigationTitle("Secure Pairing")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
        }
        .task {
            if endpoint.isEmpty { endpoint = await model.defaultInvitationEndpoint() ?? "" }
        }
        .onChange(of: mode) { _, _ in
            invitationJSON = ""
            sessionJSON = ""
            comparedCode = false
        }
    }

    private var inviteFlow: some View {
        VStack(alignment: .leading, spacing: 18) {
            step(1, "Create a 10-minute invitation") {
                TextField("Reachable host and port", text: $endpoint, prompt: Text("192.0.2.10:8787"))
                    .textFieldStyle(.roundedBorder)
                    .textInputAutocapitalization(.never)
                Button("Create Invitation") { createInvitation() }
                    .buttonStyle(.borderedProminent)
                    .disabled(endpoint.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || isWorking)
                if !invitationJSON.isEmpty {
                    transferCard("Send this invitation", text: invitationJSON)
                }
            }

            step(2, "Paste the response and compare codes") {
                transferEditor(text: $sessionJSON, prompt: "Paste responder session JSON")
                if let session = try? model.pairingSession(from: sessionJSON) {
                    authenticationCode(session.authenticationString)
                    Toggle("I compared this code on both devices", isOn: $comparedCode)
                    Button(session.inviterConfirmationSignature == nil ? "Confirm This Device" : "This Device Confirmed") {
                        confirm(asInviter: true)
                    }
                    .disabled(!comparedCode || session.inviterConfirmationSignature != nil || isWorking)
                    if session.inviterConfirmationSignature != nil {
                        transferCard("Return this signed session", text: sessionJSON)
                    }
                }
            }

            step(3, "Finalize mutual trust") {
                Text("Paste the other device's returned, mutually signed session above.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Button("Finalize Pairing") { finalize(asInviter: true) }
                    .buttonStyle(.borderedProminent)
                    .disabled(!sessionIsMutuallySigned || isWorking)
            }
        }
    }

    private var joinFlow: some View {
        VStack(alignment: .leading, spacing: 18) {
            step(1, "Paste an invitation and choose exact roles") {
                transferEditor(text: $invitationJSON, prompt: "Paste invitation JSON")
                if let invitation = try? model.pairingInvitation(from: invitationJSON) {
                    Label("Invitation from \(invitation.inviterDeviceName.isEmpty ? "another device" : invitation.inviterDeviceName)", systemImage: "checkmark.seal")
                    DisclosureGroup("Permissions to grant") {
                        VStack(alignment: .leading, spacing: 10) {
                            Text("This device can…").font(.caption.weight(.semibold))
                            roleToggles(selection: $responderRoles)
                            Divider()
                            Text("Inviting device can…").font(.caption.weight(.semibold))
                            roleToggles(selection: $inviterRoles)
                        }
                        .padding(.top, 8)
                    }
                    Button("Accept Invitation") { acceptInvitation() }
                        .buttonStyle(.borderedProminent)
                        .disabled(isWorking)
                }
            }

            if let session = try? model.pairingSession(from: sessionJSON) {
                step(2, "Compare and sign the code") {
                    authenticationCode(session.authenticationString)
                    Toggle("I compared this code on both devices", isOn: $comparedCode)
                    Button(session.responderConfirmationSignature == nil ? "Confirm This Device" : "This Device Confirmed") {
                        confirm(asInviter: false)
                    }
                    .disabled(!comparedCode || session.responderConfirmationSignature != nil || isWorking)
                    if session.responderConfirmationSignature != nil {
                        transferCard("Send this session to the inviter", text: sessionJSON)
                        Text("Replace the text below with the inviter's mutually signed response.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        transferEditor(text: $sessionJSON, prompt: "Paste final signed session")
                    }
                }

                step(3, "Finalize mutual trust") {
                    Button("Finalize Pairing") { finalize(asInviter: false) }
                        .buttonStyle(.borderedProminent)
                        .disabled(!sessionIsMutuallySigned || isWorking)
                }
            }
        }
    }

    private func step<Content: View>(
        _ number: Int,
        _ title: String,
        @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: 11) {
            HStack(spacing: 9) {
                Text(number.formatted())
                    .font(.caption.weight(.bold))
                    .foregroundStyle(.white)
                    .frame(width: 25, height: 25)
                    .background(Color.blue, in: Circle())
                    .accessibilityHidden(true)
                Text(title).font(.headline)
            }
            content()
        }
        .padding(16)
        .background(Color(uiColor: .secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 16))
    }

    private func transferEditor(text: Binding<String>, prompt: String) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(prompt).font(.caption).foregroundStyle(.secondary)
            TextEditor(text: text)
                .font(.caption.monospaced())
                .frame(minHeight: 110)
                .padding(4)
                .background(Color(uiColor: .systemBackground), in: RoundedRectangle(cornerRadius: 9))
                .overlay { RoundedRectangle(cornerRadius: 9).stroke(Color.secondary.opacity(0.25)) }
                .textInputAutocapitalization(.never)
        }
    }

    private func transferCard(_ title: String, text: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.caption.weight(.semibold))
            Text("Signed protocol 1 transfer · \(text.utf8.count.formatted()) bytes")
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
            HStack {
                ShareLink(item: text) { Label("Share", systemImage: "square.and.arrow.up") }
                Button {
                    UIPasteboard.general.string = text
                } label: {
                    Label("Copy", systemImage: "doc.on.doc")
                }
            }
        }
        .padding(12)
        .background(Color.blue.opacity(0.08), in: RoundedRectangle(cornerRadius: 10))
    }

    private func authenticationCode(_ code: String) -> some View {
        VStack(spacing: 5) {
            Text("Comparison code").font(.caption).foregroundStyle(.secondary)
            Text(code)
                .font(.system(.title2, design: .monospaced, weight: .semibold))
                .textSelection(.enabled)
                .accessibilityLabel("Comparison code \(code.replacingOccurrences(of: "-", with: " "))")
        }
        .padding(12)
        .frame(maxWidth: .infinity)
        .background(Color.blue.opacity(0.08), in: RoundedRectangle(cornerRadius: 10))
    }

    private func roleToggles(selection: Binding<Set<PeerRole>>) -> some View {
        ForEach(PeerRole.allCases) { role in
            Toggle(role.label, isOn: Binding {
                selection.wrappedValue.contains(role)
            } set: { enabled in
                if enabled { selection.wrappedValue.insert(role) }
                else { selection.wrappedValue.remove(role) }
            })
        }
    }

    private func completion(_ message: String) -> some View {
        VStack(spacing: 16) {
            Image(systemName: "checkmark.circle.fill")
                .font(.system(size: 58))
                .foregroundStyle(.green)
                .accessibilityHidden(true)
            Text("Pairing Complete").font(.title.weight(.semibold))
            Text(message).foregroundStyle(.secondary).multilineTextAlignment(.center)
            Text("Connect the confirmed numeric address and pinned certificate separately, then choose this provider for each backup.")
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            Button("Done") { dismiss() }
                .buttonStyle(.borderedProminent)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 50)
    }

    private var sessionIsMutuallySigned: Bool {
        (try? model.pairingSession(from: sessionJSON).isMutuallySigned) == true
    }

    private func createInvitation() {
        isWorking = true
        Task {
            if let invitation = await model.createInvitation(endpoint: endpoint),
               let json = try? model.transferJSON(invitation) {
                invitationJSON = json
            }
            isWorking = false
        }
    }

    private func acceptInvitation() {
        isWorking = true
        Task {
            if let session = await model.acceptInvitation(
                json: invitationJSON,
                responderRoles: responderRoles,
                inviterRoles: inviterRoles
            ), let json = try? model.transferJSON(session) {
                sessionJSON = json
            }
            isWorking = false
        }
    }

    private func confirm(asInviter: Bool) {
        isWorking = true
        Task {
            if let confirmed = await model.confirmPairing(sessionJSON: sessionJSON, asInviter: asInviter),
               let json = try? model.transferJSON(confirmed) {
                sessionJSON = json
                comparedCode = false
            }
            isWorking = false
        }
    }

    private func finalize(asInviter: Bool) {
        isWorking = true
        Task {
            if let confirmation = await model.finalizePairing(sessionJSON: sessionJSON, asInviter: asInviter) {
                let peer = asInviter ? confirmation.inviterGrant : confirmation.responderGrant
                completionMessage = "\(peer.displayName) is trusted with exactly the roles both devices approved."
            }
            isWorking = false
        }
    }
}
