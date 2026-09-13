# Completion progress

Updated 2026-09-13 after the owner reaffirmed the lightweight one-way scope. The active goal is defined in [Product requirements](../product/requirements.md#active-completion-goal).

**Current scope: 70% verified, 7 of 10 complete acceptance checks.** This counts complete user journeys, not remaining time or reusable code. The active branch uses rclone as the sole transfer engine. Native Android setup and UI, independent fan-out, collection isolation, deletion/restoration, shared settings, and transfer cadence pass real rclone journeys. The remaining three checks are open. Earlier Syncthing evidence is retained as history and does not approve the replacement.

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
| 8 | Tomato-inspired Android floating actions, typography, data/status visuals, and top bar; actual pairing, permission, transfer, and background journeys pass. | Verified |
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
The current hosted API 37 run passes all 82 tests for PR source `6260187`:
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
complete device run. Together these complete Android acceptance check 8.

Row 5 combines the complete Manual/Continuous Android journey, the complete
15-minute source-owned scheduled run, and the recorded real charging/Wi-Fi
blocking and resumption phases. The earlier conditions run remains a failed
run; only its completed phases are credited. Its later Continuous-resume failure
is independently covered by the final passing journey. The core stale-condition
check also passes. This does not promise exact Android background timing.

Row 1 uses the completed API 37 SAF journey: native initiating-device pairing,
system folder selection, native Manual link creation and Run Now, exact forward
bytes, and a bounded check that a destination-only file stays off the source.
The responder confirms pairing and accepts the destination through the fixture's
authenticated API. The tested rclone runtime and Android changes are recorded
above. The newer hosted run also passes the added address-edit regression.

Hosted Android foundation/device tests, the Mac app bundle, and Docker on amd64
and arm64 pass for PR source `6260187`. CodeQL and release-version checks also
pass. The Rust job stops before its tests at an obsolete Mac address-placeholder
assertion; its corrected pairing-port check and the full local foundation pass.
The earlier `8420180` Android run timed out propagating a source deletion.
The new run retains the immediate deletion timing and 120-second assertion and
passes. No production transfer change separates those runs, and the earlier
timeout's cause is not established.

The Mac UI job passes five of six tests, including both accessibility audits
and the corrected legacy empty state. The sixth native Links/menu bar journey
now reaches the pairing action but times out waiting for the responder's
incoming request. The test now checks the entered fixture address and waits
within its existing bound for Pair Device to become enabled. Failure-only
diagnostics distinguish the visible pairing states. Fresh native verification
remains. Its isolated nodes
use production-built helpers with test-only signatures; it does not approve
production sandbox inheritance or Keychain startup. Android's earlier hosted
baseline passes 79 of 82 tests. The obsolete setup expectations, external-driver
prototype, and broad-storage journey are replaced in `e9fdf8f`. The source-derived
test contract covers all 82 tests exactly once: 81 baseline tests and one
self-contained SAF journey.

A separate bounded rclone fault test confirms that an aggregate failed copy can
leave completed files at the destination. The wrapper now preserves ownership
from exact completed-copy records while retaining unresolved pending state;
explicit restoration cannot discard an unconfirmed file whose source vanished.
Six policy tests and four process-supervision tests pass locally, with their
temporary build targets removed. This later change is not credited to the
passing `6260187` Android run and still needs its integrated verification.

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
