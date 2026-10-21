# Interlink companion and appliance robustness — 2026-10-20

Hosted 1481c16 run 39514337288 passes Android foundation, Rust/contracts, both
containers, dependencies and both macOS jobs. Its device job passes all 86
baseline tests and the SAF journey, then fails at the added permission
harness's ADB grant command after opening Settings (exit 20). The full gate
fails; the grant failure's cause is not established. The QA harness now uses
the actual Permissions → Nearby devices → Allow controls and returns through
Settings without launching the Activity or starting its service. Back actions
are bounded independently of unavailable hierarchies, and the screen after
the final Back is observed. The headed API37/ARM64 rerun passes both Settings
return and the runtime prompt, requiring native ready state, a foreground
service and unchanged device/TLS/key-envelope hashes. They also match the
retained failed run's baseline. App runtime and APK bytes are unchanged.
Earlier framework/launcher ANRs were cleared with Wait on the same owned AVD;
no emulator restart or data reset was used. The earlier harness back-stack
failure and receipts remain. Evidence:
artifacts/interlink-qa/api37-settings-permission-recovery.log and
api37-settings-permission-recovery/permission-recovery.json. Latest-head hosted
acceptance remains required; the previous hosted grant failure is not relabeled.

Build reliability follow-up: c1ee2b8 run 39510203636 passes Android foundation,
Rust, both containers, dependencies, macOS, CodeQL and version sync. Its device
job fails in :app:packageRelease with explicit Java heap exhaustion in
ApkFlinger. The missing prebuild stamp then refuses the device suite; the new
permission journey is not reached. This is not a permission-test failure.
3b54db6 raises the Gradle heap to 4 GiB, limits workers to two and disables
parallel projects. 510a1f7 also exports the resolved SDK to all helpers, fixing
the reproduced macOS fallback failure after successful Gradle packaging.

The exact 510a1f7 full Android gate passes locally: 205 shared unit tests,
zero-error debug/release lint for both apps, native/package/provenance guards
and release inventories. Both SDK environment variables were unset at entry,
so this exercises the repaired SDK fallback. An earlier release APK is saved;
removing only the generated APK forced :app:packageRelease to execute in the
3b54db6 Gradle build, which passes. Initial wrong-Go and missing-SDK-helper
attempt logs remain; the final run uses pinned Go 1.26.10. Receipt:
artifacts/interlink-qa/build-memory-510a1f7.json. Its hosted follow-up remains
required. This documentation change does not relabel the runtime artifacts.

Previous fully green hosted milestone 6177379d1e3c0dab4c514d49d9fa340d3eb0dcbd passes all
jobs in run 39504560766: Android foundation, API37 device, both complete
native containers, shared Rust/contracts, dependency review, both macOS jobs and
the combined release-candidate software gate. CodeQL and version sync pass too.
Android proves all 86 expected
baseline tests by name and the separate SAF journey. The earlier packageRelease
failure did not recur; no unproven cause is assigned. Receipt:
artifacts/interlink-qa/hosted-6177379-success.json. These are software checks;
physical device, production signing and publication remain separate gates.

The SDK37 signer-report repair supports both known certificate labels, requires
one signer and every certificate pinned, and refuses unknown/duplicate rows.
Twenty-one policy tests and real SDK36/37 disposable-key smokes pass, including
v2/v3 signatures, debug-key refusal and existing-output preservation. No signing
requirement was relaxed or production identity substituted.

Companion follow-up: unpaired status now says Pair your Pi to continue; a paired
node without folders asks for a source folder. Error/startup/stopping handling
is retained. Purpose text states that originals stay on the phone, and setup
instructions follow the actual pair-first form order. The minified build,
release lint and all 205 shared unit tests pass. Fresh API36/ARM64 testing uses
an isolated 16 KiB-page AVD: first setup, Get started, unpaired state, light/dark
and scroll access to folder controls at 200% text pass. Logo/serif header remains
without a permanent Back button. Temporary theme/font changes were restored.
No paired link, phone original or physical installation was changed by this UI
check. Private evidence: artifacts/interlink-qa/setup-clarity-*.png and
setup-clarity-build.log; QA signing uses the existing Android Debug identity.
The exact committed `6177379` minified APK passes API37 cold launch and initial
permission denial/grant checks on the separate headed Interlink_QA_API37 AVD.
Its hosted run 39504560766 now passes all software gates above.

