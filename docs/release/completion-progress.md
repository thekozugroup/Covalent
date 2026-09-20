# Completion progress

Updated 2026-09-20. The active goal is defined in [Product requirements](../product/requirements.md#active-completion-goal).

**Current scope: 80% verified, 8 of 10 complete acceptance checks. Rclone migration: 100%.**
These percentages count accepted user journeys, not remaining development time.
The project is not release-complete.

Rclone is the sole transfer engine. Native setup, pairing, one-way transfer,
fan-out, independent collection contributors, deletion choices, explicit
restoration, cadence, and link-wide settings have working validation evidence.
The Android app's native pairing, permission, transfer, and background journeys
pass. Earlier bidirectional and multi-provider backup requirements are superseded.

Current CI tests `6d28e5e`:
[CI 35489446586](https://github.com/thekozugroup/Covalent/actions/runs/35489446586).
Rust/contracts, both Docker architecture builds and vulnerability/runtime checks,
Android foundation and API 37 device checks, Mac packaging, and dependency review
pass. Android passes 82 baseline tests and the full SAF journey, including the
source-deletion step that previously timed out. Hosted Mac UI passes five checks;
its real-folder journey reaches the rendered audit and finds two contrast failures
near the bottom of the Links form. The bounded fix hides that form's bottom
scroll-edge fade on macOS 26 and later. Pinned compilation passes; the local UI
runner stops because the login session is locked. Hosted verification remains open.

The retained Android candidate uses `06b12b5`, is 49,344,953 bytes, and has APK
SHA-256 `8f9257350ed8c35b78f4601903edfd43b19cb853ac2498cda67bc37b12804eae`.
Package, native libraries, signer, notices, and provenance checks pass. The prior
`25971fe` APK passes its full SAF journey on a private emulator in 380.332 seconds.
Final tagged-APK validation is still required. The physical Pixel is not used.

The retained local Mac six-check run passes in 264.9 seconds, including native
pairing, two transfers, relaunch, saved folder grants, link settings, menu actions,
keyboard navigation, and rendered accessibility audits. Its source tree is
`0efc1b3ab875a323d3fa4f290b6f8933d337582d`. The `06b12b5` personal Apple Silicon
archive passes architecture, signature, sandbox inheritance, manifest, and component
hash checks. The final package must include the scroll-edge correction. VoiceOver
is excluded by owner instruction; both default and hosted checks omit it.

The matching `06b12b5` packaged helpers receive three real Atmos files totaling
3,748,005 bytes, with exact path, size, and SHA-256 equality. Both processes exit
normally. All temporary copies, helpers, tokens, and source fixtures are removed;
source hashes/metadata and unrelated services stay unchanged. This drill uses
copied helpers without app entitlements; native sandbox integration is covered by
the separate real-folder app journey and bundle verification.

All 7,344 music files, totaling 15,389,291,455 bytes, reached Waypoint's E-Music
share. Full engine verification and independent path, size, and sampled hash checks
pass. Hourly cadence is confirmed on both devices, and a scheduled run starts
4.831 seconds after its due time. Source metadata and unrelated services remain
unchanged. Final runtime upgrades and their performance comparison are pending.

The current real-worker failure test passes and preserves recovery state, source
files, and unrelated destination files. Completed job errors now produce a failed
run instead of silently restarting. Unique temporary file lists are removed on
success, error, or cancellation. The real Continuous source-deletion regression
passes in 143.06 seconds, including deletion during an active copy. Existing
interrupted-copy checks remain valid. Ambiguous published copies require explicit
recovery before retrying; ownership is not guessed.

A warm local sample copies 101 files totaling 67,518,464 bytes in 35.30 seconds.
During 29.985 seconds of Manual/Scheduled idle time, no worker or guardian is
observed and sampled CPU time is 0.01 seconds. This is not whole-app battery evidence.
The real music library exposes a separate cost: an unchanged run takes 853.421
seconds and reads 42,889,273,344 bytes from Atmos. Destination metadata remains
unchanged and idle CPU samples are 0.12% and 0.23%. The redundant second source hash
inventory is removed; destination comparison and post-copy integrity checks remain.
Forty-seven service tests and the real fan-out/restart check pass. A deployed
speedup is not yet claimed.

Superseded Mac candidates and an old build cache with an observed 5,671,329,792-byte
footprint are removed. This is a measured file footprint, not guaranteed physical
blocks freed. Completed worker and cross-host fixtures are also removed. Current
packages, reusable tools, the private Android emulator/ADB, and warm build caches
remain only while needed for final acceptance. Protected historical source and
validation evidence remain intact.

Atmos access is restored. Follow the [isolated Atmos drill](atmos-one-way-drill.md)
without disrupting other server resources. Atlas remains offline. Docker is the accepted Unraid target;
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
| 7 | Mac setup, links, settings, and per-link menu bar status pass native HIG, keyboard, and accessibility checks. | Verified; VoiceOver excluded by owner |
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
