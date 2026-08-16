# Covalent

Covalent is a lightweight, self-hosted backup and restore system for devices you control. It pairs directly over a LAN or Tailnet, stores encrypted verified chunks on devices you explicitly choose, and restores relative paths only beneath a destination you authorize.

## Platform priorities

| Tier | Platforms | Release policy |
| --- | --- | --- |
| Tier 1 | macOS, Android, Docker, Unraid | Must pass production gates before a release. |
| Tier 2 | iOS | Supported and validated independently; never delays Tier 1 readiness. |

iOS protects user-selected directories through document pickers and security-scoped access. Covalent does not claim or attempt a full-device iOS backup. Windows, hosted accounts, automatic replica placement, and restores outside an authorized root are out of scope.

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

Prerequisites: Rust 1.97.1, Swift 6.3/Xcode 26 for Apple work, JDK 17 or 21 with Android SDK 36 for Android, and Docker for container checks.

```sh
./scripts/bootstrap.sh
./scripts/check.sh
cargo run -p covalent-cli -- doctor
cargo run -p covalent-node -- serve --listen 127.0.0.1:8787
```

Open `http://127.0.0.1:8787` for the local status console. No secret or external account is required for bootstrap or core workflows.

The protocol is pre-1.0 and incompatible changes remain possible. See [product requirements](docs/product/requirements.md), [architecture](docs/architecture/overview.md), [threat model](docs/security/threat-model.md), and [validation matrix](docs/release/validation-matrix.md).

## License

MIT. See [LICENSE](LICENSE).
