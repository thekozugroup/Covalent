# Completion progress

Updated: 2026-09-12. Checkpoint 42, commit
`8c380bf3ef4ecdd2eadf5b16680f0cbdbbb3d4c4`, has terminal hosted results.
Android compilation succeeds, but package verification reports a required entry
that is missing or outside its byte bounds. Zero instrumentation tests run and
the aggregate release gate fails; all other main CI jobs pass. Checkpoint 43
adds the exact entry name and declared size to that diagnostic. It preserves
all package requirements and limits; the exact packaging correction still
requires evidence from its hosted run.
Checkpoint 43 also integrates the reviewed Caddy/Alpine source evidence and
BusyBox security backport. Local foundation and native arm64 intermediate
runtime checks pass; both complete patched Docker images still need validation.

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
| 14 | The complete Android folder journey and foreground lifecycle work | Open | Checkpoint 40 passes 75 baseline API 37 tests but fails native network pairing. Checkpoint 41 corrects the test endpoint and adds native address-change coverage. Both APK variants compile and assemble, then the new collector rejects a native object that differs from its final-link evidence. No prebuilt stamp is written and zero checkpoint 41 instrumentation tests run. Package equality and the complete setup, transfer, repair, restart and removal journey must pass before acceptance. |
| 15 | Both complete Docker architectures install and run with safe writable sync mounts | Verified | Both complete Docker architectures pass package contracts, evidence extraction and all nine hardened-runtime checks in checkpoints 36 through 41: signed pairing, dual consent, full scan, transfer, pause/resume, retained identity on cold restart, offline withdrawal, signed remote delivery/acknowledgement, recipient restart, preserved files and exact-resource cleanup. Atlas uses this accepted Docker path. Vulnerability scans remain a separate open gate. |
| 16 | The server console offers an intuitive folder setup and management journey | Verified | Chrome drove two real NodeRuntime instances through offer, accept, bidirectional transfer, pause/resume and explicit removal. Typed paths and focus survive polling; cancel preserves sharing; removal preserves files and blocks later transfer. Both runtimes and test fixtures were cleaned. |
| 17 | Peer connectivity, invitation expiry/renewal, removal and address changes have complete user journeys | Open | Real worker proofs cover disconnection/reconnection, two-folder continuity, invitation renewal with lost-response retry and cold reopen, and signed removal with lost acknowledgement and both cold reopens. Checkpoint 39 Docker jobs pass offline removal. The sealed Mac manager proof verifies remote removal and scope retirement. Backend address refresh/status pass 304 node tests and strict Clippy. The web editor passes 119 tests and a 17-check Chrome/real-two-NodeRuntime journey. A signed native Mac address journey now passes eleven real-runtime checks: UI-only candidate submission, signed identity verification, route replacement, bidirectional bytes, cold retained routing, file-preserving revocation and cleanup. Its 23 production Swift files match checkpoint 39. This harness does not establish LocalNodeManager containment or upgrade. Android native address/transfer execution remains open. |
| 18 | Shipped dependencies, notices and security gates are complete | Open | Checkpoint 41 passes dependency review, Rust audit/license checks, both CodeQL languages, the zero-open-alert policy and release versions. Both Docker scans report zero High and three Medium CVE-2025-60876 findings in BusyBox 1.37.0-r30 packages. The unpublished main Dockerfile's patched runtime target passes a native arm64 build, all 13 wire tests, exact versions/ownership and tested-payload equality on Atmos; it is not a final image or amd64 proof. Caddy/Alpine final-image evidence and target classification remain open. Checkpoint 41's six-object Android distribution collector fails package equality, so hosted aggregation is incomplete. No finding is waived. |
| 19 | Platform installation, upgrade, accessibility and end-to-end regression gates pass | Open | Checkpoint 41 passes Rust/contracts, Mac bundle/integration/UI, iOS, both Docker runtime/image-scan jobs, dependency review, both CodeQL languages, zero-open-alert policy and release versions. Both Android jobs fail package verification before instrumentation, and the aggregate release gate fails. Full keyboard/spoken accessibility, personal-build upgrade, installation and the corrected Android regression remain unverified. |
| 20 | Performance is measured and optimized after stability acceptance | Open | Folder-sync benchmarks, large-folder resource bounds and measured optimization remain. |

Evidence details: [maintained-engine validation](engine-adapter-validation-2026-09-08.md).

Progress updates should report the percentage, newly verified work and the
largest remaining gate in a few short sentences. Do not infer progress from
time spent, lines changed, a queued build or an agent's implementation claim.
Update the numerator only after checking the evidence. Keep this checklist
stable; record any necessary scope change explicitly rather than quietly
changing the denominator.
