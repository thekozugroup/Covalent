# Covalent 0.2.12 release

Updated: 2026-10-09

Merged into main and published [v0.2.12](https://github.com/thekozugroup/Covalent/releases/tag/v0.2.12) with all 36 assets
verified. Runtime is `7bb83c27ed56d2b78517845db7bdf1757b80fbe1`; its signed tag remains immutable. PRs 45/46
contain the integration and removal/startup correction. PRs 48/49 contain
source-bound publishing repairs.

[The release record](docs/release/0.2.12-status.md) contains exact source/tag
identities, CI and publication runs, APK hash/certificates, container digest,
verification scope and retained failure evidence.

Completed: 202 Android unit tests, 86 hosted API 37 baseline tests and the full
SAF journey; exact signed minified APK startup/restart/navigation on API 36;
CLI workflow signatures/SBOM attestations; strict ad-hoc Mac app verification;
all container source, private-scan, signature, SBOM and promotion checks; all
asset checksums. Install guides now target the published release and the
Unraid template pins its verified immutable image digest.

Future CI uses the same cargo-deny 0.20.2 checker and all-feature policy through
a checksum-pinned native archive, avoiding the Docker-based action's startup
rate limit. The publishing repair pins BuildKit from a verified mirror; all
scan and signing requirements remain enabled.

Next: safe Pictures journal recovery, full-folder completion, phone-to-Pi
nearby discovery and cellular handoff, plus the separate GPhoto OS acceptance
gates. Preserve originals and account/pairing state. No Pi drive was flashed
as part of this Covalent release.
