# Apple clients

`CovalentMac` is the Tier 1 native SwiftUI client. `CovalentIOS` is the independently supported Tier 2 selected-folder client. Both use the committed version 1 node API and the same observable service model; neither contains a production mock.

The macOS app bundles the Rust node executable in `Covalent.app/Contents/MacOS`. On first launch it creates private node data, starts the helper on a dynamic loopback port, reads its mode-0600 token and PID-scoped readiness record, and connects through the real authenticated version 1 API. A surviving app-owned node is reconnected after an app crash, an unhealthy node is relaunched on refresh, and an owned helper receives graceful termination when the app quits. UI tests can still inject their own real temporary node. The nested helper is signed with only App Sandbox inheritance entitlements. User-selected folder access remains in the app: Apple streams validated ZIP data while its security scope and file coordination are active, so no PowerBox-only path is forwarded to the helper. iOS uses the same stream bridge with an independently running node because iOS does not permit a durable local daemon.

## Product behavior

- macOS: automatic bundled-node setup, native sidebar, commands and shortcuts, menu bar status/actions, security-scoped streamed selected-folder backups, explicit replica selection, verify/repair, create-only signed restores into an empty authorized folder, full manual signed pairing, provider pinning/revocation, settings transfer, and revocable folder grants.
- iOS and iPadOS: the same real node workflows for user-selected document-provider folders. iOS requests finite background execution time; expiration asks the node to durably pause the active job, cancels the in-process request, and schedules a `BGProcessingTask` refresh. The node retains the checkpoint, but this client does not yet reconstruct and resubmit the security-scoped archive request after process termination. It does not claim full-device backup, guaranteed background scheduling, unrestricted continuous execution, or completed background-resume acceptance.
- Folder access uses persistent bookmarks, balanced security-scope access, and `NSFileCoordinator`. Stale or revoked grants ask the user to choose the folder again.
- Bearer credentials are allowed over loopback HTTP or HTTPS. The client refuses remote plain-HTTP authentication. For Docker or Unraid, choose the exact Caddy `root.crt` during setup or use a certificate already trusted by the operating system. A selected CA is stored in Keychain and used as the only extra trust anchor; normal DNS hostname verification remains mandatory. There is no trust-all fallback.
- Discovery remains an untrusted hint. Trust requires matching roles, physical comparison of the short authentication string, signatures from both devices, finalization, and a separately pinned transport certificate.

## Generate and build

XcodeGen is the source of truth for the project. The macOS product supports Apple Silicon only. Its pre-build phase accepts exactly `arm64`, compiles the locked Rust node only for `aarch64-apple-darwin`, embeds it beside the app executable, and signs the nested helper with the exact App Sandbox inheritance entitlements when code signing is enabled. Intel and multi-architecture macOS builds fail closed:

```sh
cd apps/apple
xcodegen generate
swift test
xcodebuild -project Covalent.xcodeproj -scheme CovalentMac -configuration Debug -destination 'platform=macOS,arch=arm64' ARCHS=arm64 EXCLUDED_ARCHS=x86_64 CODE_SIGNING_ALLOWED=NO build
xcodebuild -project Covalent.xcodeproj -scheme CovalentIOS -configuration Debug -destination 'generic/platform=iOS Simulator' CODE_SIGNING_ALLOWED=NO build
```

Create an unsigned Tier 1 archive for packaging inspection:

```sh
xcodebuild -project Covalent.xcodeproj -scheme CovalentMac -configuration Release -destination 'generic/platform=macOS' -archivePath /tmp/CovalentMac.xcarchive ARCHS=arm64 EXCLUDED_ARCHS=x86_64 CODE_SIGNING_ALLOWED=NO archive
test "$(lipo -archs /tmp/CovalentMac.xcarchive/Products/Applications/Covalent.app/Contents/MacOS/Covalent)" = arm64
test "$(lipo -archs /tmp/CovalentMac.xcarchive/Products/Applications/Covalent.app/Contents/MacOS/covalent-node)" = arm64
```

The manual `Notarized macOS release` workflow is the only distributable macOS path. It requires an existing version tag at the selected commit, green exact-commit Tier 1 software checks, and the release owner's `APPLE_TEAM_ID`, `DEVELOPER_ID_P12_BASE64`, `DEVELOPER_ID_P12_PASSWORD`, `APPLE_NOTARY_KEY_BASE64`, `APPLE_NOTARY_KEY_ID`, and `APPLE_NOTARY_ISSUER_ID` secrets. It builds an arm64-only app and helper with hardened runtime and secure timestamps, rejects any x86_64 slice, verifies both signatures, submits to Apple, staples and assesses the app, and uploads a checksummed artifact plus dependency inventories. Missing credentials fail the workflow; they never downgrade it to ad-hoc signing.

Apple archives are create-only and descriptor-confined beneath the selected destination. Components are opened relative to the authorized root with no-follow operations and identity rechecks; root or child swaps abort. Local archive staging still needs transfer-sized temporary disk, so the client checks free capacity, reserves 256 MiB, and enforces conservative 200,000-entry, 8 GiB compressed, and 64 GiB expanded limits. This is bounded staging, not a claim of zero-copy transfer.

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

The macOS harness re-seals the fully assembled Xcode 26 UI runner before launch, runs with bounded process-group timeouts, and writes diagnostics when `COVALENT_TEST_ARTIFACT_DIR` is set. It exits 75 immediately when the headed login session is locked; unlocking requires the machine owner and is never bypassed by the script.

Generate the reviewed Swift dependency inventories and license notice bundle with:

```sh
./scripts/apple-dependency-inventory.sh
```

Override the simulator destination when needed:

```sh
COVALENT_IOS_DESTINATION='platform=iOS Simulator,name=iPad mini (A17 Pro),OS=latest' ./Scripts/ios-ui-test.sh
```

The scripts generate ephemeral UI-test build settings inside a guarded temporary directory and remove the node data on exit.
