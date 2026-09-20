# Completion progress

Updated 2026-09-20. The active goal is defined in [Product requirements](../product/requirements.md#active-completion-goal).

**Current scope: 70% verified, 7 of 10 complete acceptance checks. Rclone migration: 100%.**
These percentages count accepted user journeys, not remaining development time.
The remaining checks cover Mac rendered acceptance, final packages, and release
completion. The project is not release-complete.

Rclone is the sole transfer engine. Native setup, pairing, one-way transfer,
fan-out, independent collection contributors, deletion choices, explicit
restoration, cadence, and link-wide settings have working validation evidence.
The Android app's native pairing, permission, transfer, and background journeys
pass the latest hosted run. An earlier intermittent deletion-propagation failure
remains unexplained. Earlier bidirectional and multi-provider backup requirements are superseded.

The most recently completed CI tests `e40616a`:
[CI 35494827886](https://github.com/thekozugroup/Covalent/actions/runs/35494827886).
Shared Rust/contracts, both Docker architecture builds and vulnerability/runtime
checks, Android foundation, Mac packaging, dependency review, CodeQL, and version
checks pass. Android passes all 82 baseline device tests and its revised SAF
journey in 423.033 seconds. The earlier `91979b7` source-deletion failure remains
unexplained; this pass does not establish its cause.
Hosted Mac UI passes five checks. Its real-folder journey completes both transfers,
relaunch, settings, and menu actions in 217.066 seconds, but reports contrast
findings for destination-deletion guidance and a partially clipped Transfers
label. No timeout occurs this run. No assertion is waived or timeout increased.

The ineffective scroll-edge modifier is removed. Mac failure labels now say
"Last run incomplete" without claiming 24 hours elapsed. Android no longer
offers Run Now for Continuous links, which reject explicit run requests.
Affected Android UI compilation, all 196 existing JVM tests, and the current
hosted Android journey pass. Mac rendered acceptance remains open.

Commit `947ab21` uses semantic primary text for the two Form labels; that change
does not resolve their rendered contrast findings. Its Android deletion test now
waits for a successful acknowledged seed before testing source deletion, and
passes hosted acceptance. Existing independent active-copy deletion, ownership,
recovery, and failed-scan checks remain valid against the unchanged transfer
runtime. Historical failure causation is not an additional acceptance requirement;
current behavior and safety evidence remain required. Local commit `74ff315`
corrects recovery guidance to say "before the next run" for Continuous links.
VoiceOver remains excluded by the owner's repeated instruction.

The retained Android candidate uses `06b12b5`, is 49,344,953 bytes, and has APK
SHA-256 `8f9257350ed8c35b78f4601903edfd43b19cb853ac2498cda67bc37b12804eae`.
Package, native libraries, signer, notices, and provenance checks pass. The prior
`25971fe` APK passes its full SAF journey on a private emulator in 380.332 seconds.
Final tagged-APK validation is still required. The physical Pixel is not used.
The retained `06b12b5` APK also passes the unchanged full journey on the private
arm64 emulator in 364.884 seconds, including deletion during an active run.
Only failure diagnostics were added to the test APK. This pass does not explain
the intermittent x86_64 hosted failure. The test apps, folder fixture, and private
emulator are removed or stopped afterward; the temporary test source is restored.

The retained local Mac six-check run passes in 264.9 seconds, including native
pairing, two transfers, relaunch, saved folder grants, link settings, menu actions,
keyboard navigation, and rendered accessibility audits. Its source tree is
`0efc1b3ab875a323d3fa4f290b6f8933d337582d`. The `91979b7` personal Apple Silicon
archive passes architecture, signature, sandbox inheritance, manifest, archive,
and component hash checks. The archive SHA-256 is
`f8b958cdb506cd87079cae4c0f20ffbd8ad4cc7ef0899b995c8fce9a0929541d`.
Its three engine helpers and engine manifest remain byte-identical to the
`06b12b5` package used for the network drill. Current native UI acceptance is
still required. VoiceOver is excluded by owner instruction; both default and
hosted checks omit it.

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
unchanged. Both durable nodes now run verified `06b12b5` native images. Their identities,
mounts, hourly settings, deletion policy, resource limits, files, and unrelated
services are preserved. Exactly one post-upgrade unchanged run succeeds.

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
The real music library exposes a separate cost: an unchanged run initially
takes 853.421 seconds and reads 42,889,273,344 bytes from Atmos. Removing a
redundant source hash inventory reduces the measured run to 653.374 seconds
and source disk reads to 22,461,243,392 bytes. Those samples are 23.44% shorter
and 47.63% lower in disk reads; filesystem caching also affects the comparison.
Two source hash inventories remain, so substantial hourly scanning still occurs.
No copy worker runs during this unchanged check. Source and destination metadata
stay unchanged. Transfer workers stop afterward; idle CPU samples are 0.12%
and 0.24%. Docker working memory is 28.82 MiB and 14.2 MiB, while the Atmos
cgroup also retains about 2 GiB of mostly file cache. These measurements are not
whole-app battery or universal throughput claims. Existing safety checks and
post-copy integrity verification remain intact.

Superseded Mac candidates and an old build cache with an observed 5,671,329,792-byte
footprint are removed. This is a measured file footprint, not guaranteed physical
blocks freed. Completed worker and cross-host fixtures are also removed. Current
packages, reusable tools, owned Android virtual-device files, and warm build
caches remain only while needed for final acceptance. Test apps, fixtures, the
private emulator, and its ADB server are removed or stopped. Protected historical
source and validation evidence remain intact.

Atmos access is restored. Follow the [isolated Atmos drill](atmos-one-way-drill.md)
without disrupting other server resources. Atlas remains offline. Docker is the accepted Unraid target;
a plugin is required only for a demonstrated Docker limitation. No physical
Atlas validation is claimed.

| # | Acceptance check | Status |
| --- | --- | --- |
| 1 | Native setup pairs devices and creates a source/destination link; files never flow backward. | Verified |
| 2 | Fan-out destinations work independently; multiple sources contribute safely to one collection. | Verified |
| 3 | Both source-deletion options work with clear explanations and failed-scan protection. | Verified: current SAF acceptance and unchanged-runtime safety evidence |
| 4 | Destination deletions stay local across source edits/restarts; explicit restoration works. | Verified |
| 5 | Manual, scheduled, and continuous transfers respect platform limits; idle batch workers stop. | Verified |
| 6 | Settings edited on any authorized member converge across the link; pending/stale edits remain visible. | Verified |
| 7 | Mac setup, links, settings, and per-link menu bar status pass native HIG, keyboard, and accessibility checks. | Reopened: current rendered contrast failures; VoiceOver excluded by owner |
| 8 | Tomato-inspired Android floating actions, typography, data/status visuals, and top bar; actual pairing, permission, transfer, and background journeys pass. | Verified: current 83-test native acceptance |
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
