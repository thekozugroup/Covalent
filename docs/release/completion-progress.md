# Completion progress

Updated 2026-09-20. The active goal is defined in [Product requirements](../product/requirements.md#active-completion-goal).

**Current scope: 80% accepted, 8 of 10 complete acceptance checks. Rclone migration: 100%.**
These percentages count checks accepted under the owner's current scope, not
remaining development time. On 2026-09-20 the owner set this release threshold:
"as long as we know the apps work on each device and have tested at least 1 combo
of syncing, then we can assume it's fine to release."
Existing Mac, Android emulator, both Docker architecture, Atmos, and Waypoint
evidence meets that functional threshold. The owner subsequently requested a
minimal, clean, cohesive Docker/Unraid web UI. Row 9 is reopened until that
interface works and is verified; publication is on hold. No full pairing matrix or repeated exact-package
UI journey is required without a material change affecting that evidence.
The earlier 90% acceptance reflected the simplified threshold; the new web UI
requirement reduces current acceptance to 80%. Neither change claims new test passes.
Final package publication, provenance, installation guidance, and owned cleanup
remain. The project is not release-complete.

The Docker/Unraid web UI is implemented and passes local browser review, including
the token-file unlock flow, link status, Run Now, and shared timing controls.
All 74 JavaScript checks pass, with zero failures or skips. A browser-triggered
transfer between two isolated local nodes produced identical file hashes, kept
the destination-only file, and made no reverse copy. Changing the link to hourly
in the browser produced matching confirmed settings at revision 1 on both nodes.
The refreshed node serves the current assets and preserves identities and hourly
settings across restart. These are local fixture results, not proof of rollout
to Docker or Unraid. Final browser review also confirms token clearing on reload,
token-file unlock, keyboard tab navigation, and no overflow at 1280- and 390-pixel
viewport widths; no browser console errors were observed. Row 9 stays open until the deployed console is reachable
through its configured WebUI address and verified there. Evidence is retained in
`artifacts/validation-2026-09-20/web-ui-qa/`; fixture cleanup remains pending.

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

After the owner unlocked the Mac and authenticated XCTest automation, the same
real-folder journey passes locally on macOS 27 at `94917d8` in 193.447 seconds,
including its contrast audit. The hosted macOS 26 findings remain historical
failures; the local pass does not change those results. They do not require a
new exhaustive UI run under the owner's current release threshold. The preceding local attempt stopped before
any app test because XCTest required authentication. Both attempts' temporary
fixtures and processes are cleaned. Evidence is retained under
`artifacts/validation-2026-09-20/mac-current-rendered-real-folder-auth/`.

The ineffective scroll-edge modifier is removed. Mac failure labels now say
"Last run incomplete" without claiming 24 hours elapsed. Android no longer
offers Run Now for Continuous links, which reject explicit run requests.
Affected Android UI compilation, all 196 existing JVM tests, and the current
hosted Android journey pass. Existing Mac operation evidence is accepted under
the owner's reduced threshold; any remaining Mac interaction uses computer use.

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
The final APK still needs package identity and release provenance checks; the
owner no longer requires repeating its UI journey absent a material behavior
change. The physical Pixel is not used.
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
`06b12b5` package used for the network drill. The latest local full run passed
five of six checks, including the real-folder journey in 193.775 seconds; its
first-launch check failed while querying an accessibility snapshot. This failure
is retained, not counted as a pass. Existing operational evidence is accepted
under the reduced threshold. Remaining Mac interaction uses computer use instead
of XCTest. VoiceOver is excluded by owner instruction.

Computer use of the current personal Mac app then showed Ready status, completed
pairing, readable deletion explanations, keyboard selection of Manual cadence,
and a native folder grant; the receiver accepted that link with matching settings.
UI capture subsequently timed out. No new transfer, relaunch, or minimum-window
resize is credited. Existing completed native and server transfer evidence meets
the owner's reduced threshold. The owned app process, managed state, new Keychain
entry, and responder fixture were removed. The compact receipt is
`artifacts/validation-2026-09-20/mac-manual-cua-result.json`.

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
| 7 | The native Mac app works, with native HIG controls, links, settings, and menu bar status; reuse completed keyboard and accessibility evidence. | Accepted under the owner's reduced release threshold; historical hosted contrast and snapshot failures retained; no VoiceOver or further XCTest |
| 8 | Tomato-inspired Android floating actions, typography, data/status visuals, and top bar; actual pairing, permission, transfer, and background journeys pass. | Verified: current 83-test native acceptance |
| 9 | Apps work on Mac, Android, and Docker/Unraid, with at least one tested sync combination and a minimal, clean, cohesive server web UI. | Open: web UI and 74 checks pass locally; reachable Docker/Unraid rollout remains; existing platform and transfer evidence retained |
| 10 | Relevant fault/security checks pass; idle/transfer performance is measured; temporary fixtures are removed; GitHub releases and installation guidance are published. | Open |

Publication still requires relevant final-source/package checks, remaining owned
cleanup, a verified release commit, a signed annotated tag, and final packages.
The approved signing-only key is configured and registered; the actual release
commit and tag still require verification. Personal ad-hoc Apple
Silicon and Android debug signing are accepted; Developer ID/notarization and a
production Android key are outside this release scope. PR 32 remains a draft.
The release can use the fully checked signed branch commit; the release workflows
do not require a merge to `main`. Preserve required checks and repository protections. See the
[validation matrix](validation-matrix.md).

Historical scores, failures, diagnosis, and cleanup receipts remain in Git history
and retained validation evidence. The 80% acceptance records existing evidence
against the owner's new threshold; it does not claim a new test pass or completed
publication. Material changes still require relevant verification.
