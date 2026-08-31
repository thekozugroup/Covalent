import AppKit
import SwiftUI

struct MacDevicesView: View {
    @ObservedObject var model: CovalentAppModel
    @State private var showingConnectProvider = false
    @State private var providerToRevoke: ProviderConnection?
    @State private var tailscaleAddress = ""
    @State private var isAdvancedRecoveryExpanded = false

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
            Text("Covalent permanently records that this device is no longer trusted, disconnects it, and blocks any future access. Copies already stored there stay encrypted.")
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
                    .secondaryLabelStyle()
            }
            Spacer()
            Button("Find Devices") { Task { await model.refreshDiscovery() } }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .disabled(!model.isAuthorized)
                .accessibilityIdentifier("devices.pair")
        }
    }

    private var nearby: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("Nearby devices")
                    .font(.title2.weight(.semibold))
                Spacer()
            }
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                TextField("Tailscale hostname or IP", text: $tailscaleAddress, prompt: Text("nas.tailnet-name.ts.net:8787"))
                    .textFieldStyle(.roundedBorder)
                    .onSubmit { startTailscalePairing() }
                    .accessibilityHint("Enter the address shown by the other device in Tailscale")
                Button("Use as Backup Device") { startTailscalePairing() }
                    .disabled(
                        tailscaleAddress.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ||
                        !model.isAuthorized || model.startingPairingAddress != nil
                    )
            }
            Text(
                model.status?.lanDiscovery == false
                    ? "Automatic LAN discovery is off. Enter a one-time Tailscale MagicDNS hostname or IP; both devices still confirm the same code."
                    : "LAN devices appear automatically. Tailscale does not provide local device enumeration, so enter the other device's MagicDNS hostname or IP once."
            )
            .font(.caption)
            .secondaryLabelStyle()
            if model.startingPairingAddress != nil {
                ProgressView("Contacting Tailscale device…")
                    .controlSize(.small)
            }
            if model.status?.lanDiscovery == false && model.discoveryCandidates.isEmpty {
                MacCallout(
                    title: "LAN discovery is off",
                    message: "Enter a Tailscale address above, or enable LAN discovery in Settings for automatic nearby-device hints.",
                    systemImage: "network.slash",
                    tint: .secondary
                ) {
                    Button("Settings") { model.selectedSection = .settings }
                }
            } else if model.discoveryCandidates.isEmpty {
                MacEmptyState(
                    systemImage: "dot.radiowaves.left.and.right",
                    title: "No candidates found",
                    message: "No LAN devices replied. You can enter a Tailscale address above; "
                        + "discovery never grants trust automatically."
                )
                .frame(maxWidth: .infinity, minHeight: 180)
                .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 14))
            } else {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 260), spacing: 12)], spacing: 12) {
                    ForEach(model.discoveryCandidates) { candidate in
                        VStack(alignment: .leading, spacing: 10) {
                            HStack {
                                Image(systemName: candidate.source == .tailscale ? "network.badge.shield.half.filled" : "dot.radiowaves.left.and.right")
                                    .foregroundStyle(MacLabelColor.accentGlyph)
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
                                .secondaryLabelStyle()
                                .lineLimit(1)
                            Button("Pair with This Device") {
                                Task { await model.startNetworkPairing(candidate: candidate) }
                            }
                                .disabled(!candidate.isCompatible)
                                .accessibilityHint("Starts a direct pairing request; both devices must confirm the same code")
                            if model.startingPairingCandidateID == candidate.id {
                                ProgressView("Contacting device…")
                                    .controlSize(.small)
                            }
                        }
                        .padding(15)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 12))
                    }
                }
            }
        }
    }

    private func startTailscalePairing() {
        let address = tailscaleAddress.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !address.isEmpty else { return }
        Task { await model.startNetworkPairing(candidateAddress: address) }
    }

    private var providers: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("Connected storage devices")
                    .font(.title2.weight(.semibold))
            }
            if model.providers.isEmpty {
                MacEmptyState(
                    systemImage: "server.rack",
                    title: "No connected storage devices",
                    message: "Backups stay on this Mac until you pair a device, connect it, and select it yourself."
                )
                .frame(maxWidth: .infinity, minHeight: 180)
                .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 14))
            } else {
                VStack(spacing: 0) {
                    ForEach(model.providers) { provider in
                        HStack(spacing: 14) {
                            Image(systemName: "server.rack")
                                .font(.title2)
                                .foregroundStyle(MacLabelColor.accentGlyph)
                                .frame(width: 34)
                            VStack(alignment: .leading, spacing: 3) {
                                Text(provider.address).font(.headline)
                                Text("Certificate \(provider.certificateFingerprint.prefix(16))…")
                                    .font(.caption.monospaced())
                                    .secondaryLabelStyle()
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
            // The expansion is bound to explicit state, and the whole label row
            // toggles it. Left to itself, SwiftUI gives this group an
            // accessibility element of role `DisclosureTriangle` whose frame
            // covers only the label text — the triangle glyph that actually
            // toggles the group sits outside those bounds. The control then
            // advertises a disclosure with an expanded/collapsed value while
            // offering no activation point anywhere inside itself, so a click
            // on the words "Advanced recovery" does nothing and the only way
            // in is a glyph a few points wide.
            DisclosureGroup(isExpanded: $isAdvancedRecoveryExpanded) {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Recovery only: exchange signed setup files by hand when direct pairing over the network cannot be used, or when reconnecting an older backup server.")
                        .font(.caption)
                        .secondaryLabelStyle()
                    Button("Offline Pairing with Signed Files…") { model.requestManualPairing() }
                        .accessibilityIdentifier("devices.offlinePairing")
                    Button("Import Signed Connection File…") { showingConnectProvider = true }
                        .disabled(!model.isAuthorized)
                }
                .padding(.top, 6)
            } label: {
                Text("Advanced recovery")
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
                    .onTapGesture { isAdvancedRecoveryExpanded.toggle() }
                    .accessibilityIdentifier("devices.advancedRecovery")
            }
        }
    }

    private var trustExplanation: some View {
        MacCallout(
            title: "Pairing and transport are separate safeguards",
            message: "Both devices first sign the same permissions and the same comparison code. The storage connection then locks onto the exact certificate that was exchanged through that confirmed channel.",
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
    @State private var signedTransportJSON = ""
    @State private var isConnecting = false

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            VStack(alignment: .leading, spacing: 5) {
                Text("Import Signed Connection File")
                    .font(.title.weight(.semibold))
                Text("Paste the complete connection details from a pairing both devices finished. Partial details are never accepted.")
                    .secondaryLabelStyle()
            }
            Form {
                VStack(alignment: .leading) {
                    Text("Signed connection details")
                    TextEditor(text: $signedTransportJSON)
                        .font(.caption.monospaced())
                        .frame(height: 220)
                        .overlay { RoundedRectangle(cornerRadius: 6).stroke(Color.secondary.opacity(0.25)) }
                }
                Text("Your backup server checks the other device's identity, name, address, certificate, and certificate fingerprint against what both devices signed during pairing. Every one must match exactly.")
                    .font(.caption)
                    .secondaryLabelStyle()
            }
            .formStyle(.grouped)
            HStack {
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Spacer()
                Button("Connect") {
                    isConnecting = true
                    Task {
                        if await model.connectProvider(signedTransportJSON: signedTransportJSON) {
                            dismiss()
                        }
                        isConnecting = false
                    }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(
                    signedTransportJSON.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
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
    @State private var transferJSON = ""
    @State private var sessionJSON = ""
    @State private var responderRoles: Set<PeerRole> = [.storageProvider]
    @State private var inviterRoles: Set<PeerRole> = []
    @State private var comparedCode = false
    @State private var completedConfirmation: PairingConfirmation?
    @State private var isWorking = false
    @State private var isAddingBackupDevice = false

    var body: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 5) {
                Text("Secure Pairing")
                    .font(.title.weight(.semibold))
                Text("Transfer the signed file through AirDrop, Messages, or another channel you trust, then compare the code on both physical devices.")
                    .secondaryLabelStyle()
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(24)
            Divider()

            if let completedConfirmation {
                completion(completedConfirmation)
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
        .onChange(of: mode) { _, _ in
            transferJSON = ""
            sessionJSON = ""
            comparedCode = false
        }
    }

    private var inviteFlow: some View {
        VStack(alignment: .leading, spacing: 16) {
            pairingStep(1, "Create a 10-minute invitation") {
                Text("Covalent signs this Mac's reachable transport automatically. No address or certificate entry is required.")
                    .font(.caption)
                    .secondaryLabelStyle()
                Button("Create Invitation") {
                    isWorking = true
                    Task {
                        if let invitation = await model.createInvitation(),
                           let json = try? model.transferJSON(invitation) {
                            transferJSON = json
                        }
                        isWorking = false
                    }
                }
                .buttonStyle(.borderedProminent)
                .disabled(isWorking)
                if !transferJSON.isEmpty {
                    transferBox(title: "Send this invitation to the other device", text: transferJSON)
                }
            }

            pairingStep(2, "Paste the signed response and compare codes") {
                transferEditor(text: $sessionJSON, prompt: "Paste the other device's signed reply")
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
                    .secondaryLabelStyle()
                Button("Finalize Pairing") { finalize(asInviter: true) }
                    .buttonStyle(.borderedProminent)
                    .disabled(!sessionIsMutuallySigned || isWorking)
            }
        }
    }

    private var joinFlow: some View {
        VStack(alignment: .leading, spacing: 16) {
            pairingStep(1, "Paste an invitation and choose exact roles") {
                transferEditor(text: $transferJSON, prompt: "Paste the invitation")
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
                        Text("Paste the reply that the inviting device signed and returned into the box below.")
                            .font(.caption)
                            .secondaryLabelStyle()
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
                    .secondaryLabelStyle()
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
                    .secondaryLabelStyle()
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
                .secondaryLabelStyle()
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
            Label("A device seen nearby is not trusted until you finish pairing with it.", systemImage: "lock.shield")
                .font(.caption)
                .secondaryLabelStyle()
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

    private func completion(_ confirmation: PairingConfirmation) -> some View {
        let peer = peerGrant(in: confirmation)
        return VStack(spacing: 18) {
            Spacer()
            Image(systemName: "checkmark.circle.fill")
                .scaledSymbolFont(size: 54)
                .foregroundStyle(.green)
            Text("Pairing Complete").font(.title.weight(.semibold))
            Text("\(peer.displayName) is trusted with exactly the roles both devices approved.")
                .secondaryLabelStyle()
                .multilineTextAlignment(.center)
                .frame(maxWidth: 480)
            if peer.roles.contains(.storageProvider), let transport = confirmation.peerTransport {
                Text("Add this signed device now, then choose it only for backups that should keep an extra copy.")
                    .font(.subheadline)
                    .secondaryLabelStyle()
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: 520)
                Button("Use as Backup Device") {
                    isAddingBackupDevice = true
                    Task {
                        if await model.connectProvider(using: transport) { dismiss() }
                        isAddingBackupDevice = false
                    }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(isAddingBackupDevice)
                .accessibilityIdentifier("pairing.useAsBackupDevice")
            } else {
                Text(peer.roles.contains(.storageProvider)
                    ? "This older pairing did not include signed connection details. Use Advanced recovery in Devices."
                    : "Storage access was not granted, so this device cannot keep an extra copy.")
                    .font(.subheadline)
                    .secondaryLabelStyle()
                    .multilineTextAlignment(.center)
                Button("Done") { dismiss() }
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
            }
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
                completedConfirmation = confirmation
            }
            isWorking = false
        }
    }

    private func peerGrant(in confirmation: PairingConfirmation) -> PeerGrant {
        mode == .invite ? confirmation.inviterGrant : confirmation.responderGrant
    }
}

struct MacNetworkPairingView: View {
    @ObservedObject var model: CovalentAppModel
    let pairing: NetworkPairing
    @Environment(\.dismiss) private var dismiss
    @State private var isWorking = false

    private var current: NetworkPairing {
        model.activeNetworkPairing?.id == pairing.id ? model.activeNetworkPairing ?? pairing : pairing
    }

    var body: some View {
        VStack(spacing: 22) {
            Image(systemName: current.state == .complete ? "checkmark.shield.fill" : "laptopcomputer.and.iphone")
                .scaledSymbolFont(size: 52)
                .foregroundStyle(current.state == .failed ? .red : .blue)
                .accessibilityHidden(true)
            Text(title)
                .font(.title.weight(.semibold))
                .accessibilityAddTraits(.isHeader)
            Text("Pairing lets either device store encrypted backup copies for the other. Nothing is copied now—you choose \(current.peerName) separately when creating a backup.")
                .multilineTextAlignment(.center)
                .secondaryLabelStyle()
                .frame(maxWidth: 440)

            if current.state != .complete && current.state != .failed {
                VStack(spacing: 6) {
                    Text("Confirm this code on both devices")
                        .font(.caption)
                        .secondaryLabelStyle()
                    Text(current.authenticationString)
                        .font(.system(.title, design: .monospaced, weight: .semibold))
                        .textSelection(.enabled)
                        .accessibilityLabel("Comparison code \(current.authenticationString.replacingOccurrences(of: "-", with: " "))")
                }
                .padding(16)
                .frame(maxWidth: 400)
                .background(Color.blue.opacity(0.09), in: RoundedRectangle(cornerRadius: 12))
            }

            switch current.state {
            case .awaitingLocalConfirmation:
                Button("Codes Match — Use as Backup Device") {
                    isWorking = true
                    Task {
                        await model.confirmNetworkPairing(current)
                        isWorking = false
                    }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .disabled(isWorking)
                .accessibilityIdentifier("networkPairing.confirm")
            case .awaitingPeerConfirmation:
                ProgressView("Confirmed here. Waiting for \(current.peerName)…")
            case .complete:
                if model.isProviderReady(for: current) {
                    Label("Backup device ready", systemImage: "checkmark.circle.fill")
                        .font(.headline)
                        .foregroundStyle(.green)
                    Button("Done") { finish() }
                        .buttonStyle(.borderedProminent)
                        .keyboardShortcut(.defaultAction)
                } else {
                    ProgressView("Saving the signed connection to this device…")
                }
            case .failed:
                Text(current.failureMessage ?? "The devices could not finish pairing.")
                    .foregroundStyle(.red)
                    .multilineTextAlignment(.center)
                Button("Close") { finish() }
                    .buttonStyle(.borderedProminent)
            }

            if current.state != .complete && current.state != .failed {
                Button("Cancel Pairing", role: .destructive) { finish() }
                    .disabled(isWorking)
            }
        }
        .padding(32)
        .frame(width: 560, height: 520)
        .task {
            while !Task.isCancelled && model.activeNetworkPairing?.id == current.id {
                await model.refreshNetworkPairings()
                try? await Task.sleep(for: .seconds(1))
            }
        }
    }

    private var title: String {
        switch current.state {
        case .awaitingLocalConfirmation: current.direction == .incoming ? "Pair with \(current.peerName)?" : "Confirm \(current.peerName)"
        case .awaitingPeerConfirmation: "Waiting for Other Device"
        case .complete: "Pairing Complete"
        case .failed: "Pairing Failed"
        }
    }

    private func finish() {
        isWorking = true
        Task {
            await model.dismissNetworkPairing(current)
            dismiss()
            isWorking = false
        }
    }
}