Permission recovery follow-up: a real revocation on API37 reproduced a startup
screen with no permission action. Both apps' folder-sync surface now shows
Allow access/Open settings and rechecks access on resume. Restoring access
restarts only an opted-in host. The guarded debug companion journey passes
actual revocation, Settings return and runtime grant, requiring native-API
ready state and a foreground service. Device/TLS identities and the key
envelope remain byte-identical. The test never clears data and accepts only
fixed disposable AVDs, not phones. CI now runs it after the existing 86 baseline
tests and SAF journey. Exact runtime source 6b31fde6d127497e097591da8b49eb6d58dc174f
passes the full cross-app Android gate, all 205 unit tests, both lint variants
for both apps, and native/package/inventory checks. Its guarded debug journey
passes on the headed API37 ARM64/4 KiB AVD. The minified QA APK passes both
recovery routes and installed-byte readback. After an actual emulator reboot,
the foreground service and UDP 18887 listener return before opening the
Activity; the launcher remains foreground. All three protected identity-file
hashes remain unchanged after minified recovery and reboot. QA retains the
existing Android Debug signer. This documentation commit does not rebuild or
relabel those runtime bytes. Latest-head hosted follow-up remains separate.
Receipts: artifacts/interlink-qa/permission-recovery-6b31fde-build.json,
api37-permission-recovery-6b31fde/permission-recovery.json and
interlink-6b31fde-qa.json in the same private evidence directory.
The full cross-app gate caught two missing explicit API37 guards and the
superseded unused denial string. Both permission launches are now guarded;
the unused string is removed. No lint rule was suppressed. Retain the failed
07fd041 full-check log; the corrected 6b31fde full gate passes without exceptions.

The compact Pi checkpoint is GPhotoOS-rpi5-nvme-0.3.1-19a9a2f.img in the private
Interlink checkout. Offline image validation, 76 root-Linux tests, ten Android
shell checks and 66 signed Android staging scenarios pass. The current updater
fixes Android-incompatible desktop-Java APIs; the historical updater still
requires a separate safe bootstrap. Host signing safeguards pass 12 real SDK
checks. Two real system APK 5 → 7 → 5 scans preserve selected synthetic data;
the repeat includes storage access and Android Keystore. This is not the full
signed 27-component migration. The legacy assisted updater refuses this
development version before ADB contact. Private run 39519742686 at c6cdff3 cannot
start either job because of account billing/spending limits. No physical
boot or Google Photos deletion acceptance is inferred. User requests software
work first. Source retention supports retain/off; delete-after-cloud-backup is
not implemented. Production companion signing/publication, physical Pi/Solstice,
roaming, actual cards/cameras and exact Photos cleanup remain open. Pictures
only; DCIM is unauthorized. Preserve the legacy recovery journal.

## Earlier development evidence

The entries below retain prior checkpoints and failures. Current results above
supersede their pending/failing software-gate statements.

Hosted run 39491384945 passes both native container jobs completely: exact
packaging/source/notices, folder journeys, vulnerability scans and budgets. The
earlier optional amd64 deletion timeout did not recur; its cause is unproven.
Shared Rust/contracts, dependency review and both macOS jobs pass. Android
foundation fails in :app:packageRelease with an opaque IncrementalSplitterRunnable
exception and no nested cause in the retained log. Local execution initially
reports that task up to date. After preserving and removing only its generated
unsigned APK, the packaging task actually regenerates it and passes in 37 seconds.
This narrow local check does not clear the failed hosted gate or establish full
Android acceptance. The full gate now requests --stacktrace in both JDK branches
to retain the cause of a repeat failure. No release signing identity changed.
Private logs: artifacts/go-security/android-package-release-37d.log and
android-package-release-forced-37d.log; the prior unsigned APK is preserved.

