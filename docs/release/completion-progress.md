# Completion progress

Updated 2026-09-20. The active goal is defined in [Product requirements](../product/requirements.md#active-completion-goal).

**Current scope: 90% accepted, 9 of 10 complete acceptance checks. Rclone migration: 100%.**
These percentages count accepted checks, not remaining development time. The
owner's 2026-09-20 threshold accepts working apps on each platform and at least
one tested sync combination. Existing native Mac, Android emulator, both Docker
architectures, and real Atmos-to-Waypoint and Atmos-to-Mac evidence meet that
threshold. The subsequently requested Docker/Unraid web UI is now verified on
both live servers. Row 9 is complete; final publication, installation guidance, and remaining owned cleanup keep row 10 open.
The project is not release-complete.

The web UI passes 74 local JavaScript checks and browser acceptance covering
token-file unlock, pairing/link controls, Run Now, shared timing, keyboard tabs,
reload token clearing, and 1280- and 390-pixel layouts without console errors.
A browser-triggered transfer between isolated local nodes produced identical
file hashes, preserved a destination-only file, and made no reverse copy.
Browser changes to hourly timing converged at confirmed revision 1 on both nodes;
identities and settings survived restart. Those fixtures are now cleaned:
`artifacts/validation-2026-09-20/web-ui-qa/cleanup.json`.

Computer-use acceptance on both deployed consoles confirmed trusted HTTPS,
token-file unlock, readable link/status/settings, Scheduled 60-minute timing,
source-confirmed settings, and both deletion options disabled. No console errors
were observed. Atmos reports completion for every destination; Waypoint reports
completion and explains that the source owns the schedule. The live UI check
changed no production settings and triggered no transfer. Both private HTTPS URLs work. Their addresses remain in the local deployment
receipt rather than public documentation. Evidence:
`artifacts/validation-2026-09-20/web-ui-deployment/live-browser-acceptance.json`.

Both servers run the exact signed v0.2.1 images from `e7943db`.
Rollout `e54cb2d8f6c03cfae9314a5d` preserves device identities, folders, hourly
settings, deletion policy, resource limits, secrets, and unrelated services.
Container release workflow 35518301009 passed; index and platform signatures,
exact SPDX predicates, and released checksums were independently verified.
The image index is `sha256:393f8a0dafa7f17d8ad964d501f3d33668d547889dc493080e042489ee3e1677`.
Both signed platform image IDs match the deployed images. Stored image references
and Waypoint's template were updated without replacing or restarting containers.
Evidence: `artifacts/validation-2026-09-20/web-ui-deployment/signed-promotion-393f8a0dafa7f17d/attempt-2/result.json`.
Original containers and private rollback backups remain until final cleanup.

Rclone is the sole transfer engine. Native setup, pairing, one-way transfer,
fan-out, independent collection contributors, deletion choices, explicit
restoration, cadence, and link-wide settings have working validation evidence.
The Android app's native pairing, permission, transfer, and background journeys
pass the latest hosted run. An earlier intermittent deletion-propagation failure
remains unexplained. Earlier bidirectional and multi-provider backup requirements are superseded.

Current release-source [CI 35513847025](https://github.com/thekozugroup/Covalent/actions/runs/35513847025)
passes all required checks. Its PR merge commit `93e7b19` has the same Git tree
`dd972b6c89b0892101a433cf520159015f5d2c61` as release commit `e7943db`.
Android API 37 x86_64 emulator acceptance passes 82 baseline tests in 71.402
seconds and its SAF journey in 401.185 seconds: 83 tests total. This is hosted
emulator evidence, not physical Android or exact-personal-APK execution.
Current package and acceptance receipts are retained under
`artifacts/validation-2026-09-20/android-final-e7943db/`.

Historical failures remain disclosed. The `91979b7` intermittent hosted SAF
failure remains unexplained. At `e40616a`, the Mac real-folder journey completed
its transfers and other actions in 217.066 seconds but reported two contrast
findings; five of six checks passed, without a timeout. After local automation
authentication, the real-folder journey at `94917d8` passed in 193.447 seconds,
including contrast. A preceding local attempt stopped before any app test because
automation authentication was required. A later full local suite passed five of
six checks, with a first-launch accessibility-snapshot failure; its real-folder
journey passed. These results are retained, not rewritten as passes. Existing
Mac operation meets the owner's reduced threshold. Remaining interaction uses
computer use, with no further XCTest or VoiceOver.

The ineffective scroll-edge modifier is removed. Mac failure labels now say
"Last run incomplete" without claiming 24 hours elapsed. Android no longer
offers Run Now for Continuous links, which reject explicit run requests.
Affected Android UI compilation, all 196 existing JVM tests, and the current
hosted Android journey pass. Existing Mac operation evidence is accepted under
the owner's reduced threshold; any remaining Mac interaction uses computer use.

Commit `947ab21` used semantic primary text for the two Form labels; that change
did not resolve the historical hosted contrast findings. Its Android deletion test now
waits for a successful acknowledged seed before testing source deletion, and
passes hosted acceptance. Existing independent active-copy deletion, ownership,
recovery, and failed-scan checks remain valid against the unchanged transfer
runtime. Historical failure causation is not an additional acceptance requirement;
current behavior and safety evidence remain required. Local commit `74ff315`
corrects recovery guidance to say "before the next run" for Continuous links.
VoiceOver remains excluded by the owner's repeated instruction.

Final personal v0.2.1 packages are built from the clean, verified signed release
commit `e7943db3d5785294bdbb389cc2f52142da860c87`. The signed annotated `v0.2.1`
tag is verified. The Android APK is 49,352,761 bytes, with SHA-256
`7157b555a2c9f684aa6c8f06a0ec94079cfe614f633065b4189c423e5cbadf6b`.
Its preserved debug signer, package/version, both native ABIs, alignment,
source-built hashes, manifests, SBOM, and notices pass the existing builder checks.
The acceptance companion distinguishes the hosted tested APK from this personal
APK and applies the owner's explicit reuse threshold without claiming a new
device run. The physical Pixel was not used.

[Mac release workflow 35515574434](https://github.com/thekozugroup/Covalent/actions/runs/35515574434)
builds the v0.2.1 Apple Silicon archive from the same release commit. ZIP SHA-256:
`11612a50b7d6c901f538c974cd5cf5fd1d5d8cd306171e5d46c7549467b7fa2b`.
Downloaded draft assets match their GitHub asset digests. Local package inspection
passes ARM64, strict ad-hoc signature, sandbox inheritance, engine and notice
manifest, and dependency inventory checks. This package is not Developer ID signed
or notarized, and was not launched for this inspection. Receipt:
`artifacts/validation-2026-09-20/mac-final-e7943db/verified-package-receipt.json`.
Final GitHub release publication remains pending.

Earlier Android ARM64 journeys, the six-check local Mac pass, and the packaged
helper network drill remain supporting historical behavior evidence. No new
physical-device or exhaustive exact-package journey is required absent a material
behavior change. Earlier candidate hashes and detailed failure records remain in
Git history and retained validation receipts.

Earlier computer use of the personal Mac candidate showed Ready status, completed
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
unchanged. The earlier durable-node upgrade used verified `06b12b5` native images. Identities,
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

Owned local browser fixtures, private emulator/ADB resources, obsolete native
builds, and warm caches are cleaned. The final cleanup retained compact records
and recoverable source differences before removing the three remaining owned
temporary roots. Current release artifacts, the durable Android update signing
key, production identities/data, and deployment access remain protected.
Receipts: `artifacts/validation-2026-09-20/final-cache-cleanup/result.json` and
`remaining-cleanup-result.json` in that directory. Temporary release-tooling and
rollout rollback resources still need final acceptance cleanup; durable user
services are not test fixtures.

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
| 9 | Apps work on Mac, Android, and Docker/Unraid, with at least one tested sync combination and a minimal, clean, cohesive server web UI. | Verified: local functional browser checks and trusted live WebUI acceptance on both Atmos and Waypoint |
| 10 | Relevant fault/security checks pass; idle/transfer performance is measured; temporary fixtures are removed; GitHub releases and installation guidance are published. | Open |

Publication still requires the final signed container reference and provenance,
completed GitHub release assets, current installation guidance, and remaining
owned cleanup. The release source commit and annotated `v0.2.1` tag are verified;
final Android and Mac package identity checks are complete. Personal ad-hoc Apple
Silicon and Android debug signing are accepted; Developer ID/notarization and a
production Android key are outside this release scope. Release-tooling fixes must
retain source/package identity and pass their relevant checks. The release may use
the checked signed branch commit without a merge to `main`; preserve repository
protections. See the [validation matrix](validation-matrix.md).

The 90% score records completed live server-interface acceptance alongside the
previously accepted platform and behavior evidence. Row 10 remains open until
publication, install guidance, provenance, and cleanup are complete. Historical
failures and limitations remain disclosed; material changes still require
relevant verification.
