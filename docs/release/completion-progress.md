# Completion progress

Updated 2026-09-19. The active goal is defined in [Product requirements](../product/requirements.md#active-completion-goal).

**Current scope: 70% verified, 7 of 10 complete acceptance checks. Rclone migration: 100%.**
These percentages count accepted user journeys, not remaining development time.
The project is not release-complete.

Rclone is the sole transfer engine. Native setup, pairing, one-way transfer,
fan-out, independent collection contributors, deletion choices, explicit
restoration, cadence, and link-wide settings have working validation evidence.
The Android app's native pairing, permission, transfer, and background journeys
pass. Earlier bidirectional and multi-provider backup requirements are superseded.

The latest completed CI run tests the same source tree as `8b01377`:
[CI 34792839553](https://github.com/thekozugroup/Covalent/actions/runs/34792839553).
Seven direct jobs pass: Rust/contracts, Android foundation and device checks,
Mac packaging, both Docker architectures, and dependency review. CodeQL and
version checks also pass. Mac UI and its aggregate software gate fail.

The personal Android debug APK from `dedea61` is built: 49,341,349 bytes,
package `life.michaelwong.covalent`, version `0.2.0`. Device validation awaits
an authorized Android device; the existing ADB server reports no devices.

The `dedea61` local Mac result is historical: three tests pass and four fail.
VoiceOver is excluded from future test targets by owner override. The three
remaining in-scope failures are menu-bar visibility, local-network prompt
handling, and real-folder pairing. Native accessibility, keyboard, and HIG
requirements remain active. Default and `--hosted` now select the same six
functional/native-accessibility checks; no VoiceOver verification is claimed.

Atmos old owned cleanup is complete. Waypoint authorization permits hourly
E-Music transfer on the real Unraid target, but no transfer has occurred yet.

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
are removed. A further 191,115,811 bytes of disposable Mac build files are removed;
the retained inspection app is unchanged and still passes strict signature checks.
Reusable toolchains, the temporary Mac inspection build, and tested
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
| 7 | Mac setup, links, settings, and per-link menu bar status pass native HIG, keyboard, and accessibility checks. | Open |
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
