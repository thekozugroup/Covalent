import AppKit
import SwiftUI

struct MacDevicesView: View {
    @ObservedObject var model: CovalentAppModel
    @State private var showingConnectProvider = false
    @State private var providerToRevoke: ProviderConnection?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 28) {
                header
                nearby
                providers
                trustExplanation
            }
            .frame(maxWidth: 860, alignment: .leading)
            .padding(32)
        }
        .background(Color(nsColor: .windowBackgroundColor))
        .navigationTitle("Devices")
        .sheet(isPresented: $showingConnectProvider) {
            MacProviderConnectionView(model: model)
        }
        .confirmationDialog(
            "Revoke this device?",
            isPresented: Binding(
                get: { providerToRevoke != nil },
                set: { if !$0 { providerToRevoke = nil } }
            ),
            titleVisibility: .visible
        ) {
            Button("Revoke Device", role: .destructive) {
                guard let providerToRevoke else { return }
                Task { await model.revokeProvider(providerToRevoke) }
                self.providerToRevoke = nil
            }
            Button("Cancel", role: .cancel) { providerToRevoke = nil }
        } message: {
            Text("Revocation creates a durable tombstone, disconnects the provider, and prevents future access. Existing replica data remains encrypted.")
        }
    }

    private var header: some View {
        HStack(alignment: .firstTextBaseline) {
            VStack(alignment: .leading, spacing: 7) {
                Text("Your backup network")
                    .font(.largeTitle.weight(.semibold))
                    .accessibilityAddTraits(.isHeader)
                Text("Discovery is only a hint. Trust starts after both devices compare the same code.")
                    .font(.title3)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button("Pair Device") { model.presentation = .pairDevice }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .disabled(!model.isAuthorized)
                .accessibilityIdentifier("devices.pair")
        }
    }

    private var nearby: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("Nearby and Tailnet candidates")
                    .font(.title2.weight(.semibold))
                Spacer()
                Button {
                    Task { await model.refreshDiscovery() }
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                .disabled(!model.isAuthorized)
            }
            if model.status?.lanDiscovery == false && model.discoveryCandidates.isEmpty {
                MacCallout(
                    title: "LAN discovery is off",
                    message: "Manual pairing and bounded Tailscale reachability remain available. Enable LAN discovery in Settings if you want nearby hints.",
                    systemImage: "network.slash",
                    tint: .secondary
                ) {
                    Button("Settings") { model.selectedSection = .settings }
                }
            } else if model.discoveryCandidates.isEmpty {
                ContentUnavailableView {
                    Label("No candidates found", systemImage: "dot.radiowaves.left.and.right")
                } description: {
                    Text("The device may still be reachable manually. Discovery never grants trust automatically.")
                }
                .frame(maxWidth: .infinity, minHeight: 180)
                .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 14))
            } else {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 260), spacing: 12)], spacing: 12) {
                    ForEach(model.discoveryCandidates) { candidate in
                        VStack(alignment: .leading, spacing: 10) {
                            HStack {
                                Image(systemName: candidate.source == .tailscale ? "network.badge.shield.half.filled" : "dot.radiowaves.left.and.right")
                                    .foregroundStyle(.blue)
                                Text(candidate.source.label).font(.headline)
                                Spacer()
                                Text(candidate.isCompatible ? "Protocol 1" : "Incompatible")
                                    .font(.caption)
                                    .foregroundStyle(candidate.isCompatible ? Color.secondary : Color.orange)
                            }
                            Text(candidate.endpoint)
                                .font(.subheadline.monospaced())
                                .textSelection(.enabled)
                            Text("Untrusted service \(candidate.serviceId)")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                            Button("Start Secure Pairing") { model.presentation = .pairDevice }
                                .disabled(!candidate.isCompatible)
                        }
                        .padding(15)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 12))
                    }
                }
            }
        }
    }

    private var providers: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("Connected storage providers")
                    .font(.title2.weight(.semibold))
                Spacer()
                Button("Connect Confirmed Provider…") { showingConnectProvider = true }
                    .disabled(!model.isAuthorized)
            }
            if model.providers.isEmpty {
                ContentUnavailableView {
                    Label("No connected providers", systemImage: "server.rack")
                } description: {
                    Text("Backups stay local until you pair, connect, and explicitly select a provider.")
                }
                .frame(maxWidth: .infinity, minHeight: 180)
                .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 14))
            } else {
                VStack(spacing: 0) {
                    ForEach(model.providers) { provider in
                        HStack(spacing: 14) {
                            Image(systemName: "server.rack")
                                .font(.title2)
                                .foregroundStyle(.blue)
                                .frame(width: 34)
                            VStack(alignment: .leading, spacing: 3) {
                                Text(provider.address).font(.headline)
                                Text("Certificate \(provider.certificateFingerprint.prefix(16))…")
                                    .font(.caption.monospaced())
                                    .foregroundStyle(.secondary)
                            }
                            Spacer()
                            Label("Connected", systemImage: "checkmark.circle.fill")
                                .font(.caption)
                                .foregroundStyle(.green)
                            Menu {
                                Button("Disconnect") {
                                    Task { await model.disconnectProvider(provider) }
                                }
                                Divider()
                                Button("Revoke Access…", role: .destructive) {
                                    providerToRevoke = provider
                                }
                            } label: {
                                Image(systemName: "ellipsis.circle")
                            }
                            .menuStyle(.borderlessButton)
                            .accessibilityLabel("Actions for \(provider.address)")
                        }
                        .padding(.vertical, 13)
                        .padding(.horizontal, 15)
                        if provider.id != model.providers.last?.id { Divider().padding(.leading, 62) }
                    }
                }
                .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 14))
            }
        }
    }

    private var trustExplanation: some View {
        MacCallout(
            title: "Pairing and transport are separate safeguards",
            message: "Both devices first sign matching roles and a comparison code. A provider connection then pins the certificate transferred through that confirmed channel.",
            systemImage: "checkmark.shield",
            tint: .blue
        ) {
            EmptyView()
        }
    }
}

