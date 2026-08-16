# Validation matrix

No score of 100 is valid without fresh executable evidence and zero findings. Foundation checks are not production evidence.

| Gate | Tier | Foundation command | Production evidence required |
| --- | --- | --- | --- |
| Rust format/lint/tests | Shared blocker | `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `cargo test --workspace --all-features` | Unit, property, adversarial, migration, interruption, corruption, repair, multi-node, and benchmark suites. |
| Contract/docs structure | Shared blocker | `./scripts/validate-foundation.sh`; `cargo test --locked -p covalent-protocol --test contract_fixtures` | Versioned configuration, manifest, pairing, backup-summary, progress, event, and error fixtures plus `docs/product/traceability.md`. |
| macOS native | Tier 1 blocker | `swift test --package-path apps/apple`; `apps/apple/Scripts/integration-test.sh`; generated project macOS build and Release archive with a universal bundled helper | Live automatic node launch/reconnect/shutdown, authenticated health, arm64/x86_64 helper inspection, inherited-helper entitlement inspection, streamed security-scoped folder backup/empty-folder restore, permission revocation, UI/accessibility, signing/notarization where credentials exist. |
| Android native | Tier 1 blocker | `./scripts/check-android.sh`; `./scripts/android-api37-device-test.sh` on exact `Covalent_API_37` | Unit, Compose UI, instrumentation, real FD-streamed SAF backup/source-loss/restore, grant revocation, opt-in local-network permission, process death/resume, accessibility, release candidate. |
| Docker | Tier 1 blocker | `docker build -f packaging/docker/Dockerfile -t covalent:ci .`; `./scripts/check-container-runtime.sh covalent:ci`; `./scripts/docker-compose-e2e.sh covalent:ci` | Rootless/read-only runtime, multi-arch image, SBOM/scan/keyless-signing evidence, and a three-node explicit-replica disaster restore. |
| Unraid | Tier 1 blocker | XML plus safe-mount policy checks in `validate-foundation.sh` | Clean install/upgrade on Unraid, each selected-share backup, optional read-only boot backup, and explicit signed-preview restore drill. |
| iOS native | Tier 2 non-blocking | shared Swift tests; generated iOS simulator build when available | Native UI/accessibility tests, selected-directory flow, revoked bookmark, supported background resume. Report separately. |
| Public repository | Release blocker | `gh repo view thekozugroup/Covalent`; clean `git status`; remote HEAD | Public visibility, exact pushed `main`, required author, no secret/build/run debris, green required Tier 1 CI. |

## Canonical production scenario

Create three temporary nodes without external services; pair with matching authentication strings; disable and re-enable LAN discovery; connect over an explicit/Tailnet-style address; back up nested and empty directories; explicitly choose two providers; interrupt and resume; remove the source; restore from multiple providers beneath a new root; reject traversal and symlink attacks; detect one corrupt copy and repair it from the intact copy; export/import safe settings; revoke a peer; verify exact relative paths and content.
