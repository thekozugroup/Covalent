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
                // Leave room for the menu bar and Dock on smaller displays.
                .frame(minWidth: 900, minHeight: 560)
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
                Button("Create Link") {
                    model.selectedSection = .folders
                    Task { await model.refreshFolders() }
                }
                .keyboardShortcut("n")
                .disabled(!model.isAuthorized)

                Button("Pair Device…") {
                    model.selectedSection = .devices
                    Task { await model.refreshDiscovery() }
                }
                .keyboardShortcut("p", modifiers: [.command, .shift])
                .disabled(!model.isAuthorized)
            }
            CommandGroup(after: .sidebar) {
                Divider()
                Button("Links") { model.selectedSection = .folders }
                    .keyboardShortcut("1")
                Button("Devices") { model.selectedSection = .devices }
                    .keyboardShortcut("2")
                Button("Status") { model.selectedSection = .overview }
                    .keyboardShortcut("3")
                Divider()
                Button("Legacy Backups") { model.selectedSection = .backups }
                Button("Advanced Settings") { model.selectedSection = .settings }
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
        case .starting: "arrow.triangle.2.circlepath"
        case .ready: "checkmark.circle"
        case .needsAuthorization: "questionmark.circle"
        case .offline: "exclamationmark.triangle"
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
        Button("Open Links", systemImage: "link") {
            model.selectedSection = .folders
            showMainWindow()
        }

        Divider()

        Label(statusSummary, systemImage: serviceSymbol)
        linkRows

        Divider()

        Button("Create Link", systemImage: "folder.badge.plus") {
            model.selectedSection = .folders
            showMainWindow()
        }
        .disabled(!model.isAuthorized)

        Button("Pair Device", systemImage: "laptopcomputer.and.iphone") {
            model.selectedSection = .devices
            Task { await model.refreshDiscovery() }
            showMainWindow()
        }
        .disabled(!model.isAuthorized)

        Button("Legacy Backups", systemImage: "externaldrive") {
            model.selectedSection = .backups
            showMainWindow()
        }

        Button("Refresh Status") {
            Task { await model.refresh() }
        }

        Divider()

        SettingsLink {
            Text("Advanced Settings…")
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
    private var linkRows: some View {
        if let error = model.folderSyncError {
            Label(error, systemImage: "exclamationmark.triangle")
        } else if let status = model.folderSyncStatus {
            let shares = status.shares.filter { isLinkSummaryRow($0, status: status) }
            if shares.isEmpty {
                Text("No links")
            }
            ForEach(shares) { share in
                Label(
                    "\(share.label) — \(status.displayLabel(for: share))",
                    systemImage: menuFolderSymbol(share)
                )
                if let run = share.linkRun, run.phase != nil || run.pendingRequest != nil {
                    Text(menuRunLabel(run))
                }
                runAction(for: share)
            }
        } else {
            Text("Checking links…")
        }
    }

    @ViewBuilder
    private func runAction(for share: FolderShare) -> some View {
        if let settings = share.linkSettings {
            if let saved = model.pendingFolderLinkRunRequests.first(where: {
                $0.folderId == share.folderId
            }) {
                if saved.requiresReview {
                    Button("Review \(share.label) Run") {
                        model.selectedSection = .folders
                        showMainWindow()
                    }
                } else {
                    Button("Try \(share.label) Run Again", systemImage: "arrow.clockwise") {
                        Task { _ = await model.retryFolderLinkRun(folderId: share.folderId) }
                    }
                    .disabled(!model.isAuthorized || model.folderSyncMutationInFlight)
                }
            } else if settings.confirmed, settings.pendingChange == nil,
                      settings.conflictedChange == nil,
                      model.folderSyncStatus?.shares.contains(where: {
                          $0.folderId == share.folderId && $0.phase == .ready
                      }) == true,
                      settings.settings.permitsRunNow,
                      share.linkRun?.pendingRequest == nil,
                      share.linkRun?.isActive != true {
                Button("Run \(share.label) Now", systemImage: "play.fill") {
                    Task { _ = await model.runFolderLinkNow(folderId: share.folderId) }
                }
                .disabled(!model.isAuthorized || model.folderSyncMutationInFlight)
            }
        }
    }

    private func isLinkSummaryRow(_ share: FolderShare, status: FolderSyncStatus) -> Bool {
        status.shares.first(where: {
            $0.folderId == share.folderId && $0.phase != .removed
        })?.offerId == share.offerId
    }

    private func menuFolderSymbol(_ share: FolderShare) -> String {
        if share.phase == .paused { return "pause.circle" }
        switch share.linkRun?.phase {
        case .preparing, .running: return "arrow.triangle.2.circlepath"
        case .incomplete, .interrupted: return "exclamationmark.triangle"
        default: return "folder"
        }
    }

    private func menuRunLabel(_ run: FolderLinkRunSummary) -> String {
        if run.pendingRequest != nil { return "Run request waiting for source" }
        switch run.phase {
        case .preparing: return "Preparing run"
        case .running: return "Run in progress"
        case .succeeded: return "Last run completed"
        case .incomplete: return "Last run incomplete"
        case .interrupted: return "Run interrupted"
        case .cancelled: return "Run cancelled"
        case nil: return "Idle"
        }
    }

    private var serviceSymbol: String {
        switch model.phase {
        case .starting: "arrow.triangle.2.circlepath"
        case .ready: "checkmark.circle"
        case .needsAuthorization: "questionmark.circle"
        case .offline: "exclamationmark.triangle"
        }
    }

    private func showMainWindow() {
        openWindow(id: "main")
        NSApplication.shared.activate()
    }
}
