# Completion plan

Requested: 2026-09-07. Status: active, not release-complete.

## Outcome

Deliver an intuitive, reliable private file-protection product for macOS,
Android, Docker, and Atlas/Unraid. A beginner should be able to install it,
connect their own devices, choose a folder, understand its protection state,
and recover a file without learning internal identifiers or reading maintainer
documentation. Reliability and recovery gates come before performance tuning.

The current implementation is backup and restore. Given the requested
Syncthing-like behavior, this completion goal assumes automatic two-way folder
synchronization, safe conflict handling, and recoverable history are required.
It must not be advertised as implemented until separately validated. Existing
backup tests do not prove synchronization, conflict convergence, or delete
propagation.

Atmos is a separate Ubuntu test server, authorized through `ssh Atmos` for
isolated temporary-folder validation. Passing an Atmos drill does not count
as a physical Atlas/Unraid install or Android-device test. The user confirmed
Atlas is offline and accepted Docker validation as its completion path; an
on-host Atlas installation is therefore deferred and is not a release blocker
for this goal. Do not claim it was performed. Do not change any
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
| Product scope | Explicit backup/sync semantics and a matching acceptance scenario | Automatic two-way sync is required by the working interpretation of this goal; implementation and acceptance evidence remain outstanding. |
| Core correctness | Unit, property, adversarial, migration, concurrency, corruption, interrupted-job and source-loss tests; strict lint | At `6d6ef05`, hosted Rust/contracts passed: 311 Rust tests across 22 suites, zero failed/ignored. Strict workspace Clippy and 89 web tests pass. Final native and artifact checks remain required. |
| Owner-device loss | A user can export a protected recovery kit, replace a lost owner device, discover its authenticated catalogs, see partial availability, and restore | API, CLI/runtime, web and native recovery flows published at `9e52876`. Real owner-loss HTTP/QUIC restore and web-downloaded kit roundtrip pass locally. The QUIC socket-release fix and immediate-restart recovery tests passed Linux CI at `6d6ef05`. Native UI, final artifacts and large-catalog memory evidence remain required. |
| Beginner workflow | Install → connect → choose folder → protect → verify → restore through real UI; clear errors and recovery; no manual IDs in ordinary flows | Real browser unlock, automatic snapshot/name defaults, named backup selection, preview invalidation, restore and Verify passed with disposable files. Preview now explains individual file actions and renamed conflict destinations instead of raw JSON. Full cross-device onboarding pending. |
| macOS | Shared tests, live helper integration, native UI/accessibility, verified arm64 app package, install and upgrade | At `6d6ef05`, the recovery app bundle and all 105 shared tests passed. Native UI passed three of four tests; the first-launch test matched both the file-picker and system Touch Bar Cancel buttons. Its selector is scoped to the actual file picker, and the result gate now requires all four tests. A hosted rerun remains required; local Xcode license and CLT Testing limitations remain. |
| Android | JVM, instrumented SAF and process-death tests, TalkBack/large text, install and upgrade of stable personal artifact | At `e5a622c`, arm64 JNI is 8,384,224 bytes and x86_64 is 9,918,032 bytes; both pass the unchanged budget. At `6d6ef05`, all 103 JVM tests passed; two recovery-count lint findings are corrected with plural resources. Emulator tests and the full native lanes must rerun. |
| Docker | Both CPU architectures; TLS and key-protection contracts; rootless/read-only runtime; bounded storage/memory; three-node recovery | Both image architectures and container runtime/e2e passed on checkpoint `6d6ef05`; repeat on final revision. |
| Atmos network drill | Mac ↔ Ubuntu pairing, explicitly selected replica, interrupted/restarted operation, source-loss restore, exact hashes, cleanup | Passed with a 64 MiB incompressible payload: same-job pause/resume, provider container restart preserving identity, local source/cache loss, provider-only restore and exact content checks. Dedicated resources removed; pre-existing container/image IDs survived. See [drill evidence](atmos-drill-2026-09-07.md). |
| Atlas/Unraid | Docker deployment, Unraid template/mount contracts, exact-image install/upgrade and backup/restore; on-host Atlas check deferred by the user | Atlas is offline. The user accepted Docker validation in its place on 2026-09-07. Preflight fixtures and both Docker architectures pass; final-image and full owner-loss Docker validation remain required. No physical Atlas install is claimed. |
| Security and supply chain | Dependency audits, CodeQL, immutable image scans/signatures/SBOMs, safe secret storage, exact release commit provenance | cargo-audit (289 dependencies, warnings denied) and cargo-deny advisories/bans/licenses/sources passed. CodeQL Java/Kotlin, Swift and the repository-wide zero-open-alert policy passed at `6d6ef05`. Final artifact evidence is pending; current main signature is unknown_key and account has no registered signing key. |
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
- Revised web suite: 81 passed, including first-run provider-roster,
  completed-backup selection, delayed preview/page regressions, and readable
  file/conflict actions that refuse unknown action kinds.
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
- The second checkpoint `1eaaae891d2ac33d8a1af46b9735d9a2fc913657`
  passed Rust/contracts, macOS integration/UI and packaging, both container
  architectures, and dependency checks. Both Android lanes stopped on one
  remaining `UseKtx` lint finding before instrumented execution. The call now
  uses the AndroidX `toUri` extension; a fresh run is required.
- Browser testing of the readable preview passed normal restore and renamed
  conflict explanations. It also reproduced a missing target folder reporting
  a generic server error; the error now maps to a clear invalid-folder response and passed a browser retest.

- The third checkpoint `1d0219280dd06688ee062a08916716a86c0005d6`
  passed Rust, both macOS lanes, both container architectures and Android
  foundation. The emulator's five failing SAF tests could not reset their test
  provider because of its MANAGE_DOCUMENTS protection; the fixture now adopts
  that permission only around test setup/control and drops it in `finally`.
- Recovery through the web console produced two real downloaded files. A fresh
  disposable node recovered the original identity from those files; the exact
  generated downloads and all temporary state were then removed.
- [Android size and host transfer evidence](android-size-profile-2026-09-07.md)
  records measured native reductions with the unchanged size ceiling and the
  four alternating host QUIC correctness/resource runs.
- [Recovery audit](core-audit-2026-09-07.md) records streaming memory limits,
  crash-safe snapshot watermarks, runtime handoff repair and bounded shutdown.
  These improvements still require final native and artifact evidence.

## Work order

1. Close reproducible reliability and recovery defects; preserve old snapshots.
2. Complete the beginner workflows and verify them in the actual clients.
3. Prove real cross-machine behavior in disposable Mac/Atmos directories.
4. Complete native and container package checks, including the Docker acceptance
   path the user approved for offline Atlas.
5. Measure and optimize only demonstrated performance bottlenecks, rerunning
   correctness and resource bounds after each change.
6. Assemble and verify the release and final evidence; mark the goal complete
   only when every required gate passes. Report Situation, Task, Action, and
   Result in one line each.
