# Completion progress

Updated 2026-09-12 after the owner narrowed the product to one-way links.

**Current scope: 40% verified, 4 of 10 complete acceptance checks.** This counts complete user journeys, not remaining time or reusable code. Native apps, pairing, permissions, worker supervision, Docker packages, and successful transfers form a working baseline. Independent fan-out, collection isolation, deletion/restoration, and shared-setting behavior now pass real three-node journeys. The remaining six checks are open.

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
| 8 | Android design and actual pairing, permission, transfer, status, and background journeys pass. | Open |
| 9 | Installable Mac/Android/Docker candidates pass real laptop–Atmos transfers and server mount handling. | Open |
| 10 | Relevant fault/security checks pass; idle/transfer performance is measured; temporary fixtures are removed; GitHub releases and installation guidance are published. | Open |

[Current requirements](../product/requirements.md). Atlas remains offline; Docker is the accepted Unraid target. Queued builds, source inspection, and unverified agent claims cannot complete a check.

Implementation evidence: the full Rust workspace run passed 908 tests (real worker tests run separately). The current Android build, lint, and 175 JVM tests pass. All 24 Mac production Swift sources compiled and linked, and eight focused Swift tests passed. The web console passed 128 tests. The integrated three-node runtime passes one-way fan-out with an offline destination, retained deletions, source deletion propagation, restoration after restart, and shared/offline/conflicting settings. A separate three-node collection journey passes distinct child-folder ownership, identical filename isolation, overlapping-root rejection, independent settings, and scoped deletion. Missing source mounts and failed source scans preserve destination files while the healthy contributor keeps transferring; restoring the source resumes transfers. The full foundation check and strict integration-test Clippy pass.

Checkpoint 47 hosted CI found Android error-catalog omissions, Docker manifest rejection, and Mac packaging/model-test failures. Corrections now pass Android's full build gate, all 175 Swift tests, Mac engine packaging/signing, and seven host-validation tests on each platform. The fresh Atmos arm64 image passes its dynamic contract and 13-check one-way transfer gate; original server containers, tagged images, networks, and volumes are restored. Android's actual API 37 device journey passes pairing, one-way transfer, pause/resume, address changes, cold restart, visible deletion-setting confirmation, authenticated settings convergence, restoration, deletion propagation, permission loss, recovery, and safe removal. The native deletion explanations and the independently tested failed-scan protection complete row 3. The historical test selector still contains BothWays; its assertions now verify one-way behavior. Scheduling, native HIG/device acceptance, final packages, performance, and publication remain open.

Cleanup: obsolete ignored checkout build caches were removed: 43,667,280,523 logical bytes. Compact evidence and the existing Android size report were retained. Physical free-space change was not measured; active temporary toolchains, signing keys and unintegrated source remain. The completed Android journey's emulator, ADB instance, app, workers, guardians, and private build checkout were removed; its current APK and compact evidence remain.
