# Android app

Android is Tier 1. The native Kotlin/Jetpack Compose client targets Android 17/API 37 with AGP 9.2.1, built-in Kotlin, dynamic Material 3 color, and a restrained floating action toolbar limited to Pair, Backup, and Restore.

The client connects directly to the versioned local-node API in `docs/api/openapi.yaml`; it does not route production actions to a mock. It stores the local bearer token and queued request payloads using an Android Keystore AES-GCM key. Setup verifies the unauthenticated node status before enabling the actions.

Folder selection uses persisted Storage Access Framework tree grants. Every backup confirms readable access before queueing, every restore confirms read/write access and uses the node's signed no-write preview, and revoked access stops safely with a re-selection message. Queued backup/restore operations use a stable node job ID, WorkManager network constraints, foreground notification, retry for transient node failures, and encrypted persisted request payloads to survive app process death.

SAF paths never leave Android. Backup reads the selected tree through `ParcelFileDescriptor`, streams a validated ZIP with protocol-versioned metadata to the authenticated archive endpoint, and lets the Rust engine encrypt and commit it. Restore requires an empty authorized destination, streams the node's authenticated create-only ZIP response, matches every relative entry to the signed plan, and writes through destination file descriptors. The node never receives or interprets a `content://` URI. Plain HTTP bearer authentication is permitted only over loopback; network nodes require HTTPS.

Pairing treats LAN/Tailscale discovery as untrusted hints, accepts a signed invitation, shows the four-group comparison code, and only sends responder confirmation after the user confirms that the codes match. A user explicitly checks provider devices for each backup; an empty selection means local-only. Settings export/import uses the real safe configuration endpoints and never includes private identity keys or provider credentials. Enabling LAN discovery first requests Android's local-network runtime permission, then imports the explicit server-side setting.

```sh
./scripts/check-android.sh
```

The headed device gate requires the exact API 37 AVD name used by the release workflow. It builds every Android artifact, runs connected Compose tests, installs and launches through `mobilecli`, captures first-launch evidence, and configures loopback forwarding for a real host node:

```sh
# Start Covalent_API_37 first with mobilecli/MobileMCP or Android tooling.
./scripts/android-api37-device-test.sh
```

For a release candidate:

```sh
cd apps/android
./gradlew :app:assembleRelease :app:assembleDebugAndroidTest
```
