# Completion progress

Updated 2026-09-13 after the owner reaffirmed the lightweight one-way scope. The active goal is defined in [Product requirements](../product/requirements.md#active-completion-goal).

**Current scope: 60% verified, 6 of 10 complete acceptance checks.** This counts
complete user journeys, not remaining time or reusable code. Rclone is the sole
transfer engine and its migration is complete. Setup, fan-out, collection
isolation, deletion/restoration, cadence, and shared settings remain verified.
The latest completed CI on `3301162` passes all 10 foundation, packaging,
security, and version jobs. Mac setup and pairing complete, but link creation
times out. Android passes 81 baseline tests and its initial picker, but later
folder-access repair remains pending with a stopped service and no visible
retry control. This observed recovery defect reopens Android acceptance and
reduces the score from 70%. The earlier complete local Android journey and
normal/large-text review remain valid historical executions; they do not
override this newly identified failure. Mac, Android recovery, Atmos, and
final release acceptance remain open.
Earlier Syncthing evidence is retained only as history.

**Previous scope: 75%, 15 of 20 checks**, recorded at checkpoint 46 (c47003113e28b6e934a8ab4823040614fb07fb30). That score belongs to the superseded bidirectional/backup scope. Source and evidence remain in Git history.

| # | Acceptance check | Status |
| --- | --- | --- |
| 1 | Native setup pairs devices and creates a source/destination link; files never flow backward. | Verified |
| 2 | Fan-out destinations work independently; multiple sources contribute safely to one collection. | Verified |
| 3 | Both source-deletion options work with clear explanations and failed-scan protection. | Verified |
| 4 | Destination deletions stay local across source edits/restarts; explicit restoration works. | Verified |
| 5 | Manual, scheduled, and continuous transfers respect platform limits; idle batch workers stop. | Verified |
| 6 | Settings edited on any authorized member converge across the link; pending/stale edits remain visible. | Verified |
| 7 | Mac setup, links, settings, and per-link menu bar status pass native HIG/keyboard/VoiceOver checks. | Open |
| 8 | Tomato-inspired Android floating actions, typography, data/status visuals, and top bar; actual pairing, permission, transfer, and background journeys pass. | Open: saved permission repair can stall and hide its retry action |
| 9 | Installable Mac/Android/Docker candidates pass real laptop–Atmos transfers and server mount handling; use an Unraid plugin only for a demonstrated Docker limitation. | Open |
| 10 | Relevant fault/security checks pass; idle/transfer performance is measured; temporary fixtures are removed; GitHub releases and installation guidance are published. | Open |

[Current requirements](../product/requirements.md). Atlas remains offline; Docker is the accepted Unraid target. Queued builds, source inspection, and unverified agent claims cannot complete a check.

Rclone migration: the restricted upstream v1.75.1 worker builds for macOS arm64,
Android arm64/x86_64, and Linux amd64/arm64. The macOS worker is 20.0 MB; Android
workers are 21.6 MB and 23.2 MB. Eighteen prototype policy/transport checks and
twelve packaged read-only SFTP/content checks pass, including denied source
writes, host-key verification, scoped access, receiver restart after removal,
and detection of same-size, same-mtime content corruption. The authorization
helper's boundary tests pass. Mac packaging/signing and Android worker
packaging/provenance/notices pass; these are components, not final app acceptance.
The simplified web console passes a real browser fixture journey; native Mac
sources typecheck for macOS 15. The rclone Rust runtime and Android SAF bridge are integrated in the active
branch. The full Rust workspace and strict Clippy pass on commit `607180e`.
Consolidated foundation checks, 70 web tests, and 179 Swift tests pass;
two live-service Swift checks remain skipped. Android passes the real system
folder picker and nested file-access proof. The rclone three-node fan-out journey
passes source edits, retained destination deletions, restoration after settings
changes and restart, and shared/offline/conflicting settings. The manual journey
passes empty runs, retained deletions, and restart. The collection journey passes
independent contributors, missing mounts, unreadable source scans, deletion
protection, and recovery. A real 15-minute scheduled run also passes: the source
owns the due time, no file or transfer worker appears before it, the automatic
copy completes, and both batch workers stop. A fresh manual empty-run and
deletion/restart regression passes after the no-op optimization.

Android startup registers saved folder capabilities before starting transfers
and isolates unavailable grants. The actual API 37 device passes two manual
SAF-to-SAF transfers through paired nodes, including service restart and saved
permission registration. The self-contained device journey also completes
selection in the real system folder picker with exact child-folder assertions,
Unicode transfer, revoked-access interruption, exact-folder reselection, and
recovery transfer. Real charging and Wi-Fi changes block and resume transfers.
The earlier fully passing API 37 run covers all 82 tests for PR source `6260187`:
81 baseline tests in 73.77 seconds and the complete SAF journey in 412.12 seconds.
The latter covers native pairing and source offer, Manual transfer, peer-address
editing and transfer to the moved peer, cold restart, permission recovery,
shared deletion settings, Continuous transfer, pause/resume, both deletion
policies, and safe removal. Final helper counts are zero and device framework
processes remain stable. Both Setup accessibility checks pass.
The ordinary MainActivity file-transfer
screen now reuses the floating Pair/Links toolbar. Device review passes normal
and 2× text, both navigation actions, content clearance at the end of the list,
and cold reopening. A weighted status label fixes the narrow Refresh button at
large text sizes. The visual build's UI source matches `6260187`; its reused
native helpers are independently covered by the fresh hosted native build and
complete device run. The later transfer failure temporarily reopened Android
acceptance. The corrected `d4a45ff` local journey and normal/2× MainActivity
review restored check 8 at that checkpoint. The later `3301162` recovery failure
reopens it, as recorded above.

Earlier row 5 evidence combines the complete Manual/Continuous Android journey, the complete
15-minute source-owned scheduled run, and the recorded real charging/Wi-Fi
blocking and resumption phases. The earlier conditions run remains a failed
run; only its completed phases are credited. Its later Continuous-resume failure
is independently covered by the final passing journey. The core stale-condition
check also passes. This does not promise exact Android background timing. The
corrected `d4a45ff` local journey passes the current native Manual/Continuous
path, restoring check 5 with the unchanged schedule and condition evidence.

Earlier row 1 evidence uses the completed `6260187` API 37 SAF journey: native initiating-device pairing,
system folder selection, native Manual link creation and Run Now, exact forward
bytes, and a bounded check that a destination-only file stays off the source.
The responder confirms pairing and accepts the destination through the fixture's
authenticated API. The tested rclone runtime and Android changes are recorded
above. That run also passes the added address-edit regression. The complete
`d4a45ff` local journey repeats those native setup and transfer assertions,
restoring check 1 on the current source.

Integrated source `142a6eb` passes the Rust workspace and strict
Clippy, all 70 web tests, Android foundation, the Mac app bundle, and packaged
Docker journeys on amd64 and arm64. CodeQL and release-version checks pass.
Its Android device baseline passes 81 tests, but the full SAF journey times
out at its first Manual destination-byte wait. The failure occurs before the
first completed generation or deletion checks. A fresh local build with the same
native runtime reproduces the failure with both nodes idle at generation zero.
A device screenshot and measured bounds show that the floating Pair/Links toolbar
covers the Run Now button; the original physical tap saves no run request.
The list now reserves the toolbar's measured height below its viewport. Local
foundation checks pass, and the complete unchanged SAF journey passes in
399.70 seconds with ordinary taps. Its test source matches `7cf89c1`, its UI
matches `d4a45ff`, and its fresh native runtime matches `142a6eb`. Normal
MainActivity review at standard and 2× text, cold reopening, and the independent
completion audit also passed. They restored checks 1, 5, and 8 at that
checkpoint; the later `3301162` failure reopens check 8.
The earlier `6260187` device run passes
all 82 tests. The `8420180` Android run's source-deletion timeout did not
recur with the original immediate deletion timing and 120-second assertion;
no production transfer change separates those two runs, so its cause remains
unestablished.

The `d4a45ff` hosted run passes the Rust workspace (532 passes, four explicitly
ignored tests), all 70 web tests, Android foundation, both packaged Docker
one-way journeys (13 checks per architecture), Mac packaging, CodeQL, dependency
delta, and version checks. Its Android baseline passes 81 of 81 tests. The
separate full SAF journey reaches active folder-permission repair, then fails
in 220.80 seconds because the expected stable refreshed API is not observed.
The log does not retain the final API state, so the cause remains unestablished.
The unchanged full test's local 399.70-second pass is separate evidence, not a
replacement for the failed hosted result. Failure-only diagnostics are added without changing timeouts or acceptance
conditions; Android test compilation passes without rebuilding native helpers.

The Mac UI job on `142a6eb` passes five of six tests, including both accessibility
audits. The native Links/menu bar journey fails at pairing because
its isolated nodes bind localhost while automatically advertising a LAN address.
Pairing correctly requires the signed address to match the dialed address.
A fresh headless node reproduction fails with automatic addressing and pairs
successfully when both nodes explicitly advertise their localhost endpoints.
The UI fixture now sets those two addresses. On `7cf89c1`, pairing succeeds and
the journey reaches the paired-device picker, where the expected menu entry is
not found. The code preserves the expected name end to end; the log cannot
distinguish a runtime name mismatch from a menu accessibility lookup problem.
A stable paired-picker identifier and bounded failure diagnostics are included
in `d4a45ff`. Its hosted Mac run passes that checkpoint, then fails to find the
Transfers picker by its display label; later source-folder errors follow in the
same invocation. That shared picker now has its own stable identifier, and a
missing or inaccessible picker throws before later steps can run. The original
10-second timeout, Manual selection, and transfer assertions are unchanged.
The accessibility lookup cause is not yet established by runtime evidence.
That run's copied production helpers use test-only signatures, so it does not
approve production sandbox inheritance or Keychain startup. On `fd542fb`, the
journey uses the sandboxed app's own first-launch setup and LocalNodeManager,
then attempts quit/reopen, saved-identity reuse, and a second transfer through
the retained folder grant. Swift passes 179 tests and integration passes one;
UI passes five of six tests. The managed journey exceeds its unchanged
120-second allowance without retaining a last completed phase. This does not
establish which startup or transfer step was reached. Fixed, data-free phase
activities and bounded failure-log extraction are added for the next run, with
the same timeouts and assertions. The temporary root is also canonicalized so
exact process cleanup handles `/var` and `/private/var` aliases consistently.
These changes pass static validation but receive no native acceptance credit.
The attempt-two aggregate retains this same Mac failure, but its sanitized Mac
log is byte-identical to attempt one, including timestamps; it does not prove
that the Mac job reran.

On `3301162`, the Mac package and shared Rust jobs pass. The UI job remains
five of six, but its new phase markers prove that the app's managed setup and
native pairing complete. It enters manual-link creation about 22 seconds
after the first setup marker, then exceeds the same 120-second allowance
before that phase completes. The blocking operation within link creation is
still unknown; this is progress in diagnosis, not full native acceptance.
The next run records fixed boundaries for each manual-link control and folder
picker step while preserving the existing actions and timeouts.

The CI Docker jobs are configured to reuse unchanged build layers through
separate GitHub cache scopes for each architecture. Source fingerprints,
runtime tests, vulnerability scans, and artifact checks remain mandatory.
Cache export is optional and bounded, and extra build-record uploads are
disabled. Guardrail checks and workflow parsing pass; no hosted cache reuse
or speed improvement has been measured yet.

The first `fd542fb` Android attempt compiles successfully but runs zero app tests:
the emulator reports boot completion while its `settings` and `input` services
are unavailable. The requested targeted retry on the same source newly executes
all 81 baseline tests successfully in 74.895 seconds. Its separate SAF journey
then fails after 93.11 seconds because the initial system folder picker does not
complete before its existing deadline. Link setup and grant repair are not
reached, and the retained log does not establish why the picker remains open.
The aggregate also lists prior Mac and foundation results; their presence does
not show that those jobs reran. Neither Android attempt changed the then-current 70% score or
replaced the complete source-matched local Android evidence.
The next run adds failure-only elapsed time and fixed window categories to
distinguish picker visibility from the test host remaining active. It records
no screen text, paths, or URIs, and preserves the existing timeout and actions.
Offline instrumentation compilation and independent review pass.

The `3301162` Android run passes all 81 baseline tests in 77.641 seconds, then
fails the full SAF journey after 227.707 seconds during exact-folder grant
repair. Permission is persisted and the API address has changed, but the
service reports `STOPPED`, the share remains `READY`, no folder diagnostic is
present, and the saved repair is still pending. The retry action has zero UI
nodes. Source inspection confirms that the card hides the repair section when
the runtime has no folder-access error, even when a repair is saved. The
asynchronous access refresh can also stop/restart the service between saving
the repair and submitting it. The correction replays the exact saved repair
after SAF registration against the fresh API, preserves pending state until a
matching durable acknowledgement, and shows Retry even without folder health.
A recovered node keeps its repair API alive until the service restarts into
transfer mode; it does not call transfer startup on a recovery-only runtime.
All 36 focused JVM tests and Android instrumentation compilation pass. The
rendered regression and unchanged full SAF journey still require device
execution; these source changes do not close Android acceptance.

The remote drill now uses current one-way folder links and a build receipt for
exact packaged Mac component bytes. It runs private helper copies as an engine
harness and tests only owned `/sync`, synthetic appdata `/source`, and readable
`/boot-source` mounts. Syntax, mock contracts, and independent cleanup review
pass; no remote run has occurred. This harness does not establish installed Mac
app, Keychain, bookmark, application-restore, or boot-recovery acceptance. See
the [current drill runbook](atmos-one-way-drill.md).

A separate bounded rclone fault test confirms that an aggregate failed copy can
leave completed files at the destination. The wrapper now preserves ownership
from exact completed-copy records while retaining unresolved pending state;
explicit restoration cannot discard an unconfirmed file whose source vanished.
Six policy tests and four process-supervision tests pass locally, with temporary
build targets removed. The later `142a6eb` Rust and packaged Docker checks pass;
its complete Android device verification fails before the first Manual run.
The later local diagnostics establish that the earlier failure admitted no run
and never reached the copy worker. The corrected local UI then completes the
whole SAF journey on that same native runtime.

An optimized local measurement passes the exact 101-file transfer and unchanged
repeat. Two runtimes together use 0.01 seconds of sampled CPU during roughly
30 seconds of Manual/Scheduled idle, with no transfer workers. The baseline
unchanged run takes 30.02 seconds to report completion. A small delivery-timer
change removes redundant waits between successful control records while
preserving retry delay. In one comparison run, the 64 MiB plus 100 small-file
transfer completes in 25.34 seconds versus 34.88 seconds; the unchanged repeat
takes 30.46 seconds, with no improvement. These are single local loopback
measurements in a shared test process, not whole-app or Android battery figures.
Both measurement worktrees, build targets, fixtures, and processes were removed.
Final Mac native acceptance, remote validation, and release acceptance remain open.
Syncthing runtime modules, build scripts, source patch, and package assets have
been removed. [ADR 0008](../adr/0008-rclone-one-way-links.md) records the settled choice.

Previous-engine proof remains in the checkpoint 46–48 history and retained
validation receipts. It does not approve the rclone runtime or current native
release candidates.

Cleanup: obsolete ignored checkout build caches were removed: 43,667,280,523 logical bytes. Compact evidence and the existing Android size report were retained. Physical free-space change was not measured; active temporary toolchains, signing keys and unintegrated source remain. The completed Android journey's emulator, ADB instance, app, workers, guardians, and private build checkout were removed; its current APK and compact evidence remain.

Rclone cleanup: completed runtime and measurement worktrees and targets were
removed after checking ownership and active use. The latest completed root Rust
target alone contained 25,075,875,180 logical bytes. Reusable toolchains and compact proof packages remain for required native
validation. These counts
are logical file sizes; physical free-space change was not measured. Atmos
cleanup and new remote validation await restored SSH authentication; Atlas has
not been contacted.

The initial Android test replacement removed 815 net lines, including the old all-files
browser and host-controlled permission phases. Forty owned device screenshots
were removed, and the retained test no longer creates those temporary screenshots.
The completed replacement emulator, private ADB server, checkout, and build cache
are also removed. The deleted tree reported 11.62 GB through `du`; this is not a
measurement of freed physical space. Proof APKs and reusable toolchains remain.

The completed floating-toolbar review's emulator, private ADB instance, and
private build cache are removed. Its exact proof APKs and screenshots remain.
The obsolete Syncthing adapter's ignored Cargo target is also removed after
process, handle, and reference checks; it reported 30,461,364 KiB through `du`.
That checkout's uncommitted source and compact evidence are preserved.
The unused Syncthing process launcher and its legacy runtime-file validation
are removed, reducing those files by 160 net lines. Existing stop/reaper tests
now exercise the active rclone launcher and pass.

Twelve retired temporary directories are also removed after independent ownership,
source-retention, process, and open-file checks. They include the old Syncthing
sources/workers, superseded rclone package iterations, and an inactive Cargo
target; the review reported 3,002,968 KiB through `du`. Final packages and compact
evidence remain, including exact copies of the retired source diff and untracked
test. Physical free-space change was not measured. The fresh Mac pairing
reproduction's nodes and private build target are removed as well.

The completed Android layout diagnosis and acceptance fixture is removed,
including its emulator, private ADB server, worktree, build target, and owned
Gradle/Kotlin daemons. The deleted run root contained 3,418,855,288 logical bytes;
no owned device or worker processes remain. Compact logs and four proof images
are retained, and all 21 retained evidence files pass their hash checks.
