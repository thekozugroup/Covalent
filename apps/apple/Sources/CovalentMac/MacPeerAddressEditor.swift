import SwiftUI

/// A short, modal editor for one already-trusted peer. The local node performs
/// the identity and certificate checks; this view never treats a new address
/// as proof that the device is connected.
struct MacPeerAddressEditor: View {
    @ObservedObject var model: CovalentAppModel
    @Environment(\.dismiss) private var dismiss

    @State private var peer: FolderSyncPeer
    private let expectsProvider: Bool
    @State private var candidateAddress: String
    @State private var pendingRequest: PeerAddressRefreshRequest?
    @State private var failure: NodeClientFailure?
    @State private var acceptedAddressNeedingReload: String?
    @State private var isWorking = false
    @FocusState private var focusedField: Field?

    init(model: CovalentAppModel, peer: FolderSyncPeer, expectsProvider: Bool) {
        self.model = model
        self.expectsProvider = expectsProvider
        _peer = State(initialValue: peer)
        _candidateAddress = State(initialValue: peer.address ?? "")
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text("Change Device Address")
                    .font(.title2.weight(.semibold))
                    .accessibilityAddTraits(.isHeader)
                Spacer()
            }
            .padding(.horizontal, 20)
            .padding(.top, 20)

            Form {
                Section("Device") {
                    LabeledContent("Name", value: peer.displayName)
                    LabeledContent("Current Address") {
                        Text(peer.address ?? "Unavailable")
                            .font(.body.monospaced())
                            .textSelection(.enabled)
                    }
                }

                Section("New Address") {
                    TextField(
                        "IP Address and Port",
                        text: $candidateAddress,
                        prompt: Text("192.168.1.20:8787")
                    )
                    .font(.body.monospaced())
                    .focused($focusedField, equals: .candidateAddress)
                    .disabled(pendingRequest != nil || acceptedAddressNeedingReload != nil || isWorking)
                    .accessibilityHint(
                        "Enter a numeric IPv4 address and port, or an IPv6 address in brackets followed by its port."
                    )

                    Text("Use a numeric IP address and port, such as 192.168.1.20:8787 or [2001:db8::20]:8787. Covalent verifies the device's saved identity and certificate before changing the address.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                if let acceptedAddressNeedingReload {
                    Section("Status") {
                        Label(
                            "The address update was accepted. Reload current device status before making another change.",
                            systemImage: "checkmark.circle"
                        )
                        .accessibilityValue("Accepted address: \(acceptedAddressNeedingReload)")
                    }
                }

                if let failure {
                    Section("What Happened") {
                        Label(failure.summary, systemImage: "exclamationmark.triangle.fill")
                            .foregroundStyle(.red)
                            .accessibilityLabel("Address update error")
                            .accessibilityValue(failure.summary)
                        if let detail = failure.detail {
                            DisclosureGroup("Details") {
                                Text(detail)
                                    .font(.caption.monospaced())
                                    .textSelection(.enabled)
                            }
                        }
                        if pendingRequest != nil {
                            Button("Edit Address") {
                                pendingRequest = nil
                                self.failure = nil
                                focusedField = .candidateAddress
                            }
                            .accessibilityHint("Discards the uncertain retry and lets you enter a different address.")
                        }
                    }
                }
            }
            .formStyle(.grouped)

            Divider()

            HStack {
                Spacer()

                Button(acceptedAddressNeedingReload == nil ? "Cancel" : "Close") {
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)
                .disabled(isWorking)

                if acceptedAddressNeedingReload != nil {
                    Button("Reload Status") { reloadStatus() }
                        .buttonStyle(.borderedProminent)
                        .keyboardShortcut(.defaultAction)
                        .disabled(isWorking)
                } else {
                    Button(pendingRequest == nil ? "Verify and Save" : "Try Again") {
                        submit()
                    }
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
                    .disabled(!canSubmit || isWorking || model.folderSyncMutationInFlight)
                    .accessibilityHint(
                        pendingRequest == nil
                            ? "Authenticates the paired device before saving this address."
                            : "Retries the exact address update whose result could not be confirmed."
                    )
                }
            }
            .padding(20)
        }
        .frame(width: 560, height: 480)
        .interactiveDismissDisabled(isWorking)
        .onAppear { focusedField = .candidateAddress }
        .onChange(of: candidateAddress) { _, _ in
            guard pendingRequest == nil else { return }
            failure = nil
        }
    }

    private var cleanedCandidateAddress: String {
        candidateAddress.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var canSubmit: Bool {
        if let pendingRequest {
            return pendingRequest.peerId == peer.peerId
        }
        return PeerAddressRefreshPolicy.permitsFreshRequest(
            peer: peer,
            status: model.folderSyncStatus,
            candidateAddress: cleanedCandidateAddress
        )
    }

    private func submit() {
        let request: PeerAddressRefreshRequest
        if let pendingRequest {
            request = pendingRequest
        } else {
            guard let expectedAddress = peer.address else { return }
            request = PeerAddressRefreshRequest(
                peerId: peer.peerId,
                expectedAddress: expectedAddress,
                candidateAddress: cleanedCandidateAddress
            )
        }

        isWorking = true
        failure = nil
        Task {
            let outcome = await model.refreshPeerAddress(request)
            isWorking = false
            apply(outcome)
        }
    }

    private func apply(_ outcome: PeerAddressRefreshOutcome) {
        switch outcome {
        case .saved:
            dismiss()
        case let .savedNeedsReload(peerId, address, refreshFailure):
            guard peerId == peer.peerId else { return }
            pendingRequest = nil
            candidateAddress = address
            acceptedAddressNeedingReload = address
            failure = refreshFailure
        case let .retryExact(request, retryFailure):
            guard request.peerId == peer.peerId else { return }
            pendingRequest = request
            candidateAddress = request.candidateAddress
            failure = retryFailure
        case let .requiresConfirmation(currentPeer, address, conflictFailure):
            guard currentPeer.peerId == peer.peerId else { return }
            peer = currentPeer
            pendingRequest = nil
            candidateAddress = address
            failure = conflictFailure
            focusedField = .candidateAddress
        case let .failed(finalFailure):
            pendingRequest = nil
            failure = finalFailure
            focusedField = .candidateAddress
        }
    }

    private func reloadStatus() {
        guard let acceptedAddressNeedingReload else { return }
        let mustReloadProvider = expectsProvider || model.providers.contains { $0.peerId == peer.peerId }
        isWorking = true
        Task {
            await model.refresh()
            isWorking = false
            let currentPeer = model.folderSyncStatus?.peers.first { $0.peerId == peer.peerId }
            let currentProvider = model.providers.first { $0.peerId == peer.peerId }
            if currentPeer?.address == acceptedAddressNeedingReload,
               (!mustReloadProvider || currentProvider?.address == acceptedAddressNeedingReload) {
                dismiss()
            } else {
                failure = NodeClientFailure(
                    summary: "Current device status is still unavailable. Close this sheet and try reloading Devices later.",
                    recovery: .retry
                )
            }
        }
    }

    private enum Field: Hashable {
        case candidateAddress
    }
}