struct MacProviderConnectionView: View {
    @ObservedObject var model: CovalentAppModel
    @Environment(\.dismiss) private var dismiss
    @State private var peerId = ""
    @State private var address = ""
    @State private var certificateDer = ""
    @State private var isConnecting = false

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            VStack(alignment: .leading, spacing: 5) {
                Text("Connect Confirmed Provider")
                    .font(.title.weight(.semibold))
                Text("Use the exact identity, numeric QUIC address, and certificate sent through the mutually confirmed pairing channel.")
                    .foregroundStyle(.secondary)
            }
            Form {
                TextField("Peer device ID", text: $peerId, prompt: Text("00000000-0000-0000-0000-000000000000"))
                TextField("Address", text: $address, prompt: Text("192.0.2.10:8787"))
                VStack(alignment: .leading) {
                    Text("Base64url certificate DER")
                    TextEditor(text: $certificateDer)
                        .font(.caption.monospaced())
                        .frame(height: 120)
                        .overlay { RoundedRectangle(cornerRadius: 6).stroke(Color.secondary.opacity(0.25)) }
                }
                Text("The node validates the peer's confirmed storage-provider role and persists the certificate fingerprint. Discovery alone is never enough.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .formStyle(.grouped)
            HStack {
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Spacer()
                Button("Connect") {
                    guard let id = UUID(uuidString: peerId.trimmingCharacters(in: .whitespacesAndNewlines)) else { return }
                    isConnecting = true
                    Task {
                        if await model.connectProvider(peerId: id, address: address, certificateDer: certificateDer) {
                            dismiss()
                        }
                        isConnecting = false
                    }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(
                    UUID(uuidString: peerId.trimmingCharacters(in: .whitespacesAndNewlines)) == nil
                        || address.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                        || certificateDer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                        || isConnecting
                )
            }
        }
        .padding(24)
        .frame(width: 620, height: 540)
    }
}

struct MacPairingView: View {
    enum Mode: String, CaseIterable, Identifiable {
        case invite = "Invite a device"
        case join = "Join an invitation"
        var id: String { rawValue }
    }

    @ObservedObject var model: CovalentAppModel
    @Environment(\.dismiss) private var dismiss
    @State private var mode: Mode = .invite
    @State private var endpoint = ""
    @State private var transferJSON = ""
    @State private var sessionJSON = ""
    @State private var responderRoles: Set<PeerRole> = [.storageProvider]
    @State private var inviterRoles: Set<PeerRole> = []
    @State private var comparedCode = false
    @State private var completedMessage: String?
    @State private var isWorking = false

    var body: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 5) {
                Text("Secure Pairing")
                    .font(.title.weight(.semibold))
                Text("Transfer the signed JSON through AirDrop, Messages, or another channel, then compare the code on both physical devices.")
                    .foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(24)
            Divider()

            if let completedMessage {
                completion(completedMessage)
            } else {
                ScrollView {
                    VStack(spacing: 18) {
                    Picker("Pairing direction", selection: $mode) {
                        ForEach(Mode.allCases) { mode in Text(mode.rawValue).tag(mode) }
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    if mode == .invite { inviteFlow } else { joinFlow }
                    }
                    .padding(24)
                }
                Divider()
                footer.padding(24)
            }
        }
        .frame(width: 620, height: 620)
        .task {
            if endpoint.isEmpty {
                endpoint = await model.defaultInvitationEndpoint() ?? ""
            }
        }
        .onChange(of: mode) { _, _ in
            transferJSON = ""
            sessionJSON = ""
            comparedCode = false
        }
    }

