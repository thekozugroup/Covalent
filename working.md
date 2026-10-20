# Interlink companion and appliance robustness — 2026-10-20

Active GPhoto OS goal: a focused phone companion, durable Pi imports, minimal
appliance image, and verified recovery without risking originals or pairing.

- Added a separate `:companion` application (`life.michaelwong.interlink`) using
  Material 3 Expressive, dynamic light/dark colours, a serif title and the brand
  mark. Shared authenticated pairing, folder grants, settings and transfer code
  remain in Covalent; the companion exposes only photo-folder links.
- The companion has its own private identity and fixed ports 18887/18889.
  Covalent keeps 8787/8789. No automatic identity/data migration is attempted.
- Source and destination appear together, with settings inside each link card.
  Appliance deletion/restoration settings are hidden and outgoing updates force
  both policies off. Phone originals remain intact.
- Both Android apps reconnect opted-in nodes after unlock/boot or package
  replacement. Companion startup opens protected storage on a background worker;
  activity recreation reuses that manager. Android restrictions after force-stop
  and restrictive battery policies still apply.
- Replaced pairing-address input with state-based editing after retaining an
  emulator ANR and dropped-character evidence. Fast hostname entry now retains
  the full value, including with the keyboard open and after rotation. Large-text
  and minified checks pass on the isolated API 36 emulator.
- Status reads now tolerate the brief API outage during a folder-permission
  restart. Mutations are never automatically replayed. Completed pairing state
  clears a lost-response error once, without hiding later failures.
- 205 Covalent unit tests pass. Companion debug/minified builds and debug/release
  lint, plus Covalent release lint, pass. Newly resolved Android
  dependency verification hashes were independently matched to publisher files.
- An isolated API 36 journey paired the companion with the native Pi fixture,
  selected a SAF folder, accepted a named receiving folder and transferred a
  synthetic PNG with matching SHA-256 and a committed successful run. The
  minified companion restarted its foreground service after reboot and after an
  APK update; another file transferred after update without opening its Activity,
  preserving pairing and phone originals. Native receiver tests also verify
  remote settings convergence and identity preservation after forced termination.
  Minified testing used an explicitly QA-only Android Debug certificate, never a
  substitute production signing identity. Cold launch measured 332 ms under the
  recovered emulator configuration; this is not a physical-phone performance claim.

Security follow-up: hosted RustSec checks found twelve advisories across the
locked Quinn, rustls and ZIP libraries. Updated to quinn-proto 0.11.18,
rustls 0.23.46 and zip 9.0.0, and adapted the frozen-upgrade fixture to ZIP's
fallible name API. Full locked workspace tests and all-feature clippy pass;
cargo-deny passes current advisories, bans, licenses and sources. No advisory
was suppressed. The patched minified Android companion rebuild is in progress;
prior APK/device evidence above predates these dependency changes.

GPhoto OS source is in the private Interlink repository, branch
`codex/appliance-robustness`, initial milestone `7893c65`. Removable storage
imports require per-volume approval, keep originals, verify bytes, publish
atomically and retain receipts after Photos cleanup. The account-free candidate
image removes another 37.92 MiB of APKs and passes component/filesystem checks.

Open gates: API 37 and physical companion checks; production signing and
companion release/SBOM automation; physical Pi/Solstice reboot and cellular
handoff; card/camera compatibility and PTP/MTP support; exact-cloud-proof source
cleanup; safe interrupted Pictures journal recovery; update credentials,
rollback and fresh-image cold boot. No physical installation was changed.
Do not clear the legacy transfer journal or enable restoration blindly.

---

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
