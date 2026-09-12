# Completion progress

Updated: 2026-09-12. Checkpoint 44, commit
`2b077b50a8bf64e9b1d85bb9794d81e69806bb62`, passes Android foundation and
package checks. The debug and release APKs preserve all six native link records
and all 117 manifest-declared notice/source files. API 37 passes 75 baseline
tests, then its full folder journey times out displaying the pairing completion
message after the backend handshake completes. This supplies no new acceptance
credit. Checkpoint 45 corrects the test scrolling and adds bounded diagnostics;
full device execution must confirm the diagnosis.

The arm64 Docker job passes strict final source evidence, all nine packaged
sync checks and the size cap at 133,095,936 bytes. Its scanner still reports
three Medium BusyBox CVE-2025-60876 findings for the locally patched revision;
applicability review remains open. AMD64 passes final source evidence and
basic runtime checks but measures 142,051,328 bytes against the unchanged
134,217,728-byte cap, skipping later sync/Compose and security checks.

The Mac bundle passes. The Mac integration job exposes a concurrency-test
scheduling flaw before UI execution. The reviewed test correction preserves
all lock-safety assertions and passes the full 171-test suite three times;
production code is unchanged. The next checkpoint also places image-size
checks after functional/security checks while retaining their failure behavior.

**65% of acceptance milestones are verified: 13 of 20.** This is a milestone
count, not an estimate of elapsed time or remaining effort. Independent review reopened Docker image acceptance because the current image changed materially and has not completed its final tests. Each milestone has
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
| 14 | The complete Android folder journey and foreground lifecycle work | Open | Checkpoint 44 passes debug/release native package equality and 75 baseline API 37 tests. The folder journey reaches a complete backend handshake but times out displaying its completion message. Checkpoint 45 scrolls to the exact descendant within the oversized lazy item and adds sanitized diagnostics. Full setup, transfer, access repair, restart, removal and address-change execution remain required. |
| 15 | Both complete Docker architectures install and run with safe writable sync mounts | Open | Checkpoint 44 arm64 passes strict final source evidence, all nine packaged sync checks and its size cap. AMD64 passes final source evidence and basic runtime but exceeds the image budget by 7,833,600 bytes; later sync, Compose and scan checks are skipped. Checkpoint 45 moves the unchanged size check after functional/security verification. Both current images must complete their gates. |
| 16 | The server console offers an intuitive folder setup and management journey | Verified | Chrome drove two real NodeRuntime instances through offer, accept, bidirectional transfer, pause/resume and explicit removal. Typed paths and focus survive polling; cancel preserves sharing; removal preserves files and blocks later transfer. Both runtimes and test fixtures were cleaned. |
| 17 | Peer connectivity, invitation expiry/renewal, removal and address changes have complete user journeys | Open | Real worker proofs cover disconnection/reconnection, two-folder continuity, invitation renewal with lost-response retry and cold reopen, and signed removal with lost acknowledgement and both cold reopens. Checkpoint 39 Docker jobs pass offline removal. The sealed Mac manager proof verifies remote removal and scope retirement. Backend address refresh/status pass 304 node tests and strict Clippy. The web editor passes 119 tests and a 17-check Chrome/real-two-NodeRuntime journey. A signed native Mac address journey now passes eleven real-runtime checks: UI-only candidate submission, signed identity verification, route replacement, bidirectional bytes, cold retained routing, file-preserving revocation and cleanup. Its 23 production Swift files match checkpoint 39. This harness does not establish LocalNodeManager containment or upgrade. Android native address/transfer execution remains open. |
| 18 | Shipped dependencies, notices and security gates are complete | Open | Checkpoint 44 passes dependency review, Rust audit/license checks, both CodeQL languages and release versions. Both final Docker architectures verify exact Caddy/Alpine source evidence. Arm64 Grype reports zero High and three Medium CVE-2025-60876 findings for patched BusyBox 1.37.0-r1000; backport applicability review remains open and nothing is waived. AMD64 scanning is skipped after its size failure. Both Android APKs now verify all six native records and 117 notice/source files; final distribution review remains open. |
| 19 | Platform installation, upgrade, accessibility and end-to-end regression gates pass | Open | Checkpoint 44 passes Rust/contracts, Mac bundle, iOS, Android foundation, arm64 Docker, dependency review, CodeQL and release versions. Mac integration fails a test scheduling fixture; its correction passes 171 tests three times locally. Android device execution fails after 75 baseline tests. AMD64 exceeds its size cap. Full keyboard/spoken accessibility, personal-build upgrade, installation and corrected end-to-end regression remain unverified. |
| 20 | Performance is measured and optimized after stability acceptance | Open | Folder-sync benchmarks, large-folder resource bounds and measured optimization remain. |

Evidence details: [maintained-engine validation](engine-adapter-validation-2026-09-08.md).

Progress updates should report the percentage, newly verified work and the
largest remaining gate in a few short sentences. Do not infer progress from
time spent, lines changed, a queued build or an agent's implementation claim.
Update the numerator only after checking the evidence. Keep this checklist
stable; record any necessary scope change explicitly rather than quietly
changing the denominator.
