# Validation matrix

Updated 2026-09-20. The owner accepts release when the apps work on each supported
platform and at least one sync combination has been tested. Existing Mac, Android
emulator, both Docker architecture, Atmos, and Waypoint evidence satisfies this
functional threshold. The later Docker/Unraid web UI request remains open and
blocks publication until the minimal, clean, cohesive server interface is
implemented and verified. A full device-pair matrix and repeated exact-package UI
journeys are not required without a material change affecting the evidence.
Historical failures remain recorded; acceptance does not mean every historical
test passed. Final packages, provenance, publication, guidance, and owned cleanup
remain required before 100%. Android emulator coverage is not physical-device
coverage. The commands below are reusable checks, not instructions to repeat all
completed journeys.

| Gate | Tier | Foundation command | Production evidence required |
| --- | --- | --- | --- |
| Rust format/lint/tests | Shared blocker | `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `cargo test --workspace --all-features` | Unit, property, adversarial, migration, interruption, corruption, repair, multi-node, and benchmark suites. |
| Contract/docs structure | Shared blocker | `./scripts/validate-foundation.sh`; `node scripts/check-openapi-routes.mjs`; Redocly OpenAPI lint; `cargo test --locked -p covalent-protocol --test contract_fixtures` | Exact runtime/OpenAPI method-path coverage, versioned configuration, manifest, pairing, link-settings, destination-status, progress, event, and error fixtures plus `docs/product/traceability.md`. |
| macOS native on Apple Silicon | Supported platform | `swift test --package-path apps/apple`; `apps/apple/Scripts/integration-test.sh`; generated project arm64 build and Release archive; `scripts/verify-apple-silicon-bundle.sh`; `scripts/apple-package-tls-e2e.sh` against packaged Caddy | Reuse completed native app operation, helper lifecycle, exact-CA HTTPS and wrong-CA rejection, arm64 architecture, inherited-helper entitlements, ad-hoc signatures, security-scoped one-way links, permission, keyboard, menu, and rendered evidence. Remaining native UI interaction uses computer use instead of XCTest. Retain source/package identity and actual actions/outcomes for any additional check; do not require a new complete exact-package UI journey without a material change. Historical contrast and accessibility-snapshot failures remain disclosed. VoiceOver and Apple Developer ID/notarization are excluded by owner instruction. |
| Android native | Tier 1 blocker | `./scripts/check-android.sh`; `./scripts/android-api37-device-test.sh` on exact `Covalent_API_37` | Unit, Compose UI, instrumentation, real FD-streamed SAF one-way transfer, both deletion choices and explicit restoration, grant revocation and repair, opt-in local-network permission, process death/resume, accessibility, and an installable debug-signed personal APK. Production signing is deferred. |
| Docker | Tier 1 blocker | Build/load `linux/amd64` and `linux/arm64` separately with Buildx; run `scripts/check-artifact-budgets.sh --image-only IMAGE --platform PLATFORM` for each; `./scripts/check-container-contract.sh AMD64_IMAGE`; `./scripts/check-container-runtime.sh AMD64_IMAGE`; `./scripts/docker-compose-e2e.sh AMD64_IMAGE` | Pinned Alpine runtime and exact OCI base metadata, loopback-only cleartext daemon, enrolled-CA HTTPS management with hostname verification, rootless/read-only runtime, a two-architecture manifest, per-architecture SBOM/scan/child-attestation evidence, keyless index and child signatures, and authenticated three-node one-way fan-out and collection-isolation checks. |
| Unraid through Docker | Tier 1 blocker | XML plus safe-mount policy checks in `validate-foundation.sh` | The user accepts the Docker deployment and its install/upgrade, selected-folder one-way transfer, explicit share mounts, appdata consistency guidance, and clearly bounded optional read-only boot-file access while Atlas is offline. Do not claim live-database consistency, boot recovery, or a physical Atlas test. |
| iOS native | Not a blocker — unsupported platform | No app target or CI lane. | None. iOS is not a supported platform. |
| Supply chain | Release blocker | `cargo audit --deny warnings`; `cargo deny check advisories bans licenses sources`; Apple and Android dependency inventories; pinned workflow lint | Exact-commit source/platform/container SBOMs, notices, checksums, scanned immutable digests, build-isolated OIDC signing, signatures with exact subject/predicate verification, and no undocumented vulnerability exception. |
| Public repository | Release blocker | `gh repo view thekozugroup/Covalent`; clean `git status`; release commit/tag inspection | Public visibility, verified signed release commit and annotated tag, required author, no secret/build/run debris, and relevant software/package checks. Reuse accepted platform operation and actual sync-combination evidence under the owner's current threshold. Final published assets must identify their source and package hashes. Release workflows accept a fully checked signed branch commit without merging PR 32; preserve required checks and repository protections. |

## Reusable behavior scenario

The completed behavior checks cover authenticated pairing, explicit addresses,
one-way links, fan-out, collection isolation, all three cadences, deletion choices,
restoration, interruption, restricted paths, shared settings, revocation, content
integrity, and cleanup. Reuse these results where the behavior is unchanged.
The completed Atmos-to-Waypoint and Atmos-to-Mac checks exceed the owner's minimum
of one tested sync combination. Do not add another combination solely to complete
a matrix. The active authority is [Product requirements](../product/requirements.md#active-completion-goal).
