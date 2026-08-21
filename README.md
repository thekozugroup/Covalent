# Covalent

Covalent is a lightweight, self-hosted backup and restore system for devices you control. It pairs directly over a LAN or Tailnet, stores encrypted verified chunks on devices you explicitly choose, and restores relative paths only beneath a destination you authorize.

## Supported platforms

Covalent supports Unraid, macOS, and Android. These are the Tier 1 platforms,
and each must pass its production gates before a release.

| Tier 1 platform | Delivery | Release policy |
| --- | --- | --- |
| Unraid | Docker container, `linux/amd64` and `linux/arm64` | Must pass production gates before a release. |
| macOS on Apple Silicon | arm64-only app bundle | Must pass production gates before a release. |
| Android | Release APK | Must pass production gates before a release. |

**iOS and Windows are not supported.** There is no iOS or Windows build to
install, neither is covered by the release gates, and neither is being worked
toward right now. The Apple package in this repository does still contain an iOS
target built from the same shared sources, and the `iOS Tier 2` CI job still
compiles and exercises it — that is a statement about what the code contains,
not a promise of support. That job is deliberately not a required check for any
release workflow, so it can never block or unblock one.

Hosted accounts, automatic replica placement, and restores outside an authorized root are also out of scope.

## Install

> **Nothing has been released yet.** There are no git tags, no GitHub releases,
> and no published container image — `ghcr.io/thekozugroup/covalent` does not
> resolve, anonymously or otherwise. The instructions below describe the
> intended install path once a release exists; today none of them will work.
> Build from source instead (see [Start](#start)).

Download signed artifacts from the [latest release](https://github.com/thekozugroup/Covalent/releases/latest).

- Docker and Unraid: `docker pull ghcr.io/thekozugroup/covalent:v0.1.0`, then add the template at `packaging/unraid/covalent.xml`.
- macOS on Apple Silicon: the `.zip` in this release is ad-hoc signed, not notarized. Clear the quarantine flag with `xattr -dr com.apple.quarantine` after verifying its `.sha256`.
- Android: install the published `*-android-release.apk` after checking it against the release `SHA256SUMS`.

Per-release install detail and provenance live in [docs/release/notes](docs/release/notes), and the publishing lanes are documented in [docs/release/publishing.md](docs/release/publishing.md).

## Repository map

- `crates/covalent-core`: storage, verification, restore safety, and shared domain logic.
- `crates/covalent-protocol`: versioned wire and persisted contract types.
- `crates/covalent-node`: local daemon, health API, and embedded accessible console.
- `crates/covalent-ffi`: stable service facade for native clients.
- `crates/covalent-cli`: deterministic operator and developer commands.
- `apps/apple`: native SwiftUI macOS app, plus an unsupported iOS target, built from shared code.
- `apps/android`: native Kotlin and Jetpack Compose app.
- `packaging`: Docker, Unraid, and embedded web assets.
- `docs`: product, security, protocol, architecture decisions, and release gates.

## Start

Prerequisites by area: Rust 1.97.1 for the shared engine; an Apple Silicon Mac with Swift 6.3, Xcode 26, and XcodeGen for Apple; JDK 17 or 21, `adb`, and Android SDK/API 37 for Android; Docker with Compose/Buildx for containers. Choose a mode so a core-only contributor is not blocked by unrelated platform tools.

```sh
./scripts/bootstrap.sh core
./scripts/check.sh core
cargo run -p covalent-cli -- doctor
cargo run -p covalent-node -- serve --listen 127.0.0.1:8787
```

Use `apple`, `android`, `container`, or `all` with both scripts for broader work. Android headed validation additionally requires the exact `Covalent_API_37` AVD and an explicit `ANDROID_SERIAL`; Apple UI gates use the bounded scripts under `apps/apple/Scripts`. Container validation includes TLS-only management, the three-node disaster drill, and artifact budgets. Credentialed signing, notarization, public package promotion, and a physical Unraid drill are release gates, not bootstrap requirements.

Open `http://127.0.0.1:8787` for the local status console. No secret or external account is required for bootstrap or core workflows.

The implemented Rust vertical slice includes signed pairing and revocation, streaming encrypted backup, exact explicit replicas, authenticated QUIC providers, corruption repair, signed restore preview, crash recovery, and resumable root-confined restore. A direct CLI disaster-recovery drill is available through `./scripts/smoke.sh`; the full property/adversarial/multi-node suite runs with `cargo test --workspace --all-features`.

The persisted/local API contract remains protocol v1. Peer QUIC framing is independently negotiated as transport v2, so framing changes cannot silently reuse an old ALPN or signature domain. See [local API](docs/api/openapi.yaml), [protocol](docs/protocol/protocol.md), [architecture](docs/architecture/overview.md), [threat model](docs/security/threat-model.md), and [validation matrix](docs/release/validation-matrix.md).

## License

MIT. See [LICENSE](LICENSE).
