# Completion plan

Updated 2026-09-20. Active goal: release the smallest robust rclone-based one-way
Covalent wrapper for native macOS, native Android, and Docker/Unraid.

The owner now accepts release when the apps work on each supported platform and
at least one sync combination has been tested. Existing Mac, Android emulator,
both Docker architecture, Atmos, and Waypoint evidence satisfies that functional
threshold. Android has not been tested on the physical Pixel; Atlas is offline.
A full pairing matrix and repeated exact-package UI journeys are not required
unless a material change invalidates the relevant evidence. Remaining Mac UI
work uses computer use, not XCTest. Never enable or control VoiceOver.

The [product requirements](../product/requirements.md) and
[completion ledger](completion-progress.md) govern the scope. Current acceptance
is 80%, eight of ten checks. This reflects the owner's scope change, not additional
test passes. Retain historical contrast, accessibility-snapshot, and intermittent
Android failures honestly.

The later owner request adds a minimal, clean, cohesive Docker/Unraid web UI.
Server interface acceptance is open and publication waits for that work.

Remaining work:

- Implement and verify the Docker/Unraid web UI for setup, pairing, link
  configuration, and monitoring, using existing product behavior.

1. Finish relevant source and package checks; bind final artifacts to their source.
2. Verify the signed release commit and annotated tag. The approved signing key
   is already configured and registered.
3. Assemble and review the personal Mac, Android, Docker, and supporting release
   assets, checksums, notices, provenance, and installation guidance; publish the
   accepted GitHub release. Keep the Unraid template on the verified image digest.
4. Remove only owned obsolete temporary files, builds, caches, and processes.
   Preserve durable music nodes, signing identity, protected evidence, and user files.
5. Close the final ledger check and active goal only after publication and cleanup.
   Report one line each for Situation, Task, Action, and Result.

The former September 7 plan is historical and remains in Git history. Its
bidirectional Syncthing, distributed-backup, source-loss recovery, exhaustive
platform, and repeated UI gates are superseded. It must not reintroduce work
excluded by the current owner-approved scope. Release workflows accept a fully
checked signed branch commit; merging PR 32 is unnecessary. Preserve required
checks and repository protections.
