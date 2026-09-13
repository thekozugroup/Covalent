# Completion progress

Updated 2026-09-13 after the owner reaffirmed the lightweight one-way scope. The active goal is defined in [Product requirements](../product/requirements.md#active-completion-goal).

**Current scope: 40% verified, 4 of 10 complete acceptance checks.** This counts
complete user journeys, not remaining time or reusable code. Rclone is the sole
transfer engine and its migration is complete. Independent fan-out, collection
isolation, deletion/restoration, and shared settings remain verified. The latest
Android run fails its first Manual transfer, reopening native setup, cadence,
and Android acceptance previously counted at 70%. The earlier passing Android
journey remains baseline evidence; it does not approve this changed copy path.
Earlier Syncthing evidence is retained only as history.

**Previous scope: 75%, 15 of 20 checks**, recorded at checkpoint 46 (c47003113e28b6e934a8ab4823040614fb07fb30). That score belongs to the superseded bidirectional/backup scope. Source and evidence remain in Git history.

| # | Acceptance check | Status |
| --- | --- | --- |
| 1 | Native setup pairs devices and creates a source/destination link; files never flow backward. | Open |
| 2 | Fan-out destinations work independently; multiple sources contribute safely to one collection. | Verified |
| 3 | Both source-deletion options work with clear explanations and failed-scan protection. | Verified |
| 4 | Destination deletions stay local across source edits/restarts; explicit restoration works. | Verified |
| 5 | Manual, scheduled, and continuous transfers respect platform limits; idle batch workers stop. | Open |
| 6 | Settings edited on any authorized member converge across the link; pending/stale edits remain visible. | Verified |
| 7 | Mac setup, links, settings, and per-link menu bar status pass native HIG/keyboard/VoiceOver checks. | Open |
| 8 | Tomato-inspired Android floating actions, typography, data/status visuals, and top bar; actual pairing, permission, transfer, and background journeys pass. | Open |
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
complete device run. The visual coverage remains valid, but the later transfer failure reopens Android acceptance check 8.

Earlier row 5 evidence combines the complete Manual/Continuous Android journey, the complete
15-minute source-owned scheduled run, and the recorded real charging/Wi-Fi
blocking and resumption phases. The earlier conditions run remains a failed
run; only its completed phases are credited. Its later Continuous-resume failure
is independently covered by the final passing journey. The core stale-condition
check also passes. This does not promise exact Android background timing. The current native
Manual/Continuous transfer path must pass again before row 5 can close.

Earlier row 1 evidence uses the completed `6260187` API 37 SAF journey: native initiating-device pairing,
system folder selection, native Manual link creation and Run Now, exact forward
bytes, and a bounded check that a destination-only file stays off the source.
The responder confirms pairing and accepts the destination through the fixture's
authenticated API. The tested rclone runtime and Android changes are recorded
above. That run also passes the added address-edit regression. Row 1 is reopened
until the changed native transfer path passes on current source.

The latest integrated source `142a6eb` passes the Rust workspace and strict
Clippy, all 70 web tests, Android foundation, the Mac app bundle, and packaged
Docker journeys on amd64 and arm64. CodeQL and release-version checks pass.
Its Android device baseline passes 81 tests, but the full SAF journey times
out at its first Manual destination-byte wait. The failure occurs before the
first completed generation or deletion checks. No node/run status was emitted
there, so the cause remains unknown. The earlier `6260187` device run passes
all 82 tests. The `8420180` Android run's source-deletion timeout did not
recur with the original immediate deletion timing and 120-second assertion;
no production transfer change separates those two runs, so its cause remains
unestablished.

The latest Mac UI job passes five of six tests, including both accessibility
audits. The native Links/menu bar journey reaches Pair Device but fails because
its isolated nodes bind localhost while automatically advertising a LAN address.
Pairing correctly requires the signed address to match the dialed address.
A fresh headless node reproduction fails with automatic addressing and pairs
successfully when both nodes explicitly advertise their localhost endpoints.
The UI fixture now sets those two addresses; the native rerun remains pending.
Its copied production helpers use test-only signatures, so it does not approve
production sandbox inheritance or Keychain startup.

A separate bounded rclone fault test confirms that an aggregate failed copy can
leave completed files at the destination. The wrapper now preserves ownership
from exact completed-copy records while retaining unresolved pending state;
explicit restoration cannot discard an unconfirmed file whose source vanished.
Six policy tests and four process-supervision tests pass locally, with temporary
build targets removed. The later `142a6eb` Rust and packaged Docker checks pass;
its complete Android device verification fails at the first Manual transfer.
The next run will include bounded source/destination state in that failure
message without changing the deadline, retries, or transfer assertions.

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
