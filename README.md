# Covalent

Covalent is a lightweight, self-hosted backup and restore system for devices you control. It pairs directly over a LAN or Tailnet, stores encrypted verified chunks on devices you explicitly choose, and restores relative paths only beneath a destination you authorize.

## Start here

**[Back up your first folder](docs/getting-started.md)** is the single setup
guide. It takes you from prerequisites through a small backup, Verify, and a
restore test. Start there instead of reading the release or architecture docs.

Current personal-use paths do not require an Apple Developer ID or Android
production signing:

- Apple Silicon macOS uses the verified ad-hoc app build.
- Android uses the debug-signed installable APK from this checkout.
- An always-on server uses Docker built from this checkout until `v0.2.0` is
  published and pinned by immutable digest.

## Supported platforms

Covalent supports Unraid, macOS, and Android. These are the Tier 1 platforms,
and each must pass its production gates before a release.

| Tier 1 platform | Delivery | Release policy |
| --- | --- | --- |
| Unraid | Docker container, `linux/amd64` and `linux/arm64` | Must pass production gates before a release. |
| macOS on Apple Silicon | arm64-only ad-hoc app bundle for personal use | Must pass product and package gates; Developer ID/notarization is excluded. |
| Android | Debug-signed APK for personal use | Must pass product and install gates; production signing is deferred. |

**iOS and Windows are not supported.** There is no iOS or Windows build to
install, neither is covered by the release gates, and neither is being worked
toward right now. The Apple package in this repository does still contain an iOS
target built from the same shared sources, and the `iOS Tier 2` CI job still
compiles and exercises it — that is a statement about what the code contains,
not a promise of support. That job is deliberately not a required check for any
release workflow, so it can never block or unblock one.

Hosted accounts, automatic replica placement, and restores outside an authorized root are also out of scope.

## Release status

There is no active deployable release for the current KEK and trusted-claim
contract. The public `v0.1.0` alpha is historical release evidence only: it
predates that contract, so do not deploy its Docker/Unraid image or use it for
Atlas. A replacement signed release must update the template and all active
install instructions together.

- Docker and Unraid: deployment is blocked until that replacement immutable image is published. The digest in `packaging/unraid/covalent.xml` remains only to identify the historical release it will replace.
- macOS on Apple Silicon: the published v0.1.0 `.zip` is an ad-hoc-signed historical evaluation artifact, not a current deployable client workflow.
- Atlas claim client: no verified source-free CLI archive is published yet. The [CLI install guide](docs/release/cli-install.md) applies only when the replacement release attaches its verified Linux amd64, Linux arm64, and Apple Silicon macOS archives; never use a curl-pipe-shell installer.
- No Android artifact is published in v0.1.0. The
  [Android setup guide](docs/platform/android.md) documents the current
  personal-use debug APK and its upgrade boundary.

The source tree now carries the `v0.2.0` release candidate. It adds versioned
key protection, verified CLI-only first-run claiming, and acknowledged terminal
receipts. It is not published: no replacement immutable image digest or Atlas
deployment exists yet. Personal-use Android and macOS packages are built and
verified locally; production Android signing is deferred and Apple Developer
ID/notarization is excluded. See
[the v0.2.0 candidate notes](docs/release/notes/v0.2.0.md) before planning an
upgrade.

Setup belongs in [the getting-started guide](docs/getting-started.md). Per-release
provenance lives in [docs/release/notes](docs/release/notes), and maintainer-only
publishing detail lives in [docs/release/publishing.md](docs/release/publishing.md).

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

## Contribute

Prerequisites by area: Rust 1.97.1 for the shared engine; an Apple Silicon Mac with Swift 6.3, Xcode 26, and XcodeGen for Apple; JDK 17 through 25 (`17` in CI), `adb`, and Android SDK/API 37 for Android; Docker with Compose/Buildx for containers. Choose a mode so a core-only contributor is not blocked by unrelated platform tools.

```sh
./scripts/bootstrap.sh core
./scripts/check.sh core
cargo run -p covalent-cli -- doctor
```

Use `apple`, `android`, `container`, or `all` with both scripts for broader work. Android headed validation additionally requires the exact `Covalent_API_37` AVD and an explicit `ANDROID_SERIAL`; Apple UI gates use the bounded scripts under `apps/apple/Scripts`. Container validation includes TLS-only management, the three-node disaster drill, and artifact budgets. Public package promotion and a physical Unraid drill remain release gates. Apple Developer ID/notarization is excluded, and Android production signing is deferred.

Bootstrap checks tools; it does not start a node. A headless node requires an
explicitly provisioned KEK, and network access requires the TLS container path.
Use the [Docker source setup](packaging/docker/README.md#personal-use-from-this-checkout)
instead of launching `covalent-node serve` without those protections. No secret
or external account is required for bootstrap or core tests.

The implemented Rust vertical slice includes signed pairing and revocation, streaming encrypted backup, exact explicit replicas, authenticated QUIC providers, corruption repair, signed restore preview, crash recovery, and resumable root-confined restore. A direct CLI disaster-recovery drill is available through `./scripts/smoke.sh`; the full property/adversarial/multi-node suite runs with `cargo test --workspace --all-features`.

The persisted/local API contract remains protocol v1. Peer QUIC framing is independently negotiated as transport v3, so framing changes cannot silently reuse an old ALPN or signature domain. Transport v3 intentionally fails closed against v0.1.0 transport-v2 peers. See [local API](docs/api/openapi.yaml), [protocol](docs/protocol/protocol.md), [architecture](docs/architecture/overview.md), [threat model](docs/security/threat-model.md), and [validation matrix](docs/release/validation-matrix.md).

## License

MIT. See [LICENSE](LICENSE).
