# Completion plan

Requested: 2026-09-07. Status: active, not release-complete.

## Outcome

Deliver an intuitive, reliable private folder-sync and file-protection product for macOS,
Android, Docker, and Atlas/Unraid. A beginner should be able to install it,
connect their own devices, choose a folder, understand its protection state,
and recover a file without learning internal identifiers or reading maintainer
documentation. Reliability and recovery gates come before performance tuning.

The shipped user flows are backup and restore; folder-sync foundations are in
development. Given the requested
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
| Product scope | Explicit backup/sync semantics and a matching acceptance scenario | Automatic two-way sync is required. Signed records, causal/conflict checks, protected installation keys, private storage, read-only Unix inventories, bootstrap-backed membership replay and content-gated durable publication, protected authority pins, journaled Unix creation and verified incumbent adoption, bounded Android metadata observation, durable coordinator readiness, and signed write-loss freeze/receipt/abort admission are implemented. Write-loss reconciliation, the serialized sync runtime, peer exchange, replacement/deletion, native setup and multi-device acceptance remain outstanding. |
| Core correctness | Unit, property, adversarial, migration, concurrency, corruption, interrupted-job and source-loss tests; strict lint | At `f80c375`, hosted Rust/contracts passed 610 tests across 22 suites with zero failed/ignored, strict workspace Clippy and 89 web tests. All release-candidate software and CodeQL gates passed on that checkpoint. Checkpoint `d13955a` exposed a Linux adoption inode-reuse failure despite 640 local passing tests. The descriptor-lifetime and retry fixes at `4a6d1ba` subsequently passed all 643 hosted Linux Rust tests across 22 suites with zero failed/ignored, plus 89 web tests. The next combined readiness/freeze/Unicode slice passes 467 local core tests and strict workspace Clippy; fresh platform and final release confirmation remain required. |
| Owner-device loss | A user can export a protected recovery kit, replace a lost owner device, discover its authenticated catalogs, see partial availability, and restore | API, CLI/runtime, web and native recovery flows pass. At source content matching `1839796`, the real Mac–Atmos Docker drill passed protected export, deletion of the entire original owner state, automatic catalog import and exact provider-only restore. Native UI also passes. Final artifacts and large-catalog memory evidence remain required. |
| Beginner workflow | Install → connect → choose folder → protect → verify → restore through real UI; clear errors and recovery; no manual IDs in ordinary flows | Real browser unlock, automatic snapshot/name defaults, named backup selection, preview invalidation, restore and Verify passed with disposable files. Preview now explains individual file actions and renamed conflict destinations instead of raw JSON. Full cross-device onboarding pending. |
| macOS | Shared tests, live helper integration, native UI/accessibility, verified arm64 app package, install and upgrade | At `1839796`, the app bundle, all 105 shared tests, live helper integration, and all four native UI tests passed. The native gate requires exactly four passed, zero failed/skipped, including first-launch setup/recovery cancellation and the system accessibility audit. Final artifact install/upgrade remains required. Local Xcode license and CLT Testing limitations remain. |
| Android | JVM, instrumented SAF and process-death tests, TalkBack/large text, install and upgrade of stable personal artifact | At `e5a622c`, arm64 JNI is 8,384,224 bytes and x86_64 is 9,918,032 bytes; both pass the unchanged budget. At `1839796`, Android foundation and API 37 device gates passed: 103 JVM tests and all 65 named instrumentation tests, with no skipped device tests. Recovery plural resources pass lint. At `d13955a`, hosted Android compilation, lint and packaging passed, and the saved JUnit report proves all 112 JVM tests passed (including nine metadata tests), zero failures/errors/skips. All 71 expected API 37 tests also passed by name, including six new real-provider tests. Final-revision and personal-artifact validation remain required. |
| Docker | Both CPU architectures; TLS and key-protection contracts; rootless/read-only runtime; bounded storage/memory; three-node recovery | Both image architectures and container runtime/e2e passed on checkpoint `4a6d1ba`; repeat on final revision. |
| Atmos network drill | Mac ↔ Ubuntu pairing, explicitly selected replica, interrupted/restarted operation, source-loss restore, exact hashes, cleanup | Passed with a 64 MiB incompressible payload: same-job pause/resume, provider container restart preserving identity, local source/cache loss, provider-only restore and exact content checks. Dedicated resources removed; pre-existing container/image IDs survived. See [drill evidence](atmos-drill-2026-09-07.md). |
| Atlas/Unraid | Docker deployment, Unraid template/mount contracts, exact-image install/upgrade and backup/restore; on-host Atlas check deferred by the user | Atlas is offline. The user accepted Docker validation in its place on 2026-09-07. Preflight fixtures, both Docker architectures and the full owner-loss Atmos Docker drill pass. Final-image install/upgrade remains required. No physical Atlas install is claimed. |
| Security and supply chain | Dependency audits, CodeQL, immutable image scans/signatures/SBOMs, safe secret storage, exact release commit provenance | cargo-audit (289 dependencies, warnings denied) and cargo-deny advisories/bans/licenses/sources passed. CodeQL Java/Kotlin, Swift and the repository-wide zero-open-alert policy passed at `1839796`. Final artifact evidence is pending; current main signature is unknown_key and account has no registered signing key. |
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
- At `4fa2a6a`, all 65 expected API 37 instrumentation tests passed by name,
  including the SAF permission fixtures. Android foundation also passed all
  103 JVM tests and lint. The native macOS first-launch test now passes file
  picker cancellation; its later setup assertion exposed a wrong-directory
  credential read in the debug fixture, now replaced by the existing protected
  UI-test loader. The next checkpoint `45e579d` passed all four native UI tests,
  all 105 shared Swift tests, and live helper integration. Its complete CI run
  [34181412825](https://github.com/thekozugroup/Covalent/actions/runs/34181412825)
  passed every release-candidate software gate, including both Docker
  architectures and all 65 named Android device tests. Swift/Java/Kotlin
  CodeQL and the zero-open-alert policy also passed. This is software-gate
  evidence, not completion of two-way sync or final artifact acceptance.
- The opt-in Atmos drill now supports complete owner-state loss and automatic
  catalog import from protected exported recovery files. Local fixtures prove
  bounded shutdown, unrelated-process survival, private-file handling, response
  bounds and error redaction, and retention of unowned Docker resources. The
  first full cross-machine owner-loss attempt failed during replication after
  same-job pause/resume, before recovery-file export or owner-state loss.
  The aggregate failure count did not distinguish chunk replication from
  recovery-catalog publication. Bounded redacted diagnostics and a new run are
  required. Cleanup completed and pre-existing resource checks passed; this
  attempt does not prove owner-loss recovery.
- The second full owner-loss Atmos attempt passed against source content
  exactly matching `1839796`. Backup including pause/resume took 27.16 seconds;
  full owner-loss provider-only restore took 26.80 seconds for the random
  64 MiB payload with exact hashes and tree checks. Cleanup and follow-up
  resource-absence checks passed. The first attempt's cause remains unconfirmed.
  See [full owner-loss evidence](atmos-owner-loss-2026-09-07.md).
- All release-candidate software gates also passed at `1839796` in hosted CI
  [34183456803](https://github.com/thekozugroup/Covalent/actions/runs/34183456803).
  This includes fresh native macOS/Android and both Docker architecture checks.
  Swift/Java/Kotlin CodeQL and the zero-open-alert policy passed at the same
  checkpoint;
  it does not complete the still-unshipped two-way sync feature.
- The documentation checkpoint `1d8f9e6` passed every release-candidate software
  gate in [CI run 34185411055](https://github.com/thekozugroup/Covalent/actions/runs/34185411055).
  Swift and Java/Kotlin analysis and the zero-open-alert policy also passed in
  [CodeQL run 34185411065](https://github.com/thekozugroup/Covalent/actions/runs/34185411065).
- The next sync foundation checkpoint adds canonical signed records, exact
  causal history checks, deterministic conflict projection, an encrypted
  private event log and a read-only Unix inventory scanner. Local default
  and all-feature workspace tests passed: 472 tests in 22 suites, zero failed
  or ignored. A subsequent lossless cross-Unix timestamp conversion passed
  all seven focused scanner tests. Strict all-target/all-feature workspace
  Clippy and foundation validation passed. Exact-revision hosted checks are
  still required before treating the new checkpoint as software-gate evidence.
  It does not yet provide network synchronization or mutate shared user folders.
- Hosted Linux validation of `3920616` compiled the new library but stopped
  on a test-fixture cast rejected by strict Clippy. The private-directory
  fixture now uses its explicit `0700` mode, which is portable across the
  target mode types. All 20 affected local state-directory tests and strict
  workspace Clippy passed after the correction; fresh hosted checks are required.
- [Android size and host transfer evidence](android-size-profile-2026-09-07.md)
  records measured native reductions with the unchanged size ceiling and the
  four alternating host QUIC correctness/resource runs.
- [Recovery audit](core-audit-2026-09-07.md) records streaming memory limits,
  crash-safe snapshot watermarks, runtime handoff repair and bounded shutdown.
  These improvements still require final native and artifact evidence.

- The portability checkpoint `3f89119` passed every release-candidate software
  gate in [CI run 34188023482](https://github.com/thekozugroup/Covalent/actions/runs/34188023482).
  Its hosted suite passed 472 Rust tests across 22 suites with no failed or
  ignored tests, 89 web tests, both macOS lanes, both Docker architectures,
  Android foundation, and all 65 named Android API 37 tests. JNI libraries
  measured 8,384,160 bytes on arm64 and 9,918,672 bytes on x86_64; the amd64
  container measured 88,590,336 bytes, all within the existing budgets.
  Swift and Java/Kotlin CodeQL and the zero-open-alert policy also passed in
  [CodeQL run 34188023544](https://github.com/thekozugroup/Covalent/actions/runs/34188023544).
  These checks validate the foundation checkpoint,
  not network folder synchronization or final release artifacts.
- The next reviewed slice adds closed event envelopes with explicitly
  untrusted routing hints and exact membership-transition validation.
  Local focused checks pass all 11 envelope and 13 transition tests, including
  incomparable survivor histories whose join must preserve both frontiers,
  the mandatory receipt from a downgraded read-only survivor, stale evidence,
  exact cutoff digests, actor/key non-reuse and local history beyond a cutoff.
  The transition result is a pure plan; durable activation and live peer
  authorization remain the runtime's responsibility.
- The initial concrete replay engine now admits genesis and ordinary signed
  operations into one bounded, accepted-history index and per-path causal
  register. It rejects prepared changes from another instance or revision,
  retains exact equivocation history, distinguishes missing predecessor
  history for retry, and rejects index/count exhaustion before commit.
  The durable publisher derives one folder-global counter and full causal
  context, then exposes signed bytes only after append, sync and machine
  commit. Real private-log tests prove cross-path counter continuity through
  reopen, exact duplicates, quota rejection without consuming a dot, wrong
  key/identity and log-binding rejection, and external-change poisoning.
  Constructors reject pre-populated replay state before changing files.
  Full local all-feature workspace validation passes 512 tests across 22
  suites, zero failed or ignored. Bootstrap, subsequent membership epochs,
  and write-loss events remain explicitly unsupported by this runtime slice.
  It does not transfer content or mutate synchronized user folders.
- Checkpoint `5bb37ec504d2a412a2add62dffcb48a0ab61b382` subsequently passed
  [all release-candidate software gates](https://github.com/thekozugroup/Covalent/actions/runs/34190951628)
  and [CodeQL with the repository-wide zero-open-alert policy](https://github.com/thekozugroup/Covalent/actions/runs/34190951668).
  The hosted Rust log proves 512 passed tests across 22 suites with zero failed
  or ignored; web tests prove 89 passed and zero failed. Android instrumentation
  proves all 65 expected tests passed by name. Both Docker architectures, macOS
  integration/UI and packaging, Android foundation, and dependency checks passed.
  These results validate this checkpoint, not unshipped network folder sync or
  a final signed release artifact.
- The next slice adds replay of bootstrap-backed membership additions and
  Read-to-RW upgrades, ordinary Read removal, permanent writer/epoch history,
  bootstrap publication floors, and bounded pending evidence. Signed receipt
  verification precedes equivocation classification, and retained frontier
  allocations are charged explicitly. A real encrypted-log test reopens a
  two-writer membership and operation history.
- The same slice adds bounded ordered content manifests and separate local
  sync encryption keys, keyed locators and exact folder/installation/generation
  authentication. Its Unix content store verifies each chunk and the ordered
  whole-file digest before issuing a durable receipt. Local file publication
  rejects absent or mismatched receipts before signing or advancing a counter.
  All final, orphan and abandoned staging records count toward finite storage
  quotas. This first store retains them permanently; explicit safe staging
  reclamation is still outstanding. No network exchange or user-folder mutation
  is provided by this slice.
- Local validation of the integrated membership/content slice passes 565
  all-feature workspace tests across 22 suites, with zero failed or ignored,
  strict all-target/all-feature workspace Clippy, formatting, and the foundation
  validation script. This includes 30 private-state filesystem tests, 17
  membership-machine tests, 13 content-encryption tests, 16 content-store tests,
  and seven durable-publication tests. Hosted native/container/security checks
  remain required for the new checkpoint and again for the final release.
- Checkpoint `ec057326a2edca341dda1673bde326987af2ccdd` subsequently passed
  [all release-candidate software gates](https://github.com/thekozugroup/Covalent/actions/runs/34194249214)
  and [CodeQL with the zero-open-alert policy](https://github.com/thekozugroup/Covalent/actions/runs/34194249234).
  The hosted full-workspace run passed 565 tests across 22 suites, zero failed
  or ignored; three nested subprocess test summaries are excluded from this
  total. All 89 web tests and all 65 expected Android instrumentation tests
  passed. Both Docker architectures and macOS integration/UI and bundle gates
  passed. These checks do not constitute final artifact or network-sync proof.
- The next installation slice adds immutable protected local writer/key records,
  distinct installation/generation domains, bounded canonical authentication,
  no-clobber creation and durable reopen. A consuming core handoff keeps the
  outer lock while creating and reopening actual event/content stores; it proves
  stable keys, retained content and counter continuity across two publications.
  Secret-wrapping entropy failures now return a fixed retryable native error.
  Accepted-operation views bind application to admitted history; an ordered
  descendant check exposes live child entries before ancestor-file creation.
- Local validation of this installation slice passes 578 all-feature workspace
  tests across 22 suites, zero failed or ignored, strict all-target/all-feature
  workspace Clippy, formatting and foundation checks. This includes eight
  installation tests and the real child-store handoff, entropy, native-error
  mapping, admitted-view and descendant regressions. Authority configuration,
  the folder coordinator and safe user-folder mutation remain outstanding;
  hosted checks must still run on this new checkpoint and the final release.
- Checkpoint `088c14707f0fd09b897a73529f900c49554f038e` subsequently passed
  [all release-candidate software gates](https://github.com/thekozugroup/Covalent/actions/runs/34196658813)
  and [CodeQL with the zero-open-alert policy](https://github.com/thekozugroup/Covalent/actions/runs/34196658755).
  The hosted workspace run passed 578 tests across 22 suites, zero failed or
  ignored, along with 89 web tests and all 65 expected Android instrumentation
  tests by name. Both Docker architectures and macOS integration/UI and package
  checks passed. Hosted Linux release binaries remained within their budgets:
  node 13,007,472 bytes of 16,777,216; CLI 5,228,448 bytes of 8,388,608.
  Final signed artifacts and the outstanding sync runtime still require proof.
- The following history-read slice exposes authenticated committed events in
  pages capped at 256 records and 4 MiB plaintext, plus bounded scratch space.
  Opaque cursors reject other handles and reopen, preserve retry positions, and
  can continue after later appends. Every returned event must match accepted
  history; read errors, corruption or external changes invalidate the handle
  without exposing a partial page. Apply-journal plaintext is never exported
  through this API. Live session authorization and wire cursors remain future
  runtime work. A real second private log reconstructs the same current state
  from pages while retaining superseded operation history.
- Local validation of the history-read slice passes 588 all-feature workspace
  tests across 22 suites, zero failed or ignored, strict workspace Clippy,
  formatting and foundation checks. Nine focused page tests cover byte/count
  boundaries, mid-page faults, authenticated-but-unaccepted replacements,
  uncertain writes, stale cursors, private-lock replacement and redaction.
  The foundation run also exposed a stub readiness race in the Docker entrypoint
  fixture: checking file existence could read partially written arguments. The
  fixture now atomically renames the completed argument record before signalling
  readiness; all four entrypoint tests and the full foundation check pass.
  Production container behavior was not changed by this fixture correction.


- History-read checkpoint `ea4a0a1` subsequently passed every
  [release-candidate software gate](https://github.com/thekozugroup/Covalent/actions/runs/34198531297).
  Hosted Rust ran 588 tests across 22 suites, zero failed or ignored; web ran
  89 tests with zero failed or skipped. Android API 37 proved all 65 expected
  instrumentation tests passed by name. Both Docker architectures, runtime/e2e,
  macOS integration/UI/bundle, Android packaging and iOS checks passed. Linux
  node and CLI sizes remained 13,007,472 and 5,228,448 bytes, within their budgets.
  [CodeQL also passed](https://github.com/thekozugroup/Covalent/actions/runs/34198531311),
  including Swift, Java/Kotlin and the policy gate.
- The next Unix apply slice journals admitted operations before creating user
  files or directories, verifies staged content and inode identity, syncs files
  and parents, and promotes without replacing an incumbent. Every retry uses
  the original pending intent. Reopening an interrupted journal against a
  different authorized root fails before creating anything there. Unproven
  stages remain pending and preserved. Existing directories can be verified;
  existing-file adoption, replacement, deletion and conflict materialization
  remain separate work. Pre-intent parent/name failures are visible errors,
  not durable conflict records. No applied peer frontier is exposed yet.
- Local validation of this create-only slice passes 610 all-feature workspace
  tests across 22 suites, zero failed or ignored, strict workspace Clippy,
  formatting and foundation checks. Its 22 focused tests include interrupted
  intent/stage/promotion/sync boundaries, retry identity, root/parent changes,
  file corruption, resource bounds, codec rejection and redaction. Current
  applied-file verification is bounded and cancellable; encrypted-log replay
  is bounded but not yet cancellable. Hosted platform checks remain required
  for this checkpoint, and all final runtime/artifact gates remain open.

- Create-only checkpoint `f80c375` passed every
  [release-candidate software gate](https://github.com/thekozugroup/Covalent/actions/runs/34200579929)
  and [CodeQL gate](https://github.com/thekozugroup/Covalent/actions/runs/34200579922).
  Hosted Rust ran 610 tests across 22 suites, zero failed or ignored; web ran
  89 with zero failed or skipped. Android API 37 proved all 65 expected tests
  passed by name. Both Docker architectures, container runtime/e2e, macOS
  integration/UI/bundle, Android packaging and iOS checks passed. Release
  node/CLI sizes stayed within budget at 13,007,472 and 5,228,448 bytes.
  This validates the checkpoint's tests; the subsequently identified adoption
  and promotion-race work still requires its own evidence.
- The following authority slice persists immutable protected authority and local
  transport pins before any child state exists. Context authentication binds
  the exact folder, installation, generation, writer and local signing key.
  Replay configuration derives from those pins, never from a peer's key claim.
  Ten focused tests pass, including real local and remote genesis admission,
  restart, identity/key changes, malformed records, private-file substitution
  and missing trust after child state. Independent read-only review found no
  implementation defect; its requested positive remote-authority restart test
  was added and passed. A stable opaque setup commitment binds authenticated
  installation, authority and transport identities for the upcoming ready marker.
  Setup trust and live private-key possession still
  belong to the authenticated pairing/session workflow.
- Controlled event-log replay now checks pause/cancel between bounded frame
  reads and transitions and exposes no partial state after interruption. A
  started valid incomplete-tail repair finishes sync before honoring cancel.
  Four new regressions plus the existing history suite pass all 25 event-log
  tests. The Unix applier now uses this entry point and keeps subsequent
  filesystem reconciliation cancellable. The future coordinator must do the
  same. A single synchronous syscall or bounded record transition is not
  preempted. Hosted checks remain required on the resulting checkpoint.

- Startup review identified that creation-capable content-store open could
  silently recreate missing internal directories. New existing-only lock and
  content-store APIs preserve missing-state evidence and keep initialization
  separate. Three new tests pass for retained bytes/lifetime exclusion, each
  missing child or lock, and absent/unsafe lock entries. These APIs remain a
  prerequisite for the upcoming explicit coordinator-ready initialization;
  existing callers are not automatically converted to the strict mode.

- Verified incumbent adoption preserves matching file bytes and ordinary modes,
  reuses interrupted intents and never creates a missing directory. Hashing and
  sync operate on the same held descriptor, with named-identity and parent
  checks before success. Observed byte/mode/inode changes become durable
  conflicts; transient I/O remains retryable. A create-promotion race fix checks
  immediately before and after promotion, preserving an unexpected object as
  conflict. The independent final adoption review found no remaining defect.
  All 35 focused apply tests pass, including same-descriptor sync, permission
  preservation, late cancellation, retryable file/directory I/O and both sides
  of the promotion window. This still does not implement replacement/deletion
  or prove retained content availability for an adopted file.
- Android's direct SAF metadata observer requires two complete matching scans,
  preserves opaque IDs, and fails closed for incomplete provider responses,
  malformed metadata, cycles, name collisions, cancellation and observed
  mutation. Entry and UTF-8 metadata limits include pending sibling buffers.
  It opens no file content and cannot represent content proof, deletion,
  adoption or successful sync. Nine JVM tests and six real-provider device
  tests are added; the instrumentation-result fixture passes and derives 71
  required device tests by name. No local JVM, Gradle, lint or device pass is
  claimed: a working local JDK/SDK is unavailable. Hosted execution is required.
- The combined authority/adoption/controlled-replay/strict-reopen checkpoint
  passes 640 all-feature workspace tests across 22 suites with zero failed or
  ignored, strict workspace Clippy, formatting and foundation checks. Authority
  setup has ten focused tests, including all setup-commitment identity fields;
  strict existing-state startup adds three regressions. Independent reviews of
  authority, replay control and strict reopen found no remaining issues.
  These checks validate implemented boundaries; network folder sync and final
  platform/release acceptance remain open.

- Checkpoint `d13955a` failed the hosted Linux Rust gate: 445 core tests passed,
  but the existing inode-race adoption regression returned Applied instead of
  Conflict. The sync helper consumed and closed the verified descriptor before
  the final named check, permitting immediate Linux inode-number reuse for an
  unlinked replacement with identical bytes/mode. This is a production lifetime
  defect, not a test expectation to relax. The fix retains the exact synced
  file or directory descriptor through the durable Applied append. The original
  file regression is unchanged; an equivalent directory regression is added.
- An adjacent create-path review found that temporary read/stat errors could
  become permanent conflicts. Only concrete filesystem mismatches now conflict;
  I/O, resource limits and interruption preserve StageReady for exact retry.
  A new regression covers all four validation boundaries, file and directory
  creation, and each of those three failure classes. Every case closes/reopens,
  reuses the original transaction, preserves exact bytes/type and finishes once.
- The same descriptor-lifetime fix also covers file/directory creation: sync
  checks the expected target identity and retains that descriptor through the
  final named check and durable outcome. A post-sync unlink/recreate regression
  preserves replacements as conflicts. The final source passes 449 core tests
  and 643 all-feature workspace tests across 22 suites, zero failed or ignored,
  strict workspace Clippy and formatting; foundation checks also passed. The
  26 focused Unix tests include a 24-case retry matrix. Independent read-only
  review found no issue in the adoption/initial retry delta; the later create
  lifetime extension remains subject to the next combined review. Eager inode
  reuse is filesystem-dependent, so a new Linux run remains decisive evidence;
  the preceding failing Linux checkpoint is not recorded as passing.
- Android foundation at `d13955a` passed compilation, lint and packaging. Saved
  JUnit reports prove all 112 JVM tests across 12 suites passed, including all
  nine new metadata observer tests, with zero failures/errors/skips. The report
  archive SHA-256 is `686e82d9d02143174f50a918d822961a91c5edede4340ec2c9071d4f7363496d`.
  JNI sizes are 8,385,760 bytes for arm64 and 9,919,696 bytes for x86_64, both
  within the unchanged budget. API 37 instrumentation subsequently proved all
  71 expected tests passed by name at 09:07:07 UTC, including all six new real-
  provider metadata tests. Both Docker architectures, macOS integration/UI/bundle,
  iOS and dependency-delta checks also passed. The overall software gate remains
  failed because of the Linux Rust defect described above. Both CodeQL languages
  and the policy gate subsequently passed on that exact checkpoint.

- Checkpoint `4a6d1ba` subsequently passed the hosted Linux regression and all
  643 Rust tests across 22 suites, zero failed/ignored, strict Clippy, contracts
  and 89 web tests. Both Docker architectures, container runtime/e2e, macOS
  integration/UI/bundle, Android foundation and all 71 expected API 37 tests
  passed. The iOS Tier 2 accessibility audit failed with “Contrast nearly
  passed”; its separate primary-workflow test passed. The required-software
  aggregate passed because iOS is Tier 2, but the iOS failure remains an open
  finding until the new revision passes its audit. Both CodeQL languages and
  the zero-open-alert policy passed on that checkpoint.
- The new folder coordinator authenticates immutable setup and transport pins,
  holds the installation lock until every child closes, and exposes no child
  mutation/signing capability before a first-only encrypted ApplyRootReady
  record is synced. Ready binds the exact authorized root, setup commitment,
  and either owner genesis or awaiting-bootstrap initialization. Ordinary open
  requires complete existing topology; explicit initialization resumes only
  recognized empty prefixes. Unknown, missing or corrupt later state is
  preserved. Independent review found that a header-only owner event log could
  incorrectly regenerate genesis after later children existed; the fix and a
  byte-preservation regression now reject both open and resume in that state.
  All 11 coordinator tests pass, including cancellation immediately after Ready
  sync followed by successful strict reopen. The review also found no remaining
  issue in the descriptor-lifetime creation extension.
- Signed write-loss proposals now durably freeze losing writers against new
  local publication and ordinary ingress. Exact historical duplicates remain
  readable after freeze or abort; signature verification precedes equivocation
  classification. The machine retains one immutable receipt per exact survivor,
  including read-only survivors, while unresolved claims stay outside admitted
  history. An exact authority abort releases only actionable freeze quota;
  permanent evidence remains. Other membership activity is blocked while the
  proposal is pending. Local receipt signing, quarantine, reconciliation and
  write-loss epoch activation remain unfinished. Focused lifecycle, adversarial,
  quota and encrypted restart tests pass; independent review found no defect.
- Unix apply now resolves one actual on-disk spelling whose NFC form exactly
  matches the canonical path, including decomposed macOS filenames and parents.
  Case-only mismatches and multiple physical equivalent entries fail closed.
  Each bounded complete directory scan is rechecked against selected names and
  parent identity; creation still uses no-replace promotion. Three new tests
  cover scan → retain → publish → adopt → reopen, a real filesystem-dependent
  exclusive-name collision, and an incumbent appearing before promotion. All
  29 Unix tests pass locally; Linux execution remains required. This does not
  claim an atomic directory snapshot against concurrent external writers.
- The iOS selected-tab caption used light-mode system blue on an opaque white
  tab bar, with calculated contrast 4.02:1. Its adaptive replacement calculates
  to 9.23:1 on white in light mode and 9.99:1 on black in dark mode. The failing
  xcresult report did not identify its element, so this addresses the measured
  caption defect without claiming the audit is fixed before a hosted rerun.
  The original strict audit is unchanged and no failure is filtered out.

- The combined readiness/freeze/Unicode source passes 661 all-feature Rust
  workspace tests across 22 suites, zero failed/ignored, including 467 core
  tests. Strict workspace Clippy, formatting and foundation checks pass.
  The iOS color change and all other changes still require fresh hosted gates.

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
