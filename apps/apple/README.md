# Apple clients

End users should follow the
[macOS install and onboarding guide](../../docs/platform/macos.md). This file is
for source development and exact-current validation.

`CovalentMac` is the Tier 1 native SwiftUI client for macOS 15 or later on
Apple Silicon. `CovalentIOS` is an unsupported informational target. Both use
the version 1 node API and real service behavior; neither has a production mock.

## Personal app from source

From the repository root on an Apple Silicon Mac:

```sh
./scripts/build-personal-macos-app.sh
```

This is the canonical personal-use build. It installs checksum-pinned XcodeGen
in a private temporary directory, resolves only locked Swift packages, builds
and ad-hoc signs the arm64 app and bundled node, runs the repository bundle
verifier, packages the app, verifies the checksum, extracts the ZIP, and
verifies the extracted app again. Finished files land in ignored
`artifacts/install`. Existing outputs are never replaced, and the script never
installs or replaces an app. Follow the end-user guide to install and open it.

## Developer setup

Required tools:

- Apple Silicon Mac;
- Xcode 26 with command-line tools selected;
- `rustup` using `rust-toolchain.toml`;
- Rust target `aarch64-apple-darwin`; and
- checksum-pinned XcodeGen 2.46.0, installed by the repository script below.

From repository root:

```sh
test "$(uname -m)" = arm64
xcode-select -p >/dev/null
rustup show active-toolchain
rustup target add aarch64-apple-darwin

xcodegen_dir="${TMPDIR:-/tmp}/covalent-xcodegen-$PPID-$$"
./scripts/install-xcodegen.sh "$xcodegen_dir"
export PATH="$xcodegen_dir/xcodegen/bin:$PATH"
./scripts/setup-doctor.sh macos

cd apps/apple
xcodegen generate --quiet
swift test
xcodebuild \
  -project Covalent.xcodeproj \
  -scheme CovalentMac \
  -configuration Debug \
  -destination 'platform=macOS,arch=arm64' \
  ARCHS=arm64 \
  EXCLUDED_ARCHS=x86_64 \
  CODE_SIGNING_ALLOWED=NO \
  build
cd ../..
```

XcodeGen is the source of truth; do not hand-edit generated
`Covalent.xcodeproj` files. The Mac pre-build phase compiles the locked Rust
node for `aarch64-apple-darwin` and embeds it beside the app executable. Intel
or multi-architecture builds fail closed.

## Build a runnable personal archive

Use the same guarded builder from the repository root:

```sh
./scripts/build-personal-macos-app.sh
```

This produces a verified ZIP and matching SHA-256 file in `artifacts/install`.
Use the end-user guide's one-time safe Open instructions; never weaken
Gatekeeper globally.

## Exact-current validation

Run from repository root:

```sh
(
  cd apps/apple
  ./Scripts/integration-test.sh
  ./Scripts/macos-ui-test.sh
)
```

Integration starts a real temporary Rust node and exercises backup, verify,
signed restore preview, and restore execution. The macOS UI harness also uses a
real temporary node. It re-seals the assembled Xcode 26 UI runner, applies
bounded process-group timeouts, and exits 75 when the headed login session is
locked. Set `COVALENT_TEST_ARTIFACT_DIR` to retain diagnostics.

The iOS target remains useful for shared-code checks only:

```sh
(
  cd apps/apple
  xcodebuild \
    -project Covalent.xcodeproj \
    -scheme CovalentIOS \
    -configuration Debug \
    -destination 'generic/platform=iOS Simulator' \
    CODE_SIGNING_ALLOWED=NO \
    build
  COVALENT_IOS_DESTINATION='platform=iOS Simulator,id=SIMULATOR_UDID' \
    ./Scripts/ios-ui-test.sh
)
```

The scripts resolve one exact simulator ID. UI-test settings contain only a
relative token-file name; the mode-0600 token is copied into that target app's
private container and only that per-run file is removed.

Generate reviewed Swift and Rust dependency inventories from repository root:

```sh
./scripts/apple-dependency-inventory.sh
```

## Runtime design

The macOS app bundles `covalent-node`. On launch it creates private node data,
protects its key material in Keychain, starts the helper on dynamic loopback
and peer ports, reads a mode-0600 PID-scoped readiness record, then authenticates
to the real API. It reconnects a healthy app-owned node after an app crash,
relaunches an unhealthy node on refresh, and terminates its owned helper when
the app quits.

Selected-folder paths stay in the app. Backup and restore stream validated ZIP
data while a security-scoped bookmark and `NSFileCoordinator` access are active;
PowerBox-only paths never go to the helper. Stale or revoked grants require the
folder to be selected again.

Restore is descriptor-confined beneath the authorized destination. Covalent's
target inventory and conflict-policy confirmation create a signed no-write
preview, with extra confirmation for Replace. Root or child identity changes
stop the write. Local staging reserves 256 MiB and enforces 200,000-entry,
8 GiB compressed, and 64 GiB expanded limits.

LAN and Tailnet discovery are untrusted hints. Network pairing requires exact
roles and identities, a physical comparison code on both devices, both
signatures, and a pinned transport certificate. Atlas is added through Devices
as a provider; the managed Mac app stays connected to its automatic local node.
