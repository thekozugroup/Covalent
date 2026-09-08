# Completion progress

Updated: 2026-09-08. Checkpoint 31 builds on published commit
`04a2d0c80edc5b812daf76bc9011fae42c9df327`; evidence includes the integrated
checkpoint changes described below.

**70% of acceptance milestones are verified: 14 of 20.** This is a milestone
count, not an estimate of elapsed time or remaining effort. Each milestone has
equal weight. An implemented feature does not pass until its stated execution
evidence exists. A regression can lower the score. The project is not release
complete while any milestone remains open.

The scope is personal-use macOS, Android and Docker, including Docker as the
accepted Unraid target while Atlas is offline. Developer ID/notarization and
production Android signing are deferred. Existing backup and recovery remain
independent from folder sync. Full completion requires all milestones plus the
applicable detailed gates in [the validation matrix](validation-matrix.md).
The macOS UI must follow Apple Human Interface Guidelines; this explicit user
requirement is part of milestones 13 and 19. Review native navigation, system
controls, standard commands/shortcuts, folder pickers, permission repair,
resizing, appearance, keyboard focus and VoiceOver with execution evidence.
Use Apple’s [macOS design guidance](https://developer.apple.com/design/human-interface-guidelines/designing-for-macos/),
[keyboard guidance](https://developer.apple.com/design/human-interface-guidelines/keyboards),
and [accessibility guidance](https://developer.apple.com/design/human-interface-guidelines/accessibility/).
Passing automated tests alone does not establish HIG conformance.

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
| 13 | The complete macOS user-selected-folder journey and permission repair work | Open | The real production manager now passes native folder selection, two-way sync and cold bookmark/endpoint restoration. The production default Keychain store also passes actual ad-hoc sandbox provisioning, rotation and cold restart. Complete permission repair, default-manager startup integration, authorized upgrade and full HIG review remain. |
| 14 | The complete Android folder journey and foreground lifecycle work | Open | Direct first-launch folder setup, phone-local SAS pairing, durable grants and serialized foreground lifecycle are integrated. Twelve JNI tests pass. Checkpoints 29 and 30 pass Android foundation and all 74 existing API 37 tests. The expanded 76-test suite, complete native journey and repair still require hosted execution. |
| 15 | Both complete Docker architectures install and run with safe writable sync mounts | Verified | Checkpoint 30 jobs 102210076000 (arm64) and 102210076621 (amd64) pass all seven complete packaged-runtime checks: hardened nodes, signed pairing, dual consent/full scan/transfer, pause/resume, retained full identity on cold restart/reverse transfer, removal with retained files, and exact-resource cleanup. Atlas uses this accepted Docker path. |
| 16 | The server console offers an intuitive folder setup and management journey | Verified | Chrome drove two real NodeRuntime instances through offer, accept, bidirectional transfer, pause/resume and explicit removal. Typed paths and focus survive polling; cancel preserves sharing; removal preserves files and blocks later transfer. Both runtimes and test fixtures were cleaned. |
| 17 | Peer connectivity, invitation expiry/renewal, removal and address changes have complete user journeys | Open | Real paired workers now report connected, disconnected and reconnected while the surviving worker stays running. A second same-peer invitation and two-folder repair now pass a real 43-check proof after removing worker I/O from status polling. Renewal, remote removal and network-address changes remain. |
| 18 | Shipped dependencies, notices and security gates are complete | Open | Checkpoint 28 Rust dependency checks and all CodeQL languages pass. Linux notices build on both architectures; both Android target inventories generate successfully. The Android notices asset reader passes actual device execution. Mac target inventory, final classification and container findings remain. |
| 19 | Platform installation, upgrade, accessibility and end-to-end regression gates pass | Open | Fresh hosted gates and complete native/package acceptance are required. Checkpoint 30's optional iOS Home contrast audit failed; a Refresh-only tint correction awaits the unchanged audit. |
| 20 | Performance is measured and optimized after stability acceptance | Open | Folder-sync benchmarks, large-folder resource bounds and measured optimization remain. |

Evidence details: [maintained-engine validation](engine-adapter-validation-2026-09-08.md).

Progress updates should report the percentage, newly verified work and the
largest remaining gate in a few short sentences. Do not infer progress from
time spent, lines changed, a queued build or an agent's implementation claim.
Update the numerator only after checking the evidence. Keep this checklist
stable; record any necessary scope change explicitly rather than quietly
changing the denominator.
