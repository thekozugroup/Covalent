import SwiftUI
import UIKit

struct IOSDevicesView: View {
    @ObservedObject var model: CovalentAppModel
    @State private var showingProviderConnection = false
    @State private var providerToRevoke: ProviderConnection?
    @State private var tailscaleAddress = ""

    var body: some View {
        List {
            Section {
                if model.discoveryCandidates.isEmpty {
                    ContentUnavailableView {
                        Label("No candidates found", systemImage: "dot.radiowaves.left.and.right")
                    } description: {
                        Text("No LAN devices replied. You can still use a Tailscale address below.")
                    }
                    .listRowBackground(Color.clear)
                } else {
                    ForEach(model.discoveryCandidates) { candidate in
                        Button {
                            Task { await model.startNetworkPairing(candidate: candidate) }
                        } label: {
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
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .disabled(!candidate.isCompatible)
                        .accessibilityHint("Starts a direct request; both devices must confirm the same code")
                        if model.startingPairingCandidateID == candidate.id {
                            ProgressView("Contacting device…")
                        }
                    }
                }
            } header: {
                Text("Nearby devices")
            } footer: {
                Text("LAN discovery is automatic when enabled. A candidate receives no access until both devices confirm the same code.")
            }

            Section {
                TextField("nas.tailnet-name.ts.net:8787", text: $tailscaleAddress)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .keyboardType(.URL)
                    .accessibilityLabel("Tailscale hostname or IP")
                Button("Use as Backup Device") { startTailscalePairing() }
                    .disabled(
                        tailscaleAddress.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ||
                        !model.isAuthorized || model.startingPairingAddress != nil
                    )
                if model.startingPairingAddress != nil {
                    ProgressView("Contacting device…")
                }
            } header: {
                Text("Tailscale hostname or IP")
            } footer: {
                Text(
                    model.status?.lanDiscovery == false
                        ? "Automatic LAN discovery is off. Enter the address shown by the other device; both devices still confirm the same code."
                        : "Tailscale does not expose local device enumeration. Enter the other device's MagicDNS hostname or IP once."
                )
            }

            Section {
                if model.providers.isEmpty {
                    ContentUnavailableView {
                        Label("No connected storage devices", systemImage: "server.rack")
                    } description: {
                        Text("Backups stay on this device until you connect a confirmed storage device and select it yourself.")
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
                                Label("Storage device actions", systemImage: "ellipsis.circle")
                                    .labelStyle(.iconOnly)
                            }
                        }
                        .padding(.vertical, 5)
                    }
                }
            } header: {
                Text("Connected storage devices")
            }

            Section("Trust model") {
                Label("Signed roles and matching code", systemImage: "checkmark.shield")
                Label("Exact certificate, locked in", systemImage: "lock.square")
                Label("Storage device chosen separately for each backup", systemImage: "checklist")
            }

            Section("Advanced recovery") {
                DisclosureGroup {
                    Text("Recovery only: exchange signed setup files by hand when direct pairing over the network cannot be used, or when reconnecting an older backup server.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Button("Offline Pairing with Signed Files…") { model.requestManualPairing() }
                        .accessibilityIdentifier("devices.offlinePairing")
                    Button("Import Signed Connection File…") { showingProviderConnection = true }
                        .disabled(!model.isAuthorized)
                } label: {
                    Text("Manual transport details")
                        .accessibilityIdentifier("devices.advancedRecovery")
                }
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
                    Task { await model.refreshDiscovery() }
                } label: {
                    Label("Find Devices", systemImage: "plus")
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
            Text("Covalent permanently records that this device is no longer trusted and blocks any future access. Copies already stored there stay encrypted.")
        }
    }

    private func startTailscalePairing() {
        let address = tailscaleAddress.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !address.isEmpty else { return }
        Task { await model.startNetworkPairing(candidateAddress: address) }
    }
}

private struct IOSProviderConnectionView: View {
    @ObservedObject var model: CovalentAppModel
    @Environment(\.dismiss) private var dismiss
    @State private var signedTransportJSON = ""
    @State private var isConnecting = false

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Signed connection details", text: $signedTransportJSON, axis: .vertical)
                        .textInputAutocapitalization(.never)
                        .font(.caption.monospaced())
                        .lineLimit(8...16)
                } header: {
                    Text("Signed connection details")
                } footer: {
                    Text("Paste the complete connection details from a finished pairing. Loose device IDs, addresses, or certificates are never accepted.")
                }

