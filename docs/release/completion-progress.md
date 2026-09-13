# Completion progress

Updated 2026-09-13 after the owner reaffirmed the lightweight one-way scope. The active goal is defined in [Product requirements](../product/requirements.md#active-completion-goal).

**Current scope: 40% verified, 4 of 10 complete acceptance checks.** This counts complete user journeys, not remaining time or reusable code. The active branch uses rclone as the sole transfer engine. Independent fan-out, collection isolation, deletion/restoration, and shared-setting behavior pass real three-node rclone journeys. The remaining six checks are open. Earlier Syncthing evidence is retained as history and does not approve the replacement.

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
protection, and recovery. Android startup now registers saved folder capabilities
before starting transfers and isolates unavailable grants. Its fresh APK passes
package integrity and provenance checks; the full device journey is running.
The hosted Mac app bundle passes. Final native app,
Linux package execution, performance, and release acceptance remain open.
Syncthing runtime modules, build scripts, source patch, and package assets have
been removed. [ADR 0008](../adr/0008-rclone-one-way-links.md) records the settled choice.

Previous-engine evidence: the full Rust workspace run passed 908 tests (real worker tests run separately). The current Android build, lint, and 175 JVM tests pass. All 24 Mac production Swift sources compiled and linked, and eight focused Swift tests passed. The web console passed 128 tests. The integrated three-node runtime passes one-way fan-out with an offline destination, retained deletions, source deletion propagation, restoration after restart, and shared/offline/conflicting settings. A separate three-node collection journey passes distinct child-folder ownership, identical filename isolation, overlapping-root rejection, independent settings, and scoped deletion. Missing source mounts and failed source scans preserve destination files while the healthy contributor keeps transferring; restoring the source resumes transfers. The full foundation check and strict integration-test Clippy pass.

Checkpoint 47 hosted CI found Android error-catalog omissions, Docker manifest rejection, and Mac packaging/model-test failures. Corrections now pass Android's full build gate, all 175 Swift tests, Mac engine packaging/signing, and seven host-validation tests on each platform. The fresh Atmos arm64 image passes its dynamic contract and 13-check one-way transfer gate; original server containers, tagged images, networks, and volumes are restored. Android's actual API 37 device journey passes pairing, one-way transfer, pause/resume, address changes, cold restart, visible deletion-setting confirmation, authenticated settings convergence, restoration, deletion propagation, permission loss, recovery, and safe removal. The native deletion explanations and the independently tested failed-scan protection complete row 3. The historical test selector still contains BothWays; its assertions now verify one-way behavior. Scheduling, native HIG/device acceptance, final packages, performance, and publication remain open.

Cleanup: obsolete ignored checkout build caches were removed: 43,667,280,523 logical bytes. Compact evidence and the existing Android size report were retained. Physical free-space change was not measured; active temporary toolchains, signing keys and unintegrated source remain. The completed Android journey's emulator, ADB instance, app, workers, guardians, and private build checkout were removed; its current APK and compact evidence remain.