Hosted run 39490316340 passes the signed CA upgrade, exact CA bytes and Caddy
collection on both native architectures, then fails in Alpine source fetching
with the old generic diagnostic. A clean local Linux container downloads and
validates all 155 source inputs. Added sanitized per-input HTTP/network errors
without logging exception URLs/bodies, with credential-leak regressions. Retain
the unchanged 230,531-byte CA source archive in the repository and seed the
build-only cache; strict size/SHA256/original APKBUILD SHA512 still apply. This
removes GitLab archive reachability from that step. The hosted failure's exact
HTTP cause was not recorded; complete-image acceptance remains open.


October 20 CA build repair: the old 20260909 pin failed hosted image builds.
Both targets now consume the same checksum-pinned official signed 20261020
noarch APK. The unused old CA generator is removed from the Go builder so its
triggers cannot overwrite the new trust file. Signature-checked upgrades on
both real digest-verified builder roots preserve the exact expected CA bytes.
Target checks bind apk's measured metadata difference (x86_64 on amd64, noarch
on arm64). A fresh source/recipe download verifies all 155 locked inputs;
source and notice review pins were regenerated together. Caddy binaries are
unchanged and both pass their strict distribution verifier with the new CA.
Collector/verifier regressions and the static container contract pass. No
installed-runtime receipt was fabricated. Complete native CI images, scans and
the earlier optional amd64 deletion journey remain open. No release or physical
installation is promoted by this repair.


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
directory-based scan was superseded by explicit file SBOM scans: each ELF
inventories 149 packages and reports one unknown-severity `GO-2026-5932`
module finding, with no critical/high findings, using database v6.1.10 built
2026-10-20T06:29:49Z. The affected OpenPGP package is absent from both exact
target graphs (949/951 packages). No scanner finding is suppressed. This does
not cover the complete container. The Go image
and new 20260909-r0 CA provenance were verified from official registry
manifests/layers; complete source/recipe and expected-notice pins were
regenerated without fabricating an installed-runtime receipt. Complete native
CI image checks/scans remain open, as does the newly observed amd64 optional
source-deletion timeout in run 39481535802. Appliance links keep deletion off.

The Mac worker rebuilt from the committed rclone tree with SHA-256
`07cb75e9292c3c3a08322f4e34a6b1e6f91599b43ae917ae64f6d13bfb864922`.
All six notice/package pins were regenerated from measured output. The build
also exposed and fixed unquoted executable expansion when the toolchain path
contains spaces. The Android rebuild and installed transfer result are recorded
below. Evidence is private under
`artifacts/go-security/`; no release or physical update has occurred.

The real Mac packaging smoke passes, including two independently rebuilt,
byte-identical signed bundles, unsafe-output refusal and source cleanup. The
147 sync-engine host checks and all-feature workspace clippy also pass. The
rebuilt Mac worker's explicit file SBOM inventories 54 packages and reports
the same single unknown-severity OpenPGP module finding, absent from its
478-package target graph. The older directory-based zero is superseded.

The clean hosted Android foundation run exposed a missing companion release
lint report: the unqualified `lint` task had not generated it, while an older
local report masked the omission. The dedicated verification task now depends
on both lint variants, full Android checks explicitly request release lint,
and candidate generation checks the lint results as well as prebuilt mode.
The exact-source full check at `2f9bc65` regenerated both removed companion
reports and passed, followed by prebuilt verification. Both lint variants have
zero errors; all 14 companion release checksums match. The minified QA APK
`ffd13846601d87ca0ef125727a13c688335ff067b2528017b1bc151c3908343b` installed
in place with the existing Android Debug signer on the isolated API 36 emulator.
With the updated native receiver, a new synthetic Pictures file committed in
55.8 seconds without opening the Activity; pairing, bytes and the source
original were preserved. This is not a production-signed or hardware release.

Hosted run 39485175979 exposed a separate container foundation failure:
Alpine no longer supplies the requested 20260909-r0 CA bundle, selecting
20261020-r0 instead. Correct the runtime/builder CA provenance and corresponding
source/recipe/notices together; do not loosen version or scan checks. That
failure also blocks the API 37 fixture. Complete image acceptance remains open.
The current core runtime is still the verified `2f9bc65`; this documentation
follow-up does not claim rebuilt candidates from a later documentation commit.

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

Open gates: complete native image rebuild/scans and CA provenance repair; API 37 and
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
