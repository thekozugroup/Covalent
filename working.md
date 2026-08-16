# Working state

Updated: 2026-08-16

## Current milestone

Release candidate 0.1.0 is prepared from pushed main `4a2fa5662632b9c14da9f8e4b0e4fc120b69044d`. Tier 1 build, device, container, and archive evidence is current; publication remains blocked on the findings below.

The validated smoke-port repair and release-evidence updates are versioned with this milestone. The candidate binaries are from the preceding pushed integration commit; publication still requires the remaining release gates below.

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
- Versioned device state, owner-only identity/content keys, safe settings transfer, signed mutual pairing, durable revocation tombstones, and signed roster anti-rollback cursors.
- Streaming content-defined chunking, deterministic authenticated deduplication, encrypted signed manifests, immutable snapshots, restart-recoverable commit transactions, retention-safe garbage collection, integrity verification, and authenticated repair.
- Handle-anchored traversal and restore, sparse files and empty directories, exclusions and explicit symlink behavior, conflict previews, fsync plus atomic rename, checkpointed pause/resume, and source/root TOCTOU checks.
- Exact explicit replication with no auto-placement, bounded parallel multi-source reads, availability states, persisted provider pins, authenticated QUIC/TLS 1.3, strict protocol negotiation, replay/rate/resource limits, opt-in mDNS, Tailscale CLI hints, and roster gossip.
- Stable CLI, authenticated local HTTP API, OpenAPI contract, and binding-safe stateful Rust facade for native consumers.
- Unit, property, adversarial, migration, crash-recovery, multi-node, interruption, corruption/repair, restore, QUIC pinning, CLI E2E, and benchmark smoke coverage.
- Exact-current unsigned release candidates, Docker SPDX SBOM, Rust license inventory, benchmark baseline, SHA-256 manifest, and validation report in `artifacts/release-candidate-0.1.0-4a2fa56/`.
- Android API 37 headed-device, Docker three-node disaster-restore, macOS Release archive, and iOS simulator UI evidence from the release-candidate pass.

## Next milestones

1. Diagnose the reproducible local macOS Xcode UI-test runner hang before treating the macOS UI gate as passed.
2. Perform a real Unraid clean-install/upgrade, selected-share, optional boot-share, and signed-preview restore drill on a provisioned host.
3. Apply Developer ID signing, hardened runtime, notarization, and stapling with release-owner credentials.
4. Publish the multi-architecture GHCR image through the tag workflow, including its CI scan and keyless Cosign signature.
5. Run independent security, data-integrity, accessibility, and release audits.

## Release truth

The Rust core and daemon execute the distributed backup workflow with local, multi-provider, interruption, corruption, restart, and source-loss evidence. This is an unsigned source/package release candidate, not a production release: the macOS UI gate is not currently clean, live Unraid drills are absent, and signing/notarization/release-image scan/signature credentials remain outstanding.

## Fresh Rust-engine evidence

- `./scripts/validate-foundation.sh`: structure, policy text, JSON, XML, and shell syntax passed.
- `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --all-features -- -D warnings`: passed.
- `COVALENT_INCLUDE_IOS=1 ./scripts/check.sh`: passed after the smoke script switched from a fixed QUIC port to independent ephemeral loopback ports; 58 Rust tests, migration coverage, clean temporary backup/config/restore smoke, benchmark smoke, macOS Debug, Android candidates, Docker runtime, and iOS simulator candidate build passed.
- `./scripts/smoke.sh`: real daemon health plus CLI backup, authenticated verification, source deletion, signed preview, restore, empty-directory reconstruction, and confirmed safe settings import passed.
- `cargo bench --locked -p covalent-core --bench engine_smoke`: 16 MiB chunk/encrypt/decrypt smoke completed in 48 chunks at 113.64 MiB/s on this machine.
- `./scripts/android-api37-device-test.sh`: passed on the headed `Covalent_API_37` API 37 device, including connected instrumentation, install/launch fallback, and screenshots. Bloop was not targeted.
- `./scripts/docker-compose-e2e.sh covalent:foundation`: fresh three-node explicit replication, source-loss, corruption-repair, settings-import, and root-confined restore passed. The image passed rootless/read-only isolation and its Unraid template validated.
- Unsigned `CovalentMac` Release archive passed with a universal `arm64`/`x86_64` bundled helper. Apple real-daemon integration and iOS Tier 2 UI passed; the macOS UI runner hung before test execution twice and remains a release finding.
