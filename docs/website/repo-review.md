# Public repository review

Reviewed 2026-10-03. Scope: public presentation and installation documentation.
No runtime, deployment, signing, or release changes were made for this review.

## Public entry point

The README now explains the one-way file product, shows the console, and routes
readers to installation and first-link instructions. It states deletion defaults,
ordinary destination-file behavior, background scheduling limits, and the need
for independent backups. It avoids performance, deduplication, competitor, and
complete-readiness claims.

GitHub release metadata identifies
[v0.2.1](https://github.com/thekozugroup/Covalent/releases/tag/v0.2.1) as the latest
published release. It contains Mac and Android personal-use packages, CLI
archives, checksums, container evidence, and the Unraid template. v0.2.2 and
v0.2.3 were drafts at review time; they are not public installation targets.
This review checked metadata, not package contents or new device behavior.

## Documentation reconciled

First-link, product, and Mac guidance now distinguishes published v0.2.1
personal-use acceptance from verification of new builds and untested device
combinations. The historical completion ledger is unchanged. Mac and Android
setup pages put published downloads before optional source builds.

Contribution scope now follows the rclone one-way architecture and preserves
legacy data. The security policy and threat model distinguish authenticated
folder transfers and ordinary destination files from retained encrypted backup
archives. Folder links do not claim encryption at rest or historical recovery.

The README links the [website content pack](HANDOFF.md) and its overview image.

## Distribution limits to retain

Mac packages are ad-hoc signed and not notarized. Android packages use a debug
signer, and current Android acceptance is emulator evidence. Unraid has a Docker
template but no Community Applications listing. The project is pre-1.0 and has
not completed an external cryptographic audit. These limits must remain visible
in launch material; this presentation review does not resolve them.
