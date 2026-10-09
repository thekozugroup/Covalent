# Covalent 0.2.4 release

Updated: 2026-10-09

The owner authorized merging the functioning Covalent work to main and making
a new release. `codex/release-0.2.4` integrates production-readiness and the
Android appliance/mobile improvements over the current main commit. Existing
main presentation and CI setup changes are preserved. Six integration conflicts
retain the newer runtime and security source; the duplicate merged CodeQL YAML
key is removed.

Completed preparation:

- Native package versions and workspace lockfile agree on 0.2.4 / build 2004.
- Release version checking no longer loses a failed Android/Apple assertion
  when checking workspace dependencies. Isolated regression fixtures reject
  each version drift without modifying the checkout.
- Release notes distinguish Covalent validation from remaining Pi acceptance.
- All workflow mappings have unique YAML keys; integration has no unresolved
  conflicts or whitespace errors.

Acceptance gates:

1. Signed release commit, verified by GitHub, with all exact-source CI and
   CodeQL checks green. The earlier Android hosted failure occurred during
   emulator setup before the app tests; a failed-jobs retry is running.
2. Merge the checked commit into main with the owner's commit identity and
   preserve linear history. Superseded PRs 32 and 44 reference this integration.
3. Create an immutable signed annotated v0.2.4 tag. Complete CLI, container and
   unsigned macOS publishing lanes; assemble Android personal-use assets with
   checksums, certificate and exact-source provenance.
4. Verify the complete draft and publish it. Record final workflow IDs, release
   assets, container digest and signing compatibility in release evidence.

Pi acceptance remains separate: interrupted Pictures journal recovery,
full-folder completion, nearby discovery and cellular handoff are pending.
Never delete originals based only on sync completion. No Pi image is flashed
as part of this Covalent release.
