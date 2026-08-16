# Apple clients

`CovalentMac` is the Tier 1 native SwiftUI client. `CovalentIOS` is the independently supported Tier 2 selected-folder client. Both use the committed version 1 node API and the same observable service model; neither contains a production mock.

The macOS app bundles the Rust node executable in `Covalent.app/Contents/MacOS`. On first launch it creates private node data, starts the helper on a dynamic loopback port, reads its mode-0600 token and PID-scoped readiness record, and connects through the real authenticated version 1 API. A surviving app-owned node is reconnected after an app crash, an unhealthy node is relaunched on refresh, and an owned helper receives graceful termination when the app quits. UI tests can still inject their own real temporary node. The nested helper is signed with only App Sandbox inheritance entitlements. User-selected folder access remains in the app: Apple streams validated ZIP data while its security scope and file coordination are active, so no PowerBox-only path is forwarded to the helper. iOS uses the same stream bridge with an independently running node because iOS does not permit a durable local daemon.

## Product behavior

- macOS: automatic bundled-node setup, native sidebar, commands and shortcuts, menu bar status/actions, security-scoped streamed selected-folder backups, explicit replica selection, verify/repair, create-only signed restores into an empty authorized folder, full manual signed pairing, provider pinning/revocation, settings transfer, and revocable folder grants.
- iOS and iPadOS: the same real node workflows for user-selected document-provider folders. iOS requests finite background execution time while the node owns resumable checkpoints. It does not claim full-device backup or unrestricted continuous background execution.
- Folder access uses persistent bookmarks, balanced security-scope access, and `NSFileCoordinator`. Stale or revoked grants ask the user to choose the folder again.
- Bearer credentials are allowed over loopback HTTP or HTTPS. The client refuses remote plain-HTTP authentication.
- Discovery remains an untrusted hint. Trust requires matching roles, physical comparison of the short authentication string, signatures from both devices, finalization, and a separately pinned transport certificate.

## Generate and build

XcodeGen is the source of truth for the project. The macOS pre-build phase compiles the locked Rust node for every Xcode target architecture, creates a universal helper when both Apple Silicon and Intel are requested, embeds it beside the app executable, and signs the nested helper with the exact App Sandbox inheritance entitlements when code signing is enabled:

```sh
cd apps/apple
xcodegen generate
swift test
xcodebuild -project Covalent.xcodeproj -scheme CovalentMac -configuration Debug -destination 'platform=macOS' CODE_SIGNING_ALLOWED=NO build
xcodebuild -project Covalent.xcodeproj -scheme CovalentIOS -configuration Debug -destination 'generic/platform=iOS Simulator' CODE_SIGNING_ALLOWED=NO build
```

Create an unsigned Tier 1 archive for packaging inspection:

```sh
xcodebuild -project Covalent.xcodeproj -scheme CovalentMac -configuration Release -destination 'generic/platform=macOS' -archivePath /tmp/CovalentMac.xcarchive CODE_SIGNING_ALLOWED=NO archive
lipo /tmp/CovalentMac.xcarchive/Products/Applications/Covalent.app/Contents/MacOS/covalent-node -verify_arch arm64 x86_64
```

Distribution still requires the release owner's Developer ID signing, hardened runtime, notarization, and stapling credentials.

## Exact-current validation

The integration harness starts a real temporary Rust node and exercises backup, verify, signed restore preview, and restore execution:

```sh
./Scripts/integration-test.sh
```

The UI harnesses also start real temporary nodes. They never substitute a fixture API:

```sh
./Scripts/macos-ui-test.sh
./Scripts/ios-ui-test.sh
```

Override the simulator destination when needed:

```sh
COVALENT_IOS_DESTINATION='platform=iOS Simulator,name=iPad mini (A17 Pro),OS=latest' ./Scripts/ios-ui-test.sh
```

The scripts generate ephemeral UI-test build settings inside a guarded temporary directory and remove the node data on exit.
