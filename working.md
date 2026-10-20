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
was suppressed. The patched minified companion rebuilt for both Android ABIs.
A synthetic PNG transferred after in-place APK update with the Activity unopened,
then another transferred to the patched Pi core after its forced restart. Exact
source/destination bytes match; pairing and originals remain. The installed QA
APK matches `artifacts/interlink-qa/interlink-minified-4389d16-qa.apk` (SHA-256
`994200163a7ba9121cee8df122e8f41ea2274c5aa20c0c6d7f96d1c4ad326322`).
It retains the Android Debug certificate for isolated QA only. The runtime CI
run was superseded by the documentation push. The current hosted container sync
journeys pass, but vulnerability scans on both architectures reject Go 1.26.7
and golang.org/x/net v0.58.0. Reports identify fixes in Go 1.26.9 and x/net
v0.60.0; those changes and full rescan remain open. No scan was suppressed and
no release was published. Evidence: CI run `39478832334`, private
`artifacts/interlink-qa/hosted-go-findings.json`.

October 20 dependency remediation: source milestone `33ad205` updates the
restricted rclone and Caddy graphs, rustls-webpki to 0.103.16 and
webpki-root-certs to 1.0.10. The full locked all-feature Rust workspace suite
passes, as do current cargo-deny advisories/bans/licenses/sources without
exceptions. All maintained builders now require exact Go 1.26.10. Caddy pins
x/net v0.60.0, OpenTelemetry v1.45.0, aligned log modules v0.21.0 and patched
trace exporters v1.45.0. The compiler caught the incompatible mixed log APIs
before they could reach a candidate.

Both Linux Caddy binaries rebuilt in an isolated arm64 Linux container
(amd64 cross-compiled), with target-specific inventories and corresponding
source verified against strict new byte/count pins. Their final Grype 0.117.0
binary-only scan reports zero findings using database v6.1.10, built
2026-10-20T06:29:49Z. This does not cover the complete container. The Go image
and new 20260909-r0 CA provenance were verified from official registry
manifests/layers; complete source/recipe and expected-notice pins were
regenerated without fabricating an installed-runtime receipt. Complete native
CI image checks/scans remain open, as does the newly observed amd64 optional
source-deletion timeout in run 39481535802. Appliance links keep deletion off.

The Mac worker rebuilt from the committed rclone tree with SHA-256
`07cb75e9292c3c3a08322f4e34a6b1e6f91599b43ae917ae64f6d13bfb864922`.
All six notice/package pins were regenerated from measured output. The build
also exposed and fixed unquoted executable expansion when the toolchain path
contains spaces. Android rebuild and installed transfer verification with
these Go/certificate changes remain required. Evidence is private under
`artifacts/go-security/`; no release or physical update has occurred.

The real Mac packaging smoke passes, including two independently rebuilt,
byte-identical signed bundles, unsafe-output refusal and source cleanup. The
147 sync-engine host checks and all-feature workspace clippy also pass. The
rebuilt Mac worker has zero findings in the same current Grype database.

The clean hosted Android foundation run exposed a missing companion release
lint report: the unqualified `lint` task had not generated it, while an older
local report masked the omission. The dedicated verification task now depends
on both lint variants, full Android checks explicitly request release lint,
and candidate generation checks the lint results as well as prebuilt mode.
The next exact-source full check must regenerate removed companion reports.

Release hardening: both Android apps now use one reviewed, fail-closed Maven
inventory generator. Covalent's output matches its previous JSON exactly;
Interlink records its own 80 dependencies, including Material 3 Expressive.
Changed application version, dependency coordinates and candidate file bytes
invalidate the respective Gradle tasks. Real offline Gradle regressions cover
those changes, unreviewed licenses and duplicate native entries.

The dedicated candidate includes APK/AAB, mapping, runtime/license inventories,
native provenance binding and SHA-256 checksums. Both native ABIs match the
actual link records across debug/release; all eight packaged native objects
match between APK and AAB. Prebuilt checks also re-read both companion lint
reports. The package guard caught and refused an older debug APK containing
the pre-security-fix engine before both variants were rebuilt together.

Added a separate Interlink signing preparer with no fallback identity: pinned
certificate, explicit private keystore inputs, no debug signer, exact app/version,
native package validation and v2/v3 verification. Passwords stay out of command
arguments, public output and non-signing subprocesses. Existing output is never
replaced. Fifteen policy regressions and real SDK smoke with disposable test
keys pass. The smoke neither installs nor publishes. Local full builds, 205
shared unit tests, debug/release lint and native package checks pass. CI now
retains unsigned companion candidates; source fingerprints must remain stable
through the full gate before a prebuilt stamp is accepted. Production identity,
signed publication and real upgrade acceptance remain unconfigured/open.
See `docs/platform/interlink.md` for exact commands and boundaries.

GPhoto OS source is in the private Interlink repository, branch
`codex/appliance-robustness`, initial milestone `7893c65`. Removable storage
imports require per-volume approval, keep originals, verify bytes, publish
atomically and retain receipts after Photos cleanup. The account-free candidate
image removes another 37.92 MiB of APKs and passes component/filesystem checks.

Open gates: current Go/networking vulnerability fixes and rescans; API 37 and
physical companion checks; production signing and signed publication;
physical Pi/Solstice reboot and cellular handoff; physical card/PTP/MTP camera
compatibility; exact-cloud-proof source
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
