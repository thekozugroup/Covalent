# Completion progress

Updated 2026-09-12 after the owner narrowed the product to one-way links.

**Current scope: 20% verified, 2 of 10 complete acceptance checks.** This counts complete user journeys, not remaining time or reusable code. Native apps, pairing, permissions, worker supervision, Docker packages, and successful transfers form a working baseline. Deletion/restoration and shared-setting behavior now pass real three-node journeys. The remaining eight checks are open.

**Previous scope: 75%, 15 of 20 checks**, recorded at checkpoint 46 (c47003113e28b6e934a8ab4823040614fb07fb30). That score belongs to the superseded bidirectional/backup scope. Source and evidence remain in Git history.

| # | Acceptance check | Status |
| --- | --- | --- |
| 1 | Native setup pairs devices and creates a source/destination link; files never flow backward. | Open |
| 2 | Fan-out destinations work independently; multiple sources contribute safely to one collection. | Open |
| 3 | Both source-deletion options work with clear explanations and failed-scan protection. | Open |
| 4 | Destination deletions stay local across source edits/restarts; explicit restoration works. | Verified |
| 5 | Manual, scheduled, and continuous transfers respect platform limits; idle batch workers stop. | Open |
| 6 | Settings edited on any authorized member converge across the link; pending/stale edits remain visible. | Verified |
| 7 | Mac setup, links, settings, and per-link menu bar status pass native HIG/keyboard/VoiceOver checks. | Open |
| 8 | Android design and actual pairing, permission, transfer, status, and background journeys pass. | Open |
| 9 | Installable Mac/Android/Docker candidates pass real laptop–Atmos transfers and server mount handling. | Open |
| 10 | Relevant fault/security checks pass; idle/transfer performance is measured; temporary fixtures are removed; GitHub releases and installation guidance are published. | Open |

[Current requirements](../product/requirements.md). Atlas remains offline; Docker is the accepted Unraid target. Queued builds, source inspection, and unverified agent claims cannot complete a check.

Implementation evidence: the full Rust workspace run passed 908 tests (the real worker test is separate). Android shared settings passed 48 focused JVM tests and both Kotlin compilations. All 24 current Mac production Swift sources compiled and linked. Eight focused Swift tests passed with the installed Xcode toolchain; the earlier Command Line Tools attempt lacked the `Testing` module. The web console passed 128 tests. The final integrated three-node runtime run passed one-way fan-out, retained deletions, source deletion propagation, restoration after worker restart and shared/offline settings. The same run verified two competing offline edits converge to one accepted change and retain the rejected change for review. Docker built at 119,049,136 bytes, but its first transfer gate found a missing `HOME`; the correction awaits a fresh image. Scheduling and full native journeys remain pending. Rows 4 and 6 are verified by that runtime evidence; the other rows remain open.

Cleanup: obsolete ignored checkout build caches were removed: 43,667,280,523 logical bytes. Compact evidence and the existing Android size report were retained. Physical free-space change was not measured; active temporary toolchains, signing keys and unintegrated source remain.
