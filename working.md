# Working state

Updated: 2026-08-15

## Current milestone

Production Rust engine and daemon vertical slice; native Tier 1 integration remains the next release gate.

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

## Next milestones

1. Connect native clients to the real service contract and complete Tier 1 UX.
2. Complete headed-device and real Docker/Unraid installation, upgrade, share-selection, and disaster-restore drills.
3. Add platform key-store wrapping, artifact signing/SBOM/scanning, and independent cryptographic/security review.
4. Integrate Tier 2 iOS without weakening Tier 1 gates.
5. Run independent security, data-integrity, performance, accessibility, and release audits.

## Release truth

The Rust core and daemon now execute the distributed backup workflow with local, multi-provider, interruption, corruption, restart, and source-loss evidence. The product is not a production release until every Tier 1 native/package gate and independent security review is complete.

## Fresh Rust-engine evidence

- `./scripts/validate-foundation.sh`: structure, policy text, JSON, XML, and shell syntax passed.
- `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --all-features -- -D warnings`: passed.
- `cargo test --workspace --all-features`: 54 tests passed across protocol contracts, unit/property checks, stateful FFI, authenticated API/QUIC, crash recovery, adversarial source/restore handling, paired network backup/restore, and the multi-node interruption/corruption/repair scenario.
- `./scripts/smoke.sh`: real daemon health plus CLI backup, authenticated verification, source deletion, signed preview, restore, empty-directory reconstruction, and confirmed safe settings import passed.
- `cargo bench --locked -p covalent-core --bench engine_smoke`: 16 MiB chunk/encrypt/decrypt smoke completed in 48 chunks at 108.95 MiB/s on this machine.
- `swift test --package-path apps/apple` and unsigned `CovalentMac`/`CovalentIOS` builds passed; the iOS result remains Tier 2.
- `./scripts/check-android.sh`: Android unit tests, strict lint, and debug APK assembly passed.
- Docker rebuilt from `Cargo.lock`; Compose validation and the rootless, read-only, capability-dropped runtime health check passed. Unraid XML and safe mount defaults validated.