    private var inviteFlow: some View {
        VStack(alignment: .leading, spacing: 16) {
            pairingStep(1, "Create a 10-minute invitation") {
                HStack {
                    TextField("Reachable host and port", text: $endpoint, prompt: Text("192.0.2.10:8787"))
                        .textFieldStyle(.roundedBorder)
                    Button("Create") {
                        isWorking = true
                        Task {
                            if let invitation = await model.createInvitation(endpoint: endpoint),
                               let json = try? model.transferJSON(invitation) {
                                transferJSON = json
                            }
                            isWorking = false
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(endpoint.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || isWorking)
                }
                if !transferJSON.isEmpty {
                    transferBox(title: "Send this invitation to the other device", text: transferJSON)
                }
            }

            pairingStep(2, "Paste the signed response and compare codes") {
                transferEditor(text: $sessionJSON, prompt: "Paste the responder's session JSON")
                if let session = try? model.pairingSession(from: sessionJSON) {
                    pairingConsent(session)
                    authenticationCode(session.authenticationString)
                    Toggle("I compared this code on both physical devices", isOn: $comparedCode)
                    Button(session.inviterConfirmationSignature == nil ? "Confirm This Mac" : "This Mac Confirmed") {
                        isWorking = true
                        Task {
                            if let confirmed = await model.confirmPairing(sessionJSON: sessionJSON, asInviter: true),
                               let json = try? model.transferJSON(confirmed) {
                                sessionJSON = json
                                comparedCode = false
                            }
                            isWorking = false
                        }
                    }
                    .disabled(!comparedCode || session.inviterConfirmationSignature != nil || isWorking)
                    if session.inviterConfirmationSignature != nil {
                        transferBox(title: "Return this signed session, then paste the final response here", text: sessionJSON)
                    }
                }
            }

            pairingStep(3, "Finalize mutual trust") {
                Text("After the other device signs the same code, paste its returned session above.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Button("Finalize Pairing") { finalize(asInviter: true) }
                    .buttonStyle(.borderedProminent)
                    .disabled(!sessionIsMutuallySigned || isWorking)
            }
        }
    }

    private var joinFlow: some View {
        VStack(alignment: .leading, spacing: 16) {
            pairingStep(1, "Paste an invitation and choose exact roles") {
                transferEditor(text: $transferJSON, prompt: "Paste invitation JSON")
                if let invitation = try? model.pairingInvitation(from: transferJSON) {
                    Label("Invitation from \(invitation.inviterDeviceName)", systemImage: "checkmark.seal")
                        .font(.subheadline)
                    DisclosureGroup("Permissions to grant") {
                        VStack(alignment: .leading, spacing: 8) {
                            Text("This device can…").font(.caption.weight(.semibold))
                            roleToggles(selection: $responderRoles)
                            Divider()
                            Text("Inviting device can…").font(.caption.weight(.semibold))
                            roleToggles(selection: $inviterRoles)
                        }
                        .padding(.top, 8)
                    }
                    Button("Accept Invitation") {
                        isWorking = true
                        Task {
                            if let session = await model.acceptInvitation(
                                json: transferJSON,
                                responderRoles: responderRoles,
                                inviterRoles: inviterRoles
                            ), let json = try? model.transferJSON(session) {
                                sessionJSON = json
                            }
                            isWorking = false
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(isWorking)
                }
            }

            if let session = try? model.pairingSession(from: sessionJSON) {
                pairingStep(2, "Compare and sign the code") {
                    pairingConsent(session)
                    authenticationCode(session.authenticationString)
                    Toggle("I compared this code on both physical devices", isOn: $comparedCode)
                    Button(session.responderConfirmationSignature == nil ? "Confirm This Mac" : "This Mac Confirmed") {
                        isWorking = true
                        Task {
                            if let confirmed = await model.confirmPairing(sessionJSON: sessionJSON, asInviter: false),
                               let json = try? model.transferJSON(confirmed) {
                                sessionJSON = json
                                comparedCode = false
                            }
                            isWorking = false
                        }
                    }
                    .disabled(!comparedCode || session.responderConfirmationSignature != nil || isWorking)
                    if session.responderConfirmationSignature != nil {
                        transferBox(title: "Send this signed session to the inviter", text: sessionJSON)
                        Text("Paste the inviter's returned, mutually signed session into the box below.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        transferEditor(text: $sessionJSON, prompt: "Paste final signed session")
                    }
                }
                pairingStep(3, "Finalize mutual trust") {
                    Button("Finalize Pairing") { finalize(asInviter: false) }
                        .buttonStyle(.borderedProminent)
                        .disabled(!sessionIsMutuallySigned || isWorking)
                }
            }
        }
    }

    private func pairingStep<Content: View>(
        _ number: Int,
        _ title: String,
        @ViewBuilder content: () -> Content
    ) -> some View {
        HStack(alignment: .top, spacing: 12) {
            Text(number.formatted())
                .font(.caption.weight(.bold))
                .foregroundStyle(.white)
                .frame(width: 24, height: 24)
                .background(Color.blue, in: Circle())
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 10) {
                Text(title).font(.headline)
                content()
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func transferBox(title: String, text: String) -> some View {
        VStack(alignment: .leading, spacing: 7) {
            Text(title).font(.caption.weight(.semibold))
            HStack {
                Text("Signed protocol 1 transfer · \(text.utf8.count.formatted()) bytes")
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                Spacer()
                ShareLink(item: text) { Label("Share", systemImage: "square.and.arrow.up") }
                Button {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(text, forType: .string)
                } label: {
                    Label("Copy", systemImage: "doc.on.doc")
                }
            }
            .padding(10)
            .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 8))
        }
    }

    private func transferEditor(text: Binding<String>, prompt: String) -> some View {
        ZStack(alignment: .topLeading) {
            TextEditor(text: text)
                .font(.caption.monospaced())
                .frame(height: 72)
                .overlay { RoundedRectangle(cornerRadius: 6).stroke(Color.secondary.opacity(0.25)) }
            if text.wrappedValue.isEmpty {
                Text(prompt)
                    .font(.caption)
                    .foregroundStyle(.tertiary)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 8)
                    .allowsHitTesting(false)
            }
        }
    }

    private func authenticationCode(_ code: String) -> some View {
        VStack(spacing: 5) {
            Text("Comparison code")
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(code)
                .font(.system(.title2, design: .monospaced, weight: .semibold))
                .textSelection(.enabled)
                .accessibilityLabel("Comparison code \(code.replacingOccurrences(of: "-", with: " "))")
        }
        .padding(12)
        .frame(maxWidth: .infinity)
        .background(Color.blue.opacity(0.08), in: RoundedRectangle(cornerRadius: 10))
    }

    private func pairingConsent(_ session: PairingSession) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Exact signed consent").font(.caption.weight(.semibold))
            Text("Inviter: \(session.invitation.inviterDeviceName.isEmpty ? "Unnamed device" : session.invitation.inviterDeviceName) · \(session.invitation.inviterDeviceId.uuidString)")
            Text("Inviter receives: \(roleSummary(session.inviterRoles))")
            Divider()
            Text("Responder: \(session.responderName) · \(session.responderDeviceId.uuidString)")
            Text("Responder receives: \(roleSummary(session.responderRoles))")
        }
        .font(.caption)
        .textSelection(.enabled)
        .padding(10)
        .background(Color.orange.opacity(0.08), in: RoundedRectangle(cornerRadius: 8))
        .accessibilityElement(children: .combine)
    }

    private func roleSummary(_ roles: Set<PeerRole>) -> String {
        let labels = roles.sorted { $0.rawValue < $1.rawValue }.map(\.label)
        return labels.isEmpty ? "No access" : labels.joined(separator: ", ")
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

    private var footer: some View {
        HStack {
            Label("Nearby advertisements remain untrusted until finalization.", systemImage: "lock.shield")
                .font(.caption)
                .foregroundStyle(.secondary)
            Spacer()
            Button("Close") {
                // Keep the item-backed sheet binding authoritative. Relying on
                // environment dismissal alone can leave the pairing sheet in
                // place while a subsequent navigation action is dispatched.
                model.presentation = nil
                dismiss()
            }
            .keyboardShortcut(.cancelAction)
            .accessibilityIdentifier("pairing.close")
        }
    }

    private func completion(_ message: String) -> some View {
        VStack(spacing: 18) {
            Spacer()
            Image(systemName: "checkmark.circle.fill")
                .font(.system(size: 54))
                .foregroundStyle(.green)
            Text("Pairing Complete").font(.title.weight(.semibold))
            Text(message)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 480)
            Text("To use this device as a replica, connect its numeric address and pinned certificate from the confirmed channel. You still choose it separately for each backup.")
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 520)
            Button("Done") { dismiss() }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
            Spacer()
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(24)
    }

    private var sessionIsMutuallySigned: Bool {
        (try? model.pairingSession(from: sessionJSON).isMutuallySigned) == true
    }

    private func finalize(asInviter: Bool) {
        isWorking = true
        Task {
            if let confirmation = await model.finalizePairing(sessionJSON: sessionJSON, asInviter: asInviter) {
                let peer = asInviter ? confirmation.inviterGrant : confirmation.responderGrant
                completedMessage = "\(peer.displayName) is trusted with exactly the roles both devices approved."
            }
            isWorking = false
        }
    }
}
