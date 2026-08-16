# Validation matrix

No score of 100 is valid without fresh executable evidence and zero findings. Foundation checks are not production evidence.

| Gate | Tier | Foundation command | Production evidence required |
| --- | --- | --- | --- |
| Rust format/lint/tests | Shared blocker | `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `cargo test --workspace --all-features` | Unit, property, adversarial, migration, interruption, corruption, repair, multi-node, and benchmark suites. |
| Contract/docs structure | Shared blocker | `./scripts/validate-foundation.sh` | Golden compatibility fixtures and requirement traceability. |
| macOS native | Tier 1 blocker | `swift test --package-path apps/apple`; generated project macOS build | Signed app build where credentials exist, UI/accessibility tests, security-scoped folder backup/restore, permission revocation, background resume. |
| Android native | Tier 1 blocker | `./scripts/check-android.sh` | Unit, Compose UI, instrumentation, SAF revocation, process death/resume, accessibility, release candidate. |
| Docker | Tier 1 blocker | `docker build -f packaging/docker/Dockerfile .`; container health smoke | Rootless/read-only runtime, multi-arch image, SBOM/scan/signing evidence, multi-node disaster restore. |
| Unraid | Tier 1 blocker | XML and mount-policy checks in `validate-foundation.sh` | Clean install/upgrade on Unraid, selected-share backup, optional read-only boot backup, explicit restore drill. |
| iOS native | Tier 2 non-blocking | shared Swift tests; generated iOS simulator build when available | Native UI/accessibility tests, selected-directory flow, revoked bookmark, supported background resume. Report separately. |
| Public repository | Release blocker | `gh repo view thekozugroup/Covalent`; clean `git status`; remote HEAD | Public visibility, exact pushed `main`, required author, no secret/build/run debris, green required Tier 1 CI. |

## Canonical production scenario

Create three temporary nodes without external services; pair with matching authentication strings; disable and re-enable LAN discovery; connect over an explicit/Tailnet-style address; back up nested and empty directories; explicitly choose two providers; interrupt and resume; remove the source; restore from multiple providers beneath a new root; reject traversal and symlink attacks; detect one corrupt copy and repair it from the intact copy; export/import safe settings; revoke a peer; verify exact relative paths and content.
