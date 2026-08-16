# Working state

Updated: 2026-08-15

## Current milestone

Production monorepo foundation.

## Platform policy

- Tier 1 release gates: macOS, Android, Docker, Unraid.
- Tier 2 supported track: iOS. An iOS gap must be visible but cannot hold a Tier 1 release.
- Unsupported: Windows.

## Completed in this milestone

- Product scope, architecture, protocol, threat model, ADRs, and validation policy.
- Rust workspace boundaries for protocol, core, daemon, FFI facade, and CLI.
- Native Apple and Android project foundations.
- Docker, Unraid, embedded web console, fixtures, scripts, and CI foundations.
- Restore path and symlink safety executable tests.

## Next milestones

1. Complete encrypted chunking, signed manifests, crash-safe metadata, pairing, QUIC, discovery, and multi-provider transfer in Rust.
2. Connect native clients to the real service contract and complete Tier 1 UX.
3. Complete Docker/Unraid flows and multi-node disaster restore tests.
4. Integrate Tier 2 iOS without weakening Tier 1 gates.
5. Run independent security, data-integrity, performance, accessibility, and release audits.

## Release truth

This milestone is a foundation, not a production release. Passing foundation checks proves structure and baseline invariants only; it does not prove the complete distributed backup workflow.

## Fresh foundation evidence

- `./scripts/validate-foundation.sh`: structure, policy text, JSON, XML, and shell syntax passed.
- Rust format, Clippy with warnings denied, and all workspace tests passed; 12 executable tests cover contract fixtures, explicit replica intent, settings-key exclusion, path traversal, symlinks, FFI behavior, and node/console health.
- `./scripts/smoke.sh`: daemon health and authorized-root path smoke passed.
- `swift test --package-path apps/apple`: two shared contract tests passed.
- Generated native `CovalentMac` and `CovalentIOS` Xcode schemes built with signing disabled. macOS is Tier 1; the iOS result is reported separately as Tier 2.
- `./scripts/check-android.sh`: unit tests, strict lint, and debug APK assembly passed.
- Docker image built from the lockfile; rootless, read-only, capability-dropped runtime health passed. Unraid XML and safe mount defaults validated.
