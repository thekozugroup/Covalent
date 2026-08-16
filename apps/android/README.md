# Android app

Android is Tier 1. The native Kotlin/Jetpack Compose client targets Android 17/API 37 with AGP 9.2.1, built-in Kotlin, dynamic Material 3 color, and a restrained floating action toolbar limited to Pair, Backup, and Restore.

The client connects directly to the versioned local-node API in `docs/api/openapi.yaml`; it does not route production actions to a mock. It stores the local bearer token, exact CA/certificate trust material, and queued request payloads using Android security storage. Setup classifies LAN addresses before validation, requests Android 17 local-network permission when needed, and verifies authoritative node status before enabling actions. Remote bearer authentication requires HTTPS with either system trust, an enrolled exact CA certificate, or an exact SHA-256 certificate pin; standard hostname verification remains enabled and there is no trust-all mode.

Folder selection uses persisted Storage Access Framework tree grants. Every backup confirms readable access before queueing, every restore confirms read/write access and uses a durable signed no-write plan ID with bounded entry pages, and revoked access stops safely with a re-selection message. Queued backup/restore operations use stable job IDs, a bounded scheduler, WorkManager network constraints, foreground notification, durable progress/error records, explicit pause/resume/cancel/retry controls, and encrypted persisted request payloads to survive rotation or process death. Completed archive responses are acknowledged only after successful SAF extraction; abandoned inactive previews are discarded explicitly.

SAF paths never leave Android. Backup reads the selected tree through `ParcelFileDescriptor`, streams a validated ZIP with protocol-versioned metadata to the authenticated archive endpoint, and lets the Rust engine encrypt and commit it. Restore requires an empty authorized destination, streams the node's authenticated create-only ZIP response, matches every relative entry to the signed plan, and writes through destination file descriptors. The node never receives or interprets a `content://` URI. Plain HTTP bearer authentication is permitted only over loopback; network nodes require HTTPS.

Pairing treats LAN/Tailscale discovery as untrusted hints. Android supports invitation creation and acceptance, transfer/share of the signed session, exact inviter/responder identities and roles beside the comparison code, each side's confirmation, mutual-signature finalization, transport pin exchange, and explicit provider connection. A user separately checks provider devices for each backup; an empty selection means local-only. Settings export/import uses the real safe configuration endpoints and never includes private identity keys or provider credentials. LAN discovery reflects the confirmed server state rather than an optimistic switch value.

```sh
./scripts/check-android.sh
```

The headed device gate requires the exact API 37 AVD name used by the release workflow. It builds every Android artifact, runs connected Compose tests, installs and launches through `mobilecli`, captures first-launch evidence, and configures loopback forwarding for a real host node:

```sh
# Start Covalent_API_37 first with mobilecli/MobileMCP or Android tooling.
./scripts/android-api37-device-test.sh
```

Unsigned local Release builds are inspection-only. The manual `Signed Android release` workflow requires a matching version tag, green exact-commit Tier 1 checks, and all four `COVALENT_ANDROID_*` signing secrets. It builds signed APK/AAB artifacts, verifies their signatures, emits mapping/SBOM/fail-closed license inventory/checksums, and runs a same-certificate API 37 upgrade gate. When no previous signed release exists, the owner must explicitly mark the first-release path; the workflow never silently substitutes debug signing.

Generate local dependency evidence without signing credentials:

```sh
cd apps/android
./gradlew :app:generateAndroidSbom
```
