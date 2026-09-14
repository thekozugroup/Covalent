# Completion progress

Updated 2026-09-14. The active goal is defined in [Product requirements](../product/requirements.md#active-completion-goal).

**Current scope: 70% verified, 7 of 10 complete acceptance checks. Rclone migration: 100%.**
These percentages count accepted user journeys, not remaining development time.
The project is not release-complete.

Rclone is the sole transfer engine. Native setup, pairing, one-way transfer,
fan-out, independent collection contributors, deletion choices, explicit
restoration, cadence, and link-wide settings have working validation evidence.
The Android app's native pairing, permission, transfer, and background journeys
pass. Earlier bidirectional and multi-provider backup requirements are superseded.

The latest completed CI run tests the same source tree as `2bde4a9`:
[CI 34789989171](https://github.com/thekozugroup/Covalent/actions/runs/34789989171).
Seven direct jobs pass: Rust/contracts, Android foundation and device checks,
Mac packaging, both Docker architectures, and dependency review. CodeQL and
version checks also pass. Mac UI and its aggregate software gate fail.

Android passes all 82 baseline device tests in 85.262 seconds and the unchanged
full SAF journey in 424.523 seconds: 83 tests, zero failures or skips. Its worker,
guardian, picker fixture, second-node data, and private credential cleanup checks
pass. The hosted emulator stops; the runner reaps one remaining ADB process.
The log does not enumerate the trap-driven container cleanup afterward.

The Mac suite passes five of seven tests with no skips. All four native popup
selections and functional transfer, relaunch, menu, and status phases complete.
The real-folder test finishes in 222.205 seconds, inside its unchanged 240-second
allowance. Six contrast occurrences across five strings remain. Hosted VoiceOver
fails its system keyboard-access preflight before speech or native action.

Local Mac builds and strict ad-hoc signature checks pass after fixing physical
`/private/tmp` path comparisons and explicit personal signing settings. XCTest
then times out enabling automation mode before any product test runs. A separate
Debug app builds in 22.344 seconds, but direct inspection is blocked while the
Mac is locked. Unlock and developer-permission replies remain pending; neither
permission nor successful UI testing is assumed.

The harness keeps the full seven-test suite as its default. Its explicit hosted
mode selects the six functional/accessibility tests supported by GitHub's runner.
All test bodies, assertions, and time limits remain unchanged. The new selection
still needs hosted execution. Before publication, the exact release SHA must pass
the full local suite, including actual VoiceOver speech and native action, plus
the defined HIG and keyboard checks. Green hosted CI alone cannot complete this
goal. This is a documented operator gate, like the isolated Atmos drill.

The real Continuous source-deletion test passes in 148.03 seconds. It deletes a
source file while the same rclone process is copying, without stopping or
reconfiguring the link. The destination copy disappears within 120 seconds;
remaining files are exact, unrelated destination files stay intact, and a newer
generation succeeds on both devices. The interrupted-copy regression passes in
57.38 seconds; stopping the receiver reaps its worker in 105 milliseconds.

Ambiguous interrupted copies fail visibly. If ownership of a published copy
cannot be confirmed, recovery requires restoring the source file or removing
only that uncertain copy before retrying. The historical `ec49a3d` Android
deletion timeout remains recorded; its unrecorded internal cause is not another
gate after the reviewed current-source deletion and interruption checks pass.

The current performance sample copies 101 files, totaling 67,518,464 bytes,
correctly in 35.30 seconds. During 29.985 seconds of Manual/Scheduled idle time,
no rclone worker or guardian is observed; sampled CPU time is 0.01 seconds.
This is one warm macOS loopback sample including control/status delivery, not
whole-app battery evidence. It establishes neither a new speedup nor a regression.
No additional performance rerun is currently justified.

Completed transfer fixtures, private test helpers, and the performance checkout
are removed. Reusable toolchains, the temporary Mac inspection build, and tested
candidate/evidence files remain while needed. The retained personal Android
candidate from `6499ded` passed its full SAF journey in 394.998 seconds; its
emulator, private ADB, worktree, and build files are removed. Final-source Mac,
Android, and Docker candidates still need acceptance.

Atmos validation and its remaining owned cleanup await SSH reauthentication.
Follow the [isolated Atmos drill](atmos-one-way-drill.md); do not disrupt other
server resources. Atlas remains offline. Docker is the accepted Unraid target;
a plugin is required only for a demonstrated Docker limitation. No physical
Atlas validation is claimed.

| # | Acceptance check | Status |
| --- | --- | --- |
| 1 | Native setup pairs devices and creates a source/destination link; files never flow backward. | Verified |
| 2 | Fan-out destinations work independently; multiple sources contribute safely to one collection. | Verified |
| 3 | Both source-deletion options work with clear explanations and failed-scan protection. | Verified; ambiguous interrupted copies require explicit recovery |
| 4 | Destination deletions stay local across source edits/restarts; explicit restoration works. | Verified |
| 5 | Manual, scheduled, and continuous transfers respect platform limits; idle batch workers stop. | Verified |
| 6 | Settings edited on any authorized member converge across the link; pending/stale edits remain visible. | Verified |
| 7 | Mac setup, links, settings, and per-link menu bar status pass native HIG/keyboard/VoiceOver checks. | Open |
| 8 | Tomato-inspired Android floating actions, typography, data/status visuals, and top bar; actual pairing, permission, transfer, and background journeys pass. | Verified |
| 9 | Installable Mac/Android/Docker candidates pass real laptop–Atmos transfers and server mount handling; use an Unraid plugin only for a demonstrated Docker limitation. | Open |
| 10 | Relevant fault/security checks pass; idle/transfer performance is measured; temporary fixtures are removed; GitHub releases and installation guidance are published. | Open |

Publication still requires final-source checks, remaining owned cleanup, a verified
release commit, a signed annotated tag, and final packages. Personal ad-hoc Apple
Silicon and Android debug signing are accepted; Developer ID/notarization and a
production Android key are outside this release scope. PR 32 remains a draft;
merging is not authorized. See the [validation matrix](validation-matrix.md).

Historical scores, diagnosis, and cleanup receipts remain in Git history and the
retained validation evidence. Queued builds, source inspection, and unverified
claims cannot complete an acceptance check.