                Section {
                    Label("Your backup server checks every detail against what both devices signed during pairing. All of them must match exactly.", systemImage: "lock.shield")
                        .foregroundStyle(.secondary)
                }
            }
            .navigationTitle("Signed Connection")
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
        !signedTransportJSON.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func connect() {
        isConnecting = true
        Task {
            if await model.connectProvider(signedTransportJSON: signedTransportJSON) {
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
    @State private var invitationJSON = ""
    @State private var sessionJSON = ""
    @State private var responderRoles: Set<PeerRole> = [.storageProvider]
    @State private var inviterRoles: Set<PeerRole> = []
    @State private var comparedCode = false
    @State private var completedConfirmation: PairingConfirmation?
    @State private var isWorking = false
    @State private var isAddingBackupDevice = false

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    if let completedConfirmation {
                        completion(completedConfirmation)
                    } else {
                        Picker("Pairing direction", selection: $mode) {
                            ForEach(Mode.allCases) { Text($0.rawValue).tag($0) }
                        }
                        .pickerStyle(.segmented)

                        Text("Transfer the signed file with Share, then compare the code on both physical devices. Seeing a device nearby is not enough to trust it.")
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
        .onChange(of: mode) { _, _ in
            invitationJSON = ""
            sessionJSON = ""
            comparedCode = false
        }
    }

    private var inviteFlow: some View {
        VStack(alignment: .leading, spacing: 18) {
            step(1, "Create a 10-minute invitation") {
                Text("Covalent signs this device's reachable transport automatically. No address or certificate entry is required.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Button("Create Invitation") { createInvitation() }
                    .buttonStyle(.borderedProminent)
                    .disabled(isWorking)
                if !invitationJSON.isEmpty {
                    transferCard("Send this invitation", text: invitationJSON)
                }
            }

            step(2, "Paste the response and compare codes") {
                transferEditor(text: $sessionJSON, prompt: "Paste the other device's signed reply")
                if let session = try? model.pairingSession(from: sessionJSON) {
                    pairingConsent(session)
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
                Text("Paste the signed reply the other device returned into the box above.")
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
                transferEditor(text: $invitationJSON, prompt: "Paste the invitation")
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
                    pairingConsent(session)
                    authenticationCode(session.authenticationString)
                    Toggle("I compared this code on both devices", isOn: $comparedCode)
                    Button(session.responderConfirmationSignature == nil ? "Confirm This Device" : "This Device Confirmed") {
                        confirm(asInviter: false)
                    }
                    .disabled(!comparedCode || session.responderConfirmationSignature != nil || isWorking)
                    if session.responderConfirmationSignature != nil {
                        transferCard("Send this session to the inviter", text: sessionJSON)
                        Text("Replace the text below with the signed reply from the inviting device.")
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

    private func pairingConsent(_ session: PairingSession) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Exact signed consent").font(.caption.weight(.semibold))
            Text("Inviter: \(session.invitation.inviterDeviceName.isEmpty ? "Unnamed device" : session.invitation.inviterDeviceName)")
            Text(session.invitation.inviterDeviceId.uuidString).font(.caption2.monospaced())
            Text("Inviter receives: \(roleSummary(session.inviterRoles))")
            Divider()
            Text("Responder: \(session.responderName)")
            Text(session.responderDeviceId.uuidString).font(.caption2.monospaced())
            Text("Responder receives: \(roleSummary(session.responderRoles))")
        }
        .font(.caption)
        .textSelection(.enabled)
        .padding(12)
        .background(Color.orange.opacity(0.08), in: RoundedRectangle(cornerRadius: 10))
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

    private func completion(_ confirmation: PairingConfirmation) -> some View {
        let peer = peerGrant(in: confirmation)
        return VStack(spacing: 16) {
            Image(systemName: "checkmark.circle.fill")
                .scaledSymbolFont(size: 58)
                .foregroundStyle(.green)
                .accessibilityHidden(true)
            Text("Pairing Complete").font(.title.weight(.semibold))
            Text("\(peer.displayName) is trusted with exactly the roles both devices approved.")
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            if peer.roles.contains(.storageProvider), let transport = confirmation.peerTransport {
                Text("Add this signed device now, then choose it only for backups that should keep an extra copy.")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                Button("Use as Backup Device") {
                    isAddingBackupDevice = true
                    Task {
                        if await model.connectProvider(using: transport) { dismiss() }
                        isAddingBackupDevice = false
                    }
                }
                .buttonStyle(.borderedProminent)
                .disabled(isAddingBackupDevice)
                .accessibilityIdentifier("pairing.useAsBackupDevice")
            } else {
                Text(peer.roles.contains(.storageProvider)
                    ? "This older pairing did not include signed connection details. Use Advanced recovery in Devices."
                    : "Storage access was not granted, so this device cannot keep an extra copy.")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                Button("Done") { dismiss() }
                    .buttonStyle(.borderedProminent)
            }
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
            if let invitation = await model.createInvitation(),
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
                completedConfirmation = confirmation
            }
            isWorking = false
        }
    }

    private func peerGrant(in confirmation: PairingConfirmation) -> PeerGrant {
        mode == .invite ? confirmation.inviterGrant : confirmation.responderGrant
    }
}

struct IOSNetworkPairingView: View {
    @ObservedObject var model: CovalentAppModel
    let pairing: NetworkPairing
    @Environment(\.dismiss) private var dismiss
    @State private var isWorking = false

    private var current: NetworkPairing {
        if model.activeNetworkPairing?.id == pairing.id {
            return model.activeNetworkPairing ?? pairing
        }
        return pairing
    }

    var body: some View {
        NavigationStack {
            VStack(spacing: 20) {
                Spacer()
                Image(systemName: current.state == .complete ? "checkmark.shield.fill" : "laptopcomputer.and.iphone")
                    .scaledSymbolFont(size: 56)
                    .foregroundStyle(current.state == .failed ? .red : .blue)
                    .accessibilityHidden(true)
                Text(title)
                    .font(.title.weight(.semibold))
                    .multilineTextAlignment(.center)
                    .accessibilityAddTraits(.isHeader)
                Text("Pairing lets either device store encrypted backup copies for the other. Nothing is copied now—you choose \(current.peerName) separately when creating a backup.")
                    .multilineTextAlignment(.center)
                    .foregroundStyle(.secondary)

                if current.state != .complete && current.state != .failed {
                    VStack(spacing: 6) {
                        Text("Confirm this code on both devices")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Text(current.authenticationString)
                            .font(.system(.title, design: .monospaced, weight: .semibold))
                            .textSelection(.enabled)
                            .accessibilityLabel("Comparison code \(current.authenticationString.replacingOccurrences(of: "-", with: " "))")
                    }
                    .padding(16)
                    .frame(maxWidth: .infinity)
                    .background(Color.blue.opacity(0.09), in: RoundedRectangle(cornerRadius: 14))
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
                Spacer()
            }
            .padding()
            .navigationTitle("Secure Pairing")
            .navigationBarTitleDisplayMode(.inline)
        }
        .interactiveDismissDisabled(current.state != .complete && current.state != .failed)
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
