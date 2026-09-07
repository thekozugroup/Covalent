# Core reliability audit — 2026-09-07

Scope: `covalent-core` and `covalent-node` crash recovery, durable writes, restore safety, and replica streaming behavior.

The durable backup and restore paths already use the expected commit ordering: encrypted chunk batches are journaled before publication; backup checkpoints are synced before completion; snapshot metadata is published only after referenced local chunks are durable; restore files are fully written, metadata-applied, and synced before a durable `Prepared` WAL record; file renames are followed by parent-directory sync; startup reconciles pending local/provider journals; engine state has an exclusive process lock.

One high-impact recovery bug was fixed. Recovery catalog enumeration used to stop at the first online provider error, and duplicate capsules were compared before owner authentication. A single failed or malicious replica could therefore block recovery even when another provider held an intact owner-signed catalog. Provider listing failures are now isolated, every capsule is authenticated and decrypted before candidate comparison, unauthorized sources are excluded, and import still returns an error when no replica supplies a usable catalog. Authentication is sequential and only encrypted capsules are retained while choosing the latest candidate, so long histories do not retain a decrypted manifest and backup key per capsule. The aggregate catalog limit now rejects only the over-limit provider instead of discarding already collected valid catalogs.

An unauthenticated capsule cannot block recovery by claiming a newer timestamp or snapshot ID. If a capsule's owner signature is valid but its encrypted payload or signed manifest is unusable, recovery records that as authenticated evidence and refuses to select an older candidate for that backup. Provider listing failures cannot prove whether an unreachable replica has a newer snapshot; successful results therefore continue to expose only the exact `source_providers` that supplied the selected authenticated capsule and do not claim catalog completeness. The current recovery API has no warning field for unreachable catalog sources. Adding a user-visible partial-recovery warning remains open because it requires a coordinated API/OpenAPI and native-client contract change.

This hardened catalog importer is currently a Rust-library capability rather than an end-user recovery flow. `NodeRuntime` can recreate an identity and signed provider connections from a `RecoveryBootstrap`, but it does not call `import_recovery_catalogs` after activating those providers. The standalone daemon, Android JNI bridge, Apple runtime, web console, and native clients expose neither recovery-kit export nor new-node recovery input; the only configured `RecoveryBootstrap` and every importer call are in tests. A runtime recovered by an external Rust embedder therefore starts with an empty backup list until code outside the shipped product invokes the core importer. Owner-loss recovery, including a visible partial-provider warning and retry path, remains a release-blocking integration gap.

Regression coverage exercises: one provider listing failure plus one intact replica; one invalid colliding capsule plus one intact replica; and the all-unusable case. The node console asset test also covers the embedded backup verification flow route and its non-stale JavaScript headers.

Focused verification commands:

```text
cargo check -p covalent-core -p covalent-node
cargo test -p covalent-core recovery_catalog_import -- --nocapture
cargo test -p covalent-node console_scripts_are_served_as_non_stale_javascript -- --nocapture
```

These passed on Rust 1.97.1. The complete `covalent-core` and `covalent-node` suites also passed: 239 tests across library, binary, integration, and doc-test targets. The tests simulate durable boundaries and replica faults; they do not reproduce physical power loss on each target filesystem.
