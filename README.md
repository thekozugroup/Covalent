# Covalent

Covalent is a lightweight, self-hosted backup and restore system for devices you control. It pairs directly over a LAN or Tailnet, stores encrypted verified chunks on devices you explicitly choose, and restores relative paths only beneath a destination you authorize.

## Platform priorities

| Tier | Platforms | Release policy |
| --- | --- | --- |
| Tier 1 | arm64-only macOS on Apple Silicon, Android, Docker (`linux/amd64` and `linux/arm64`), Unraid | Must pass production gates before a release. |
| Tier 2 | iOS | Supported and validated independently; never delays Tier 1 readiness. |

iOS protects user-selected directories through document pickers and security-scoped access. Covalent does not claim or attempt a full-device iOS backup. Windows, hosted accounts, automatic replica placement, and restores outside an authorized root are out of scope.

## Install

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
- `apps/apple`: native SwiftUI macOS Tier 1 and iOS Tier 2 targets plus shared code.
- `apps/android`: native Kotlin and Jetpack Compose Tier 1 app.
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
