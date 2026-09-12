# Completion progress

Updated: 2026-09-12. Checkpoint 45, commit
`dd849925f22190badf321561525c298a8f9805d0`, passes Rust/contracts, Mac bundle,
Mac integration/UI, iOS, Android foundation, arm64 Docker, dependency review,
CodeQL and release versions. Its Android installed folder journey exposes
three real platform defects: the executable verifier rejects Android installer
UID 1000, root browsing rejects an empty relative component, and worker launch
requires an environment variable absent from Android app processes.

The checkpoint 46 candidate fixes those causes while preserving executable
hash/path/mode checks, private-state ownership and storage boundaries. Its full
local Android build passes 167 JVM tests, lint and package contracts. The exact
installed ARM64 API 37 production APK passes the three-phase native folder
journey: pairing, two-way transfer, pause/resume, changed peer and worker ports,
cold restart, permission loss, permission restoration and file-preserving
removal. Permission loss stops both workers. Test-only corrections fix singular
instrumentation-result parsing and fixture lookup after permission revocation.
One earlier address-change timeout is retained; two subsequent setup runs pass.
The full current hosted baseline and package gates still require execution.

Checkpoint 45 AMD64 Docker passes strict source evidence, packaged sync and
multi-node Compose tests. Its size is 142,051,328 bytes against the unchanged
134,217,728-byte cap. Arm64 passes at 133,095,936 bytes. The scanner reports
zero High and three Medium BusyBox CVE-2025-60876 rows for the locally patched
revision. Arm64 source applicability is reviewed; final image-bound review and
independent clean-builder payload comparison remain required. Findings are
retained without suppression.

The Docker candidate selects only Caddy modules used by the shipped TLS proxy,
headers, compression and runtime configuration, saving 7,315,456 executable
bytes on AMD64. It also removes superseded Alpine package layers while copying
the complete patched runtime filesystem and preserving runtime metadata.
Caddy's SSH dependency is pinned to its fixed version. Both Linux target
source/notice verifiers and eight native TLS/proxy/compression checks pass;
source analysis reports zero called or imported-package vulnerabilities and
retains one unused OpenPGP module finding. Both complete final images still
require all acceptance gates, including the unchanged size limit.

**75% of acceptance milestones are verified: 15 of 20.** This is a milestone
count, not an estimate of elapsed time or remaining effort. Independent review verifies Android folder recovery and the complete peer-connectivity journey from combined live evidence. Docker image acceptance remains open until both final images pass. Each milestone has
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
| 14 | The complete Android folder journey and foreground lifecycle work | Verified | The corrected exact local ARM64 API 37 production APK passes setup, denied-access and restored-access phases, including native pairing, both transfer directions, pause/resume, moved control/worker addresses, cold restart, worker reaping, resumed transfer and file-preserving removal. The full local build passes 167 JVM tests, lint and eight-object package evidence. One earlier address timeout remains recorded alongside two later setup passes. The full current hosted baseline and broader packaged-device gate remain under milestone 19. |
| 15 | Both complete Docker architectures install and run with safe writable sync mounts | Open | Checkpoint 45 arm64 passes strict source evidence, packaged sync and its size cap. AMD64 passes strict source evidence, packaged sync and full multi-node Compose, but exceeds the unchanged image budget by 7,833,600 bytes. Both final images must pass all gates after the size correction. |
| 16 | The server console offers an intuitive folder setup and management journey | Verified | Chrome drove two real NodeRuntime instances through offer, accept, bidirectional transfer, pause/resume and explicit removal. Typed paths and focus survive polling; cancel preserves sharing; removal preserves files and blocks later transfer. Both runtimes and test fixtures were cleaned. |
| 17 | Peer connectivity, invitation expiry/renewal, removal and address changes have complete user journeys | Verified | Real worker proofs cover disconnection/reconnection, two-folder continuity, invitation renewal with lost-response retry and cold reopen, and signed removal with lost acknowledgement and both cold reopens. Checkpoint 39 Docker jobs pass offline removal. The sealed Mac manager proof verifies remote removal and scope retirement. Backend address refresh/status pass 304 node tests and strict Clippy. The web editor passes 119 tests and a 17-check Chrome/real-two-NodeRuntime journey. A signed native Mac address journey now passes eleven real-runtime checks: UI-only candidate submission, signed identity verification, route replacement, bidirectional bytes, cold retained routing, file-preserving revocation and cleanup. Its 23 production Swift files match checkpoint 39. This harness does not establish LocalNodeManager containment or upgrade. The exact local Android API 37 native journey now passes moved control and worker ports, both transfer directions and cold retained routing. Independent review combines these native journeys with the earlier connectivity, expiry/renewal and authenticated-removal evidence to verify this milestone. Full platform regression remains under milestone 19. |
| 18 | Shipped dependencies, notices and security gates are complete | Open | Checkpoint 45 passes dependency review, Rust audit/license checks, CodeQL and release versions. Both Docker architectures verify strict Caddy/Alpine source evidence. Scanning retains three Medium BusyBox local-backport rows and zero High; arm64 applicability is reviewed, with final image-bound review and clean-builder reproducibility still open. The Android candidate verifies all eight packaged native objects, six NDK link records, pinned AndroidX AAR evidence and recipient notices; current hosted verification remains open. |
| 19 | Platform installation, upgrade, accessibility and end-to-end regression gates pass | Open | Checkpoint 45 passes Rust/contracts, Mac bundle, Mac integration/UI, iOS, Android foundation, arm64 Docker, dependency review, CodeQL and release versions. Its Android runtime defects are corrected and the exact local ARM64 API 37 recovery journey passes. Current full hosted device execution, both final image gates, keyboard/spoken accessibility and personal-build upgrade remain open. |
| 20 | Performance is measured and optimized after stability acceptance | Open | Folder-sync benchmarks, large-folder resource bounds and measured optimization remain. |

Evidence details: [maintained-engine validation](engine-adapter-validation-2026-09-08.md).

Progress updates should report the percentage, newly verified work and the
largest remaining gate in a few short sentences. Do not infer progress from
time spent, lines changed, a queued build or an agent's implementation claim.
Update the numerator only after checking the evidence. Keep this checklist
stable; record any necessary scope change explicitly rather than quietly
changing the denominator.
