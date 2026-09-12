import AppKit
import SwiftUI

/// The only path shown before a managed node is allowed to create identity
/// state. Selected URLs are held by this view only and are never persisted.
struct MacFirstLaunchRecoveryView: View {
    @ObservedObject var model: CovalentAppModel
    @Environment(\.dismiss) private var dismiss
    @State private var recoveryKitFile: URL?
    @State private var recoveryKeyFile: URL?
    @State private var confirmingRecovery = false
    @State private var starting = false

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            Label("Set up this Mac", systemImage: "externaldrive.badge.plus")
                .font(.title2.weight(.semibold))
            Text("Set up this Mac, or recover your backups if you’re replacing a lost device.")
                .secondaryLabelStyle()

            VStack(alignment: .leading, spacing: 10) {
                Button("Set Up This Mac") {
                    starting = true
                    Task {
                        await model.beginNormalFirstLaunch()
                        starting = false
                        if !model.needsFirstLaunchChoice { dismiss() }
                    }
                }
                .buttonStyle(.borderedProminent)
                .disabled(starting)
                .accessibilityIdentifier("firstLaunch.setup")
                Text("Start using Covalent on this Mac.")
                    .font(.caption)
                    .secondaryLabelStyle()
            }

            Divider()

            VStack(alignment: .leading, spacing: 10) {
                Text("Recover this Mac")
                    .font(.headline)
                Text("Choose the two recovery files you saved earlier. Covalent will use them to find your backups on your other devices.")
                    .font(.caption)
                    .secondaryLabelStyle()
                HStack {
                    Button(recoveryKitFile == nil ? "Choose Recovery Kit…" : "Recovery Kit Chosen") {
                        recoveryKitFile = chooseFile(title: "Choose Recovery Kit", prompt: "Use Kit")
                    }
                    .accessibilityIdentifier("firstLaunch.chooseKit")
                    Button(recoveryKeyFile == nil ? "Choose Recovery Code…" : "Recovery Code Chosen") {
                        recoveryKeyFile = chooseFile(title: "Choose Recovery Code", prompt: "Use Code")
                    }
                    .accessibilityIdentifier("firstLaunch.chooseCode")
                }
                Button("Recover This Mac…") { confirmingRecovery = true }
                    .disabled(recoveryKitFile == nil || recoveryKeyFile == nil || starting)
                    .accessibilityIdentifier("firstLaunch.recover")
            }
        }
        .padding(28)
        .frame(width: 570)
        .interactiveDismissDisabled(starting)
        .confirmationDialog(
            "Recover this Mac?",
            isPresented: $confirmingRecovery,
            titleVisibility: .visible
        ) {
            Button("Recover") {
                guard let recoveryKitFile, let recoveryKeyFile else { return }
                starting = true
                Task {
                    await model.beginRecoveryFirstLaunch(
                        recoveryKitFile: recoveryKitFile,
                        recoveryKeyFile: recoveryKeyFile
                    )
                    starting = false
                    if !model.needsFirstLaunchChoice { dismiss() }
                }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Covalent will open your recovery files and check your backup devices. Keep those devices online while recovery runs.")
        }
    }

    private func chooseFile(title: String, prompt: String) -> URL? {
        let panel = NSOpenPanel()
        panel.title = title
        panel.prompt = prompt
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        return panel.runModal() == .OK ? panel.url : nil
    }
}
