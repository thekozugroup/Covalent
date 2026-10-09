# Covalent 0.2.12 release

Updated: 2026-10-09

The owner authorized merging Covalent into main and publishing a new release.
PR 45 is merged at adafa5375f9a14ec3fecbc4641b2dbcb13c1f432. Its exact-source
CI and CodeQL passed, including 86 API 37 baseline tests and the full SAF journey.

The additional local arm64 journey exposed a real interrupted-removal gap:
preparing a removal can restart the local service, leaving the durable tombstone
unreplayed on startup. The source remained running and its removal stayed pending.
The v0.2.11 tag remains immutable; its draft is not published and its container
lane was cancelled before promotion. Final release advances to v0.2.12.

The correction replays exact saved removals with the fresh runtime, retains
tombstones until acknowledgement is durable, and serializes startup/interactive
capability changes. Three new regression tests fail on the previous code and pass after correction;
the existing capability-repair and lifecycle tests also pass.

Acceptance gates:

1. Corrected source passes unit/lint/package checks, targeted persisted-removal
   recovery on the owned API 37 arm64 emulator, and the complete hosted suite.
2. Merge the signed checked correction into main preserving the owner identity.
3. Create a signed immutable v0.2.12 tag and complete CLI, macOS and container lanes.
4. Rebuild and test the exact signed minified APK, verify every draft asset and
   signature, then publish the complete release. Record exact-source evidence.
5. Pin the verified published container digest and commit final release status.

Pi acceptance remains separate: Pictures journal recovery, full-folder completion,
nearby discovery and cellular handoff. Never delete originals based only on sync
completion. No Pi image is flashed as part of this release.
