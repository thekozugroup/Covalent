# Interlink companion development

Interlink is the focused phone companion for the Pi photo appliance. Its APK
uses `life.michaelwong.interlink`, its own protected pairing identity and ports
18887/18889. Covalent remains a separate installed app. There is no automatic
account, identity or folder-grant migration between them.

The interface uses Material 3 Expressive. Each folder link shows both ends and
keeps its settings in the card. Pick a source through Android's folder picker,
pair with the appliance and approve its named receiving destination. A stable
Tailscale address can reach the same authenticated peer as the network changes;
installing Interlink does not enroll the phone in Tailscale.

Sources remain intact. A successful transfer is not proof of Google Photos
backup. Remote deletion and restoration are disabled for appliance links.
Android battery restrictions, a deliberate force-stop and locked boot can delay
reconnection; no claim of unrestricted continuous Android execution is made.

## Build and verify

Use the pinned toolchain in [the Android source guide](../../apps/android/README.md).
Set `ANDROID_HOME` and `COVALENT_ANDROID_NDK_HOME`. CI uses SDK Build Tools
37.0.0. For an explicitly reviewed local SDK installation, set
`INTERLINK_ANDROID_BUILD_TOOLS` to its absolute Build Tools directory.

```sh
./apps/android/gradlew -p apps/android --no-daemon \
  --dependency-verification=strict :companion:generateReleaseChecksums
```

This builds debug and minified release APKs plus an AAB, verifies the application
identity and version, checks both native ABIs and 16 KiB alignment, binds exact
packaged native objects to link records, and records this app's resolved Maven
dependencies, license evidence and hashes. Unreviewed licenses fail the build.
The shared generator preserves Covalent's established inventory schema.

Outputs under `apps/android/companion/build/`:

- `outputs/apk/release/companion-release-unsigned.apk` — build input only;
- `outputs/bundle/release/companion-release.aab` — unsigned build input;
- `outputs/mapping/release/` — retain for diagnosing minified builds;
- `reports/interlink/android-sbom.cdx.json` — CycloneDX runtime inventory;
- `reports/interlink/android-license-inventory.json` — reviewed license evidence;
- `reports/interlink/android-native-distribution-summary.json` — package/native binding;
- `reports/interlink/SHA256SUMS` — hashes of those release files.

CI builds and retains unsigned candidates without production signing secrets.
Its artifact paths remain relative to `companion/build`, so verify the handoff
with `shasum -a 256 -c reports/interlink/SHA256SUMS` from that root. Unsigned
candidates are not installable releases. The AAB has no store-signing workflow.

## Production identity

There is no production Interlink signer configured yet. Isolated emulator QA
uses an explicitly labeled debug certificate. Do not uninstall an existing
phone app to work around a certificate mismatch: that removes private pairing
state and folder grants. Plan and verify any deliberate migration first.

Keep a durable Interlink keystore and its backup outside the checkout. Supply
these environment variables locally or through a separated signing job:

- `INTERLINK_ANDROID_KEYSTORE_PATH` — owner-only keystore;
- `INTERLINK_ANDROID_STORE_PASSWORD`;
- `INTERLINK_ANDROID_KEY_ALIAS`;
- `INTERLINK_ANDROID_KEY_PASSWORD`;
- `INTERLINK_ANDROID_CERTIFICATE_SHA256` — independently pinned certificate hash.

```sh
python3 -B scripts/interlink-release.py sign \
  --build-tools "$ANDROID_HOME/build-tools/37.0.0" \
  --unsigned-apk apps/android/companion/build/outputs/apk/release/companion-release-unsigned.apk \
  --version 0.1.0-dev --version-code 1 \
  --source-revision "$(git rev-parse HEAD)" \
  --output-directory /absolute/private/release-staging/interlink-candidate
```

Use the exact version/code and source revision of the checksum-verified build.
The helper never creates a key or falls back to Covalent/Android debug signing.
It rejects an Android Debug certificate even when its fingerprint matches,
validates the native package, checks v2/v3 signatures and the pinned signer, emits only public APK/receipt/hash
files, and refuses to replace an existing output directory. Passwords are read
by SDK tools from environment variables, never placed in their command line.
This prepares an APK; it does not publish a release or prove device upgrade.

Before production distribution: verified commit/tag and exact CI gates;
credential-free build and checksum handoff to a separated signing job; prior
production APK upgrade with persistent pairing/SAF grants; API 37 companion
journey; physical Pi/Solstice restart and cellular handoff. Publishing and
automatic update promotion remain disabled until those checks and signer
provisioning are complete.

## Regression checks

```sh
python3 -B scripts/test-android-inventory.py
python3 -B scripts/test-interlink-release.py
python3 -B scripts/test-interlink-release-sdk.py \
  --build-tools "$ANDROID_HOME/build-tools/37.0.0" \
  --unsigned-apk apps/android/companion/build/outputs/apk/release/companion-release-unsigned.apk \
  --version 0.1.0-dev --version-code 1
```

The inventory regression exercises actual Gradle task invalidation, changed
artifact bytes/version/coordinates, unreviewed-license refusal and duplicate
native entries. SDK signing smoke uses disposable test keys, rejects debug
signing and deletes its temporary artifacts. It never installs or publishes.
