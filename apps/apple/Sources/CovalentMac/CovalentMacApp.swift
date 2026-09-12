import AppKit
import SwiftUI

@main
struct CovalentMacApp: App {
    @StateObject private var model: CovalentAppModel
    private let localNodeManager: (any LocalNodeBootstrapping)?

    init() {
        let isUITest = ProcessInfo.processInfo.environment["COVALENT_UI_TEST_BASE_URL"] != nil
        let manager: (any LocalNodeBootstrapping)?
        #if DEBUG
        if isUITest,
           ProcessInfo.processInfo.environment["COVALENT_UI_TEST_FIRST_LAUNCH"] == "1" {
            manager = FirstLaunchUITestBootstrapper()
        } else {
            manager = isUITest ? nil : LocalNodeManager()
        }
        #else
        manager = isUITest ? nil : LocalNodeManager()
        #endif
        localNodeManager = manager
        _model = StateObject(wrappedValue: CovalentAppModel(localNodeBootstrapper: manager))
    }

    var body: some Scene {
        Window("Covalent", id: "main") {
            MacRootView(model: model)
                .frame(minWidth: 900, minHeight: 640)
                // The window's hosting container carries no description, which
                // the system accessibility audit reports as "Element has no
                // description". It sits above every SwiftUI modifier — a
                // `.accessibilityLabel` here only names a *nested* group and
                // leaves the offending one untouched — so name it through
                // AppKit, where it actually lives.
                .onAppear { Self.nameWindowContainer() }
        }
        .defaultSize(width: 1_080, height: 720)
        .commands {
            CommandGroup(replacing: .newItem) {
                Button("New Backup…") {
                    model.requestNewBackup()
                }
                .keyboardShortcut("n")
                .disabled(!model.isAuthorized || model.activeTask != nil)

                Button("Pair Device…") {
                    model.selectedSection = .devices
                    Task { await model.refreshDiscovery() }
                }
                .keyboardShortcut("p", modifiers: [.command, .shift])
                .disabled(!model.isAuthorized)
            }
            CommandGroup(after: .sidebar) {
                Divider()
                Button("Pair") { model.selectedSection = .devices }
                    .keyboardShortcut("1")
                Button("Links") { model.selectedSection = .folders }
                    .keyboardShortcut("2")
                Button("Settings") { model.selectedSection = .settings }
                    .keyboardShortcut("3")
                Divider()
                Button("Overview") { model.selectedSection = .overview }
                Button("Backups") { model.selectedSection = .backups }
            }
            CommandMenu("Service") {
                Button("Refresh") {
                    Task { await model.refresh() }
                }
                .keyboardShortcut("r")
                Button("Connect…") {
                    model.presentation = .connection
                }
            }
        }

        Settings {
            MacSettingsView(model: model, compact: true)
                .frame(width: 620, height: 540)
        }

        // Keep the compact symbol presentation while making the status item
        // independently discoverable as Covalent to VoiceOver and XCTest.
        // The titled initializer alone exposes the SF Symbol name as the AX
        // title, which is not a meaningful product label.
        MenuBarExtra {
            MacMenuBarMenu(model: model)
        } label: {
            Label("Covalent", systemImage: menuBarSymbol)
                .labelStyle(.iconOnly)
                .accessibilityLabel("Covalent")
                .accessibilityValue(model.serviceStatusLabel)
        }
        .menuBarExtraStyle(.menu)
    }

    /// Gives the main window's hosting view an accessibility description.
    ///
    /// SwiftUI exposes no hook for this container, so reach it through AppKit.
    /// Runs on the next turn of the run loop because the window is not yet
    /// installed when the content view's `onAppear` fires.
    private static func nameWindowContainer() {
        DispatchQueue.main.async {
            for window in NSApplication.shared.windows {
                guard let contentView = window.contentView else { continue }
                contentView.setAccessibilityLabel("Covalent")
                contentView.setAccessibilityRoleDescription("Covalent main window content")
            }
        }
    }

    private var menuBarSymbol: String {
        switch model.phase {
        case .starting: "arrow.trianglehead.2.clockwise.rotate.90"
        case .ready: "externaldrive.badge.checkmark"
        case .needsAuthorization: "externaldrive.badge.questionmark"
        case .offline: "externaldrive.badge.xmark"
        }
    }
}

#if DEBUG
@MainActor
private final class FirstLaunchUITestBootstrapper: LocalNodeBootstrapping {
    func startupDisposition() throws -> LocalNodeStartupDisposition { .needsFirstLaunchChoice }

