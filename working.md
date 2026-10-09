# Covalent 0.2.11 release

Updated: 2026-10-09

The owner authorized merging the functioning Covalent work to main and making
a new release. PR 45 integrates production-readiness and Android appliance/mobile
improvements over main, preserving the current presentation and CI setup work.

Completed preparation:

- Native versions and lockfile agree on 0.2.11 / build 2011. The public container
  stable channel is already container-v0.2.10; 0.2.11 advances both channels
  without bypassing the downgrade guard.
- Release version checking preserves Android/Apple failures. Regression fixtures
  reject version drift without modifying the checkout.
- GitHub verifies the release signing key. Both author and committer use the
  owner's email; no AI attribution is added.
- Android first-launch tests now assert the saved-links heading and absent root
  Back button, then verify system Back preserves the setup draft. The targeted
  API 37 test passes. The preceding exact 0.2.4 candidate passed the local Android
  gate and minified signed assembly; final 0.2.11 validation is required below.
- The 0.2.11 personal APK passed the full local Android gate and signed assembly.
  Exact APK startup, cold restart, node readiness and Settings/system Back passed
  on an API 37 arm64 emulator with software rendering.
- Hosted CI passed all 86 baseline instrumentation tests and reached the final
  removal check in the SAF transfer journey. That check polled a cached local
  port after removal restarted the service. It now follows the manager's live
  connection and still verifies both devices' removed state, unchanged identity,
  released durable folder capability and zero remaining workers. Full rerun is
  required before release.
- Release notes and publishing guidance identify the personal release certificate
  mismatch with the public v0.2.1 debug APK and the separate Pi acceptance scope.

Acceptance gates:

1. Final signed release commit has GitHub-verified signatures and all exact-source
   CI and CodeQL checks green, including the complete API 37 device suite.
2. Fast-forward the checked commit to main, preserving identity and linear history.
   Close superseded PRs 32 and 44 with a reference to integration PR 45.
3. Create an immutable signed annotated v0.2.11 tag. Complete CLI, container and
   unsigned macOS lanes; assemble signed Android personal assets with inventories,
   checksums, certificate and exact-source provenance.
4. Verify every draft asset and publish. Record workflow IDs, source SHA, artifact
   hashes, container digest and Android signing compatibility in release evidence.
5. Pin the verified published container digest in the Unraid template and commit
   the post-release status update.

Pi acceptance remains separate: interrupted Pictures journal recovery,
full-folder completion, nearby discovery and cellular handoff are pending.
Never delete originals based only on sync completion. No Pi image is flashed
as part of this Covalent release.
