# Completion plan

Requested: 2026-09-07. Status: active, not release-complete.

## Outcome

Deliver an intuitive, reliable private file-protection product for macOS,
Android, Docker, and Atlas/Unraid. A beginner should be able to install it,
connect their own devices, choose a folder, understand its protection state,
and recover a file without learning internal identifiers or reading maintainer
documentation. Reliability and recovery gates come before performance tuning.

The current implementation is backup and restore. The request also describes
Syncthing-like use; automatic two-way folder synchronization is a pending scope
decision and must not be advertised as implemented. Existing backup tests do
not prove synchronization, conflict convergence, or delete propagation.

Atmos is a separate Ubuntu test server, authorized through `ssh Atmos` for
isolated temporary-folder validation. Passing an Atmos drill does not count
as a physical Atlas/Unraid install or Android-device test. Do not change any
existing Atmos services, host settings, or user data. Use unique temporary
paths, bounded resources, unused ports, and cleanup of only this run's resources.

## Definition of 100% completion

Every applicable gate below and in [the validation matrix](validation-matrix.md)
must pass on the final source revision and its actual install artifacts. A
blocked, unrun, skipped, stale, or mocked platform check is not a pass. Record
counts and measurements, not an averaged percentage that hides a failed gate.
Close every known release-blocking finding; do not remove a gate to raise a score.

| Area | Required evidence | Current state |
| --- | --- | --- |
| Product scope | Explicit backup/sync semantics and a matching acceptance scenario | Two-way sync decision pending |
| Core correctness | Unit, property, adversarial, migration, concurrency, corruption, interrupted-job and source-loss tests; strict lint | Revised stability patch: 275 Rust tests passed, zero failed/ignored; format and strict Clippy passed. |
| Owner-device loss | A user can export a protected recovery kit, replace a lost owner device, discover its authenticated catalogs, see partial availability, and restore | Release blocker: core recovery primitives exist, but no production CLI/API/native workflow invokes recovery bootstrap and catalog import. |
| Beginner workflow | Install → connect → choose folder → protect → verify → restore through real UI; clear errors and recovery; no manual IDs in ordinary flows | Real browser unlock, automatic snapshot/name defaults, named backup selection, preview invalidation, restore and Verify passed with disposable files. Raw identifier/JSON presentation still needs simplification. Full cross-device onboarding pending. |
| macOS | Shared tests, live helper integration, native UI/accessibility, verified arm64 app package, install and upgrade | Full Xcode blocked on license acceptance; CLT-only tests lack the Testing module |
| Android | JVM, instrumented SAF and process-death tests, TalkBack/large text, install and upgrade of stable personal artifact | Fresh SDK/JDK setup and device evidence pending |
| Docker | Both CPU architectures; TLS and key-protection contracts; rootless/read-only runtime; bounded storage/memory; three-node recovery | Fresh build and runtime evidence pending |
| Atmos network drill | Mac ↔ Ubuntu pairing, explicitly selected replica, interrupted/restarted operation, source-loss restore, exact hashes, cleanup | First build passed; node correctly rejected an SFTP-copied token with mode 0644. Explicit 0600 staging added. All first-run resources cleaned. Second drill building at 1 CPU/4 GiB without swap, with at least 8 GiB host memory headroom. |
| Atlas/Unraid | Trusted preflight, exact-image install/upgrade, selected-share permissions, real backup/restore | Preflight strengthened and fixtures pass. A trusted-key read-only SSH attempt to Atlas timed out; actual host evidence pending. |
| Security and supply chain | Dependency audits, CodeQL, immutable image scans/signatures/SBOMs, safe secret storage, exact release commit provenance | cargo-audit (289 dependencies, warnings denied) and cargo-deny advisories/bans/licenses/sources passed. Hosted and artifact evidence pending; current main signature is unknown_key and account has no registered signing key. |
| Performance | Crypto ≥20 MiB/s; default 10k-entry interrupted/resumed scan and restore ≤45 s; provider restore ≥4 MiB/s and scaling gate; node ≤16 MiB, CLI ≤8 MiB, image ≤96 MiB | Baseline crypto 185.04 MiB/s, 10k-entry recovery 5.10 s, local provider restore 222–242 MiB/s; intermediate release node 10,304,896 bytes and CLI 4,079,664 bytes. Final artifacts pending. |
| Release delivery | Green exact-revision CI, verified signed tag, complete downloadable artifacts and checksums, replacement immutable Unraid digest, working beginner instructions | v0.2.0 remains unpublished |

Performance values above are the existing executable defaults in
`crates/covalent-core/benches/engine_smoke.rs` and
`scripts/check-artifact-budgets.sh`. Real network throughput is recorded
separately with payload size and environment; a local benchmark is not a WAN
throughput promise.

## Evidence from this working session

Local logs are in ignored `artifacts/validation-2026-09-07/`. Keep durable,
sanitized outcome summaries in release documentation; do not commit generated
packages, credentials, private server inventories, or raw diagnostic bundles.

- Foundation structure, setup path safety, release guardrail fixtures and
  runtime/OpenAPI coverage passed before changes.
- Baseline web tests: 61 passed. Seven behavioral verification tests were
  added for exact snapshot selection, local-only results, offline/missing/
  revoked/corrupt replicas, contradictory reports, retryable errors, and
  concurrent selection changes.
- Revised web suite: 79 passed, including first-run provider-roster,
  completed-backup selection and delayed preview/page regressions.
  Runtime/OpenAPI method-path coverage still passes.
- Atlas preflight now checks Tailnet ownership of every resolved address and
  read access as the container's UID/GID without launching a container.
- Real browser validation reproduced the unpaired-node failure, then passed
  after its fix: a valid token unlocks the console without a provider roster,
  and Verify reports the actual local snapshot intact. The same absent roster
  still rejects any nonempty provider list. Test server/data and browser were
  cleaned up afterward.
- [Draft PR #32](https://github.com/thekozugroup/Covalent/pull/32) carries the
  first stability checkpoint, commit `69a6106cbfbd6960c7fe18075ac6d6795d8b41d3`.
  Its shared Rust/contracts, macOS integration/UI, macOS app bundle, both
  container architectures, dependency-delta and CodeQL checks passed. Android lanes
  failed lint after the SAF changes left an unused resource. The obsolete
  resource is removed; a new checkpoint must rerun native checks. CI now
  preserves detailed lint and JVM reports even when an earlier gate fails.
- Browser testing caught an uninstantiated restore-preview coordinator missed
  by module tests. After wiring the coordinator, real UI selection → preview →
  option change (old preview removed) → new preview → restore passed. Restored
  bytes and the empty directory matched the source; a new backup using an
  automatically generated snapshot then verified intact. Its fixture is
  temporary and contains generated test content only.

## Work order

1. Close reproducible reliability and recovery defects; preserve old snapshots.
2. Complete the beginner workflows and verify them in the actual clients.
3. Prove real cross-machine behavior in disposable Mac/Atmos directories.
4. Complete native and container package checks, then Atlas hardware validation.
5. Measure and optimize only demonstrated performance bottlenecks, rerunning
   correctness and resource bounds after each change.
6. Assemble and verify the release and final evidence; mark the goal complete
   only when every required gate passes. Report Situation, Task, Action, and
   Result in one line each.
