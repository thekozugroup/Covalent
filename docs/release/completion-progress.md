# Completion progress

Updated: 2026-09-08. Checkpoint 26 builds on published commit
`d19e189f535ef0d489ec59764c9015d1f1294210`; evidence includes the integrated
checkpoint changes described below.

**60% of acceptance milestones are verified: 12 of 20.** This is a milestone
count, not an estimate of elapsed time or remaining effort. Each milestone has
equal weight. An implemented feature does not pass until its stated execution
evidence exists. A regression can lower the score. The project is not release
complete while any milestone remains open.

The scope is personal-use macOS, Android and Docker, including Docker as the
accepted Unraid target while Atlas is offline. Developer ID/notarization and
production Android signing are deferred. Existing backup and recovery remain
independent from folder sync. Full completion requires all milestones plus the
applicable detailed gates in [the validation matrix](validation-matrix.md).

| # | Acceptance milestone | Status | Evidence or remaining work |
| --- | --- | --- | --- |
| 1 | Protected durable identity survives reopening; damaged identity fails safely | Verified | Core identity/recovery tests and actual production runtime reopening. |
| 2 | Owner-loss recovery restores identity and retained backup/provider state | Verified | Existing owner-loss runtime and source-loss recovery drills; independently retained backup/recovery API. |
| 3 | The maintained worker and guardian are pinned and checked before launch | Verified | Native/package tamper tests, exact executable digests and actual pinned workers. |
| 4 | Private engine control verifies its endpoint, bounds input and handles timeout/cancellation | Verified | Unix endpoint checks and eight TLS/config tests, including zero credential bytes to a wrong leaf and cancellation. |
| 5 | Device pairing validates signed identity and pinned transport | Verified | Actual two-NodeRuntime signed network pairing. |
| 6 | Both devices explicitly consent to a folder | Verified | Actual offer, delivery, acceptance and signed commit flow. |
| 7 | Sharing journal and retries survive restart without duplicate consent | Verified | Journal/replay tests and actual idempotent HTTP retry/cold restart. |
| 8 | Bidirectional file transfer, pause and resume work | Verified | Actual pinned-worker production runtime convergence; paused edit withheld for seven seconds. |
| 9 | Restart and peer revocation preserve local files and clean up workers | Verified | Actual reverse transfer after recipient restart, source peer revocation, file preservation and released runtime resources. |
| 10 | The macOS packaged worker runs inside its inherited app sandbox | Verified | Two signed LaunchServices TLS sessions, identity-preserving restart and complete reaping; container-owned fixture only. |
| 11 | The Android packaged worker executes under Android process restrictions | Verified | Pinned arm64/x86_64 builds and actual API 37 x86_64 guardian/worker execution in proof run 34241597202. |
| 12 | A full initial scan succeeds before any folder exchange | Verified | Two real production-runtime proofs used the exact integrated controller and pinned worker: 120,000-entry scan stayed network-inert; pause cancelled/reaped; resume transferred both ways; cold restart repeated the barrier. |
| 13 | The complete macOS user-selected-folder journey and permission repair work | Open | Native UI exists; external security-scope grant, repair and install/upgrade acceptance remain. |
| 14 | The complete Android folder journey and foreground lifecycle work | Open | Native folder UI, durable raw-folder grants and serialized foreground lifecycle are integrated; ten JNI tests pass. Actual Kotlin/Compose/device execution remains. |
| 15 | Both complete Docker architectures install and run with safe writable sync mounts | Open | Both images build; amd64 hardened runtime passes. A measured 128 MiB combined-image budget, opt-in writable mount, mapped-port fix and isolated two-node test await fresh hosted execution. Atlas uses this Docker acceptance path. |
| 16 | The server console offers an intuitive folder setup and management journey | Open | The primary Folders tab passes 101 web tests and an actual delayed DOM submit with a bounded fake node; complete real-node UI acceptance remains. |
| 17 | Peer connectivity, invitation expiry/renewal, removal and address changes have complete user journeys | Open | Basic consent/pause/remove exist; renewal, remote removal and network-address changes remain. |
| 18 | Shipped dependencies, notices and security gates are complete | Open | Rust and CodeQL checks pass; exact platform inventories, shipped notices and final container findings remain. |
| 19 | Platform installation, upgrade, accessibility and end-to-end regression gates pass | Open | Fresh hosted gates and complete native/package acceptance are required. |
| 20 | Performance is measured and optimized after stability acceptance | Open | Folder-sync benchmarks, large-folder resource bounds and measured optimization remain. |

Evidence details: [maintained-engine validation](engine-adapter-validation-2026-09-08.md).

Progress updates should report the percentage, newly verified work and the
largest remaining gate in a few short sentences. Do not infer progress from
time spent, lines changed, a queued build or an agent's implementation claim.
Update the numerator only after checking the evidence. Keep this checklist
stable; record any necessary scope change explicitly rather than quietly
changing the denominator.
