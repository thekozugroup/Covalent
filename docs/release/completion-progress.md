# Completion progress

Updated: 2026-09-09. Checkpoint 39 builds on published commit
`c4be5c504f458398eb13dbe052b7b5e6496aecf0`; evidence includes the integrated
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
| 13 | The complete macOS user-selected-folder journey and permission repair work | Open | Real signed sandbox launches pass production Keychain startup, native selection, two-way sync, paused same-root regrant, replacement-root repair, cold restoration and file-preserving removal. The second-folder busy/restart correction now passes 158 isolated Swift tests and a sealed production-manager proof on checkpoint 35 plus that exact fix: two native roots, authenticated remote removal, helper-tree replacement and reaping, retirement of only the removed grant, unrelated transfer, unchanged-binary cold restart and direct inherited-scope denial for the removed root. Native HIG navigation, appearance, compact width, pickers, removal dialogs and AX labels/actions have separate execution evidence. Full keyboard/spoken VoiceOver review and personal-build upgrade remain open; automatic approval review rejected SecurityAgent access for the changed ad-hoc upgrade fixture. |
| 14 | The complete Android folder journey and foreground lifecycle work | Open | Direct first-launch folder setup, phone-local SAS pairing, durable grants and serialized foreground lifecycle are integrated. Checkpoint 38 passes JNI provenance and bounded size checks for both ABIs, JVM tests, lint and debug/release/test packaging. The API 37 device run passes 75 non-journey tests; the separate setup test fails inside packaged engine loading after installed-path/executable/hash preflight passes. Permission-denied and restored phases never execute. New native address entry and eight JVM regressions are integrated but still require hosted execution. Complete device setup, transfer, repair and lifecycle remain open. |
| 15 | Both complete Docker architectures install and run with safe writable sync mounts | Verified | Both complete Docker architectures pass package contracts, evidence extraction and all nine hardened-runtime checks in checkpoints 36 through 38: signed pairing, dual consent, full scan, transfer, pause/resume, retained identity on cold restart, offline withdrawal, signed remote delivery/acknowledgement, recipient restart, preserved files and exact-resource cleanup. Atlas uses this accepted Docker path. Vulnerability scans remain a separate open gate. |
| 16 | The server console offers an intuitive folder setup and management journey | Verified | Chrome drove two real NodeRuntime instances through offer, accept, bidirectional transfer, pause/resume and explicit removal. Typed paths and focus survive polling; cancel preserves sharing; removal preserves files and blocks later transfer. Both runtimes and test fixtures were cleaned. |
| 17 | Peer connectivity, invitation expiry/renewal, removal and address changes have complete user journeys | Open | Real worker proofs cover disconnection/reconnection, two-folder continuity, invitation renewal with lost-response retry and cold reopen, and signed removal with lost acknowledgement and both cold reopens. Both checkpoint-38 Docker jobs pass offline removal. The sealed Mac manager proof verifies remote removal and scope retirement. Backend address refresh and status pass 304 node tests and strict Clippy. The integrated web editor passes 119 tests and a 17-check Chrome/real-two-NodeRuntime journey: identity-preserving port change, browser-only update, bidirectional bytes, cold retained route and file-preserving revocation. Android address entry is integrated. The native Mac sheet passes 171 Swift tests, full production compilation and signed synthetic-HTTP UI execution; actual native address/transfer journeys remain open. |
| 18 | Shipped dependencies, notices and security gates are complete | Open | Mac and both Linux targets include deterministic corresponding-source archives for Syncthing and four compiled MPL dependencies. Checkpoint 38 validates both Android ABI provenance, Rust audit/license checks, dependency review, both CodeQL languages/policy and release versions. Exact Docker scans now pass report binding and the bounded private-data gate, then identify one High and three Medium findings per image. The High is CVE-2026-84304 in Caddy gRPC 1.82.1. The reviewed 1.83.1 update passes Go module verification, consumer compilation/vet, host Caddyfile validation, exact host/Linux metadata and packaging guards. Both rebuilt-image rescans, the three Medium classifications and final multi-target dependency classification remain required. No exception is added. |
| 19 | Platform installation, upgrade, accessibility and end-to-end regression gates pass | Open | Checkpoint 38 passes full Rust/contracts, Mac bundle/integration/UI, Android foundation, iOS diagnostics, dependency review, all CodeQL languages/policy and release versions. Both Docker runtime suites pass but concrete security findings fail scans; the Android device journey fails at packaged engine loading after 75 other tests pass. The corrected foundation gate reads current worktree bytes and detects an injected unstaged OpenAPI defect. All 171 combined checkpoint-39 Swift tests pass; the production Mac source compiles. The isolated busy correction has separate real sandbox lifecycle evidence. Final hosted regression, installation/upgrade and full keyboard/spoken accessibility remain open. |
| 20 | Performance is measured and optimized after stability acceptance | Open | Folder-sync benchmarks, large-folder resource bounds and measured optimization remain. |

Evidence details: [maintained-engine validation](engine-adapter-validation-2026-09-08.md).

Progress updates should report the percentage, newly verified work and the
largest remaining gate in a few short sentences. Do not infer progress from
time spent, lines changed, a queued build or an agent's implementation claim.
Update the numerator only after checking the evidence. Keep this checklist
stable; record any necessary scope change explicitly rather than quietly
changing the denominator.
