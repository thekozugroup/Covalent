# Platform capabilities

Supported platforms are Unraid, macOS, and Android. iOS and Windows are not
supported. Windows has no client or packaging at all. iOS has code and a CI lane
but is not a supported platform — see the note under the table.

| Platform | Release tier | Node ownership | File-access bridge | Release gate |
| --- | --- | --- | --- | --- |
| macOS 15+ on Apple Silicon | Tier 1 | `Covalent.app` bundles, launches, authenticates to, reconnects to, and terminates its loopback arm64 `covalent-node` helper. The arm64-only app explicitly rejects x86_64 build requests. The helper publishes a private PID-scoped readiness record, uses a private token file, and is signed with inheritance-only sandbox entitlements. | Security-scoped bookmarks and `NSFileCoordinator` stay in the app. Backup and create-only empty-folder restore stream validated ZIP data; PowerBox paths are never forwarded to the helper. | Swift contracts/real archive integration, generated arm64 Xcode build, exact app/helper architecture and signature inspection, live managed-node health, UI/accessibility, signing/notarization when credentials exist. |
| Android 17 / API 37 | Tier 1 | Connects to a user-controlled node. Development can use `adb reverse` so authenticated HTTP remains loopback; network nodes require HTTPS. | Persisted SAF tree grants stay on Android. Backup and create-only empty-folder restore stream ZIP data through `ParcelFileDescriptor`; every entry must match the signed plan. A `content://` URI is never sent to the node. | `./scripts/check-android.sh`, then headed `Covalent_API_37` gate with `./scripts/android-api37-device-test.sh`, plus real SAF backup/source-loss/restore and grant-revocation drills. |
| Docker | Tier 1 | Pinned Alpine 3.23 runtime images for `linux/amd64` and `linux/arm64` own the same Rust node and authenticated local web console. | Explicit read-only backup mounts, a durable data volume, and explicit writable restore mounts. | Per-architecture 96 MiB budgets, OCI base-label assertion, rootless/read-only runtime probe, and three-node Compose disaster-recovery E2E. |
| Unraid | Tier 1 | Docker node configured by the Unraid template. | Selected shares are read-only; restore destinations are separately writable; boot-drive backup is opt-in and read-only. | Template validation plus clean-install, upgrade, share, boot-drive, and restore drills on Unraid. |

## iOS is not supported

The Apple package contains a `CovalentIOS` target built from the same shared
sources, and CI still builds it and runs its bounded UI and accessibility script
in the `iOS Tier 2` job. None of that makes iOS a supported platform:

- No iOS build is published or installable — there is no App Store listing, no
  TestFlight, and no signed artifact in any release.
- The `iOS Tier 2` lane is deliberately **not** a required check for any release
  workflow, so an iOS failure neither blocks nor is blocked by a release. The
  omission is declared and enforced in `scripts/check-required-checks.sh`.
- The behaviour that was previously described here — document-provider folder
  selection, security-scoped access, bounded background execution — exists in
  the code but is not validated against the release gates and carries no
  support promise. Process-termination rehydration of the original
  security-scoped archive request was never completed.

Treat the iOS target as unreleased work in progress that is not currently being
invested in.

## Windows is not supported

There is no Windows client and no Windows packaging. Nothing in this repository
builds or targets Windows.

All supported platforms share protocol version 1 backup summaries and machine-readable errors. Rust, Swift, and Android decode the committed settings, manifest, pairing, backup-summary, progress, event, and error fixtures under `fixtures/contracts`. Replica placement is always an exact user-selected provider set; no client or node automatically chooses another device.