    func start(mode: LocalNodeStartupMode) async throws -> NodeConnectionConfiguration {
        let environment = ProcessInfo.processInfo.environment
        guard let address = environment["COVALENT_UI_TEST_BASE_URL"],
              let url = URL(string: address),
              let tokenFile = environment["COVALENT_UI_TEST_TOKEN_FILE"],
              let token = readPrivateUITestToken(relativePath: tokenFile)
        else { throw CocoaError(.fileNoSuchFile) }
        return try NodeConnectionConfiguration(baseURL: url, apiToken: token)
    }
}
#endif

private struct MacMenuBarMenu: View {
    @ObservedObject var model: CovalentAppModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
      Group {
        Button("Open Covalent") {
            showMainWindow()
        }

        Divider()

        Text(statusSummary)
        folderMenus
        if let task = model.activeTask {
            Text("\(task.kind.label): \(task.title)")
            if task.jobId != nil {
                Button(task.state == .paused ? "Resume Active Job" : "Pause Active Job") {
                    Task { await model.controlActiveTask(task.state == .paused ? .resume : .pause) }
                }
                Button("Cancel Active Job…", role: .destructive) {
                    Task { await model.controlActiveTask(.cancel) }
                }
            }
        }

        Divider()

        Button("New Backup…") {
            model.requestNewBackup()
            showMainWindow()
        }
        .disabled(!model.isAuthorized || model.activeTask != nil)

        Button("Restore Latest Backup…") {
            model.requestRestoreLatest()
            showMainWindow()
        }
        .disabled(!model.isAuthorized || model.snapshots.isEmpty || model.activeTask != nil)

        Button("Refresh Status") {
            Task { await model.refresh() }
        }

        Divider()

        SettingsLink {
            Text("Settings…")
        }

        Button("Quit Covalent") {
            NSApplication.shared.terminate(nil)
        }
        .keyboardShortcut("q")
      }
      .onAppear {
        Task { await model.refreshFolders() }
      }
    }

    private var statusSummary: String {
        if let status = model.status {
            return "\(model.serviceStatusLabel) · \(status.deviceName)"
        }
        return model.serviceStatusLabel
    }

    @ViewBuilder
    private var folderMenus: some View {
        if let error = model.folderSyncError {
            Label(error, systemImage: "exclamationmark.triangle")
        } else if let status = model.folderSyncStatus {
            let shares = status.shares.filter { $0.phase != .removed }
            ForEach(shares) { share in
                Menu {
                    let peer = status.peers.first { $0.peerId == share.peerId }
                    Text(peer?.displayName ?? "Paired Device")
                    Text(status.displayLabel(for: share))
                    Button("Show in Covalent") {
                        model.selectedSection = .folders
                        showMainWindow()
                    }
                    Divider()
                    let state = status.displayState(for: share)
                    if state == .needsAttention {
                        Button("Try Again") {
                            Task { await model.retryFolderSync() }
                        }
                        .disabled(!model.isAuthorized || model.folderSyncMutationInFlight)
                    }
                    if share.expired && !share.incoming
                        && (share.phase == .offered || share.phase == .paused) {
                        Button("Send New Invitation") {
                            Task { _ = await model.renewFolderInvitation(share.offerId) }
                        }
                        .disabled(!model.isAuthorized || model.folderSyncMutationInFlight)
                    } else if !(share.incoming && share.phase == .offered) {
                        Button(share.phase == .paused ? "Resume" : "Pause") {
                            Task {
                                await model.setFolderPaused(
                                    share.offerId,
                                    paused: share.phase != .paused
                                )
                            }
                        }
                        .disabled(
                            state == .invitationExpired
                                || !model.isAuthorized
                                || model.folderSyncMutationInFlight
                        )
                    }
                    Button("Remove…", role: .destructive) {
                        confirmRemoval(of: share)
                    }
                    .disabled(!model.isAuthorized || model.folderSyncMutationInFlight)
                } label: {
                    Label(
                        "\(share.label) — \(status.displayLabel(for: share))",
                        systemImage: share.phase == .paused ? "pause.circle" : "folder"
                    )
                }
            }
        }
    }

    private func confirmRemoval(of share: FolderShare) {
        let alert = NSAlert()
        alert.messageText = "Remove \(share.label)?"
        alert.informativeText = "Transfers stop on this Mac now and on the other device when it reconnects. Files stay on both devices."
        alert.alertStyle = .warning
        alert.addButton(withTitle: "Remove and Keep Files").hasDestructiveAction = true
        alert.addButton(withTitle: "Cancel")
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        Task { await model.removeFolder(share.offerId) }
    }

    private func showMainWindow() {
        openWindow(id: "main")
        NSApplication.shared.activate()
    }
}
