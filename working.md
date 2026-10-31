# Interlink selected-Pi dashboard — 2026-10-31

Interlink now calls the existing authenticated read-only controller/status APIs.
An explicit choice stores the phone and Pi full paired keys, display name and
last confirmed bond generation in credential-encrypted storage. Selection never
silently chooses a discovered address, changes pairings or grants Pi authority.
Confirmation rereads current full pairing; saved-choice CAS/readback prevents a
late status from replacing a newer choice or publishing connected for it.
Missing/damaged/future saved state never becomes empty setup. First run waits
for explicit phone-node setup; disabled/offline/refused routes retain the Pi.

The native header card distinguishes checking, connected, phone/Pi unavailable,
owner approval, changed/revoked identity, unsupported control and saved-state
attention. Pi service health is distinct from control reachability. No transfer
completion or Google Photos backup/deletion claim is inferred. Visible polling
is lifecycle-bound/serialized, with manual refresh, and installs no new service.
Read bodies are bounded and refuse redirects; raw server messages stay out of UI.
The existing photo-link setup remains below, with no migration/retarget of links.
See docs/interlink-dashboard.md for the exact boundaries and remaining control work.

Full Android JVM237/0 includes11 new control/selection tests. Two final native
API36 Compose component tests pass on one unchanged emulator framework in4.496s:
connection-loss/stale-running removal, service-unavailable separation and200%font
reachable/disabled actions. Light/dark/unavailable/large-text actual previews use
synthetic data and live canonically in docs/brand. The separate visual APK uses
an isolated generic Application with native producers omitted; this establishes
UI behavior, not real node startup/pairing, production signing or native release
packaging. Normal package/producer gates remain unchanged. Frozen companion
lintDebug has0 errors/11 existing warning categories. Foundation and OpenAPI
coverage pass:59 runtime operations,433 references and122 client call sites.

Initial lint attempts overlap source construction and fail in Kotlin FIR/UAST
analysis. Full test-source compilation plus stable-source lint passes, including
one final frozen-source run. The first emulator process terminates before boot
because AVD manager registers inside task ANDROID_USER_HOME/avd. Pointing the
same AVD launch at that observed registry boots normally. No observation timeout
causes a restart. The successful guest boot/system_server/surfaceflinger identities
stay unchanged through instrumentation and all captures. Failures and corrections
are retained distinctly, not relabelled as passes.

Prior697364c hostedCI41249267431 is terminal: Rust/contracts, Android foundation,
dependency delta, Mac bundle and Mac integration/UI pass. Its full maintained-
worker suite passes9/0, zero ignored/filtered, in1202.36s. API37 stops before
instrumentation: dumpsys window has no SnapshotCache Task block, so the guest
snapshot switch cannot be verified/called. Both container contracts pass before
strict Caddy go-jose High scan refusal. These are open acceptance gates; no
manual rerun, snapshot bypass, scan exemption or billing change is made.
Rust/Cargo/Go inputs are unchanged by this Android-only feature. No fresh Rust/
worker/native-device result is inferred; earlier qualified runtime evidence keeps
its own exact source boundary.

No phone/Pi access or production mutation. Preview/test packages, APKs and owned
Interlink_Dashboard_36_32899 AVD/data/registration are removed. Emulator39248 and
netsimd39284 exit normally; boot25d1cb29-8a64-4b85-b02d-fd0a6d853fad stays stable.
CleanOS interlink-bonded-dashboard-1793487630376507000-32899-0 owns remaining
compiler/log resources until closeout. Final evidence, source pin/readback and
cleanup follow privately. Shared ADB76421/5037, SDKs/caches/source/keys, other
active work and prior migration candidate stay protected. Goal remains active
for Pi owner setup, privileged settings/import confirmation, route roaming,
hosted/security/base gates, signed migration/rollback, cloud proof and gold image.

# Safe destination changes and Interlink identity — 2026-10-31

Destination changes now refuse retained unconfirmed-copy proof during consent
preflight, after exact worker reap and at actual policy reset. Renewing the same
root keeps the evidence. A late pre-commit refusal reconciles unchanged consent
instead of unnecessarily leaving that configuration stopped. Private policy
opens are nonblocking and reject writerless FIFOs. This grants no overwrite,
adoption, cloud confirmation or deletion authority.

The specific 409 folder_sync_recovery_required contract guarantees the proposed
root was never committed. Android durably drops only the matching proposal on
that explicit protocol1/nonretryable response; uncertain outcomes, old contracts,
failed saves and already-committed reset intents remain replayable. Root guards
preserve policy bytes, original media and other folders' proof. Owner review
and explicit resolution are still unfinished.

Interlink now has its own vector link mark, adaptive/themed icons, native
MaterialExpressiveTheme/MotionScheme, coherent older-Android light/dark roles,
dynamic colours on Android12+, and a serif wordmark without Back. Actual native
light/dark/200%-font previews use synthetic saved/offline Pictures data and are
retained canonically in docs/brand. This visual-only fixture omits native
producers under a temporary manifest/application identity; it is not runtime,
pairing, production signing or release-package acceptance. Normal companion
producer/package gates remain intact. Shared notification branding remains
future work; see docs/brand/README.md for scope and official design references.

Final node331/0, Android JVM226/0, Swift error-copy14/0, console92/0, denied-warning
workspace/all-target/all-feature clippy, Android26 ARM64 node/JNI, foundation and
OpenAPI route checks pass. Companion lintDebug has0 errors/11 warnings (dependency,
obsolete-SDK and unused-resource notices). The root regression fails unchanged
2e5dc0a at its intended safety assertion. The real-worker acknowledgement-loss/
collision/root-refusal/restart case passes on the final runtime in42.20s.
An earlier full nine-case guard run passes9/0 in1179.69s. The final full nine-case
run is still executing at this commit after the added precommit/committed-reset
distinction and explicit-busy setup fix; its terminal source-bound receipt will
be retained with the appliance audit and PR metadata. Original60s file assertions,
two-test concurrency and the real15minute scheduled transfer remain unchanged.

Hosted 2e5dc0a CI41244102155 is terminal: worker8/1 fails an explicitly retryable
503 on POST /sync/folders, so the test setup now uses the existing bounded exact-
request retry for that route too. Android unit copy coverage fails the missing
invalid_request mapping now fixed here; API37 stops before instrumentation when
that prerequisite fails. SharedRust, dependency delta and Mac bundle pass.
Container contracts pass before Caddy's go-jose High advisory refusal. CodeQL is
a separate successful run41244102185. No release-ready or fresh hosted-pass
claim follows these local fixes; no scan exemption or manual rerun is made.

Solstice stays away; no phone/Pi access, account, pairing, originals, network,
installed services, updater, release or gold image changed. UI AVD
Interlink_UI_36_94259, emulator5596, preview app/data and temporary APK have been
removed and independently checked. CleanOS task
gphoto-destination-proof-guard-1793483003145301000-94259-0 owns remaining compiler/
worker/log resources until full checks finish; final cleanup, signed source pin,
adapted native checking and bounded receipts follow privately. Shared ADB76421,
SDKs/caches, signing keys, other active work and the prior migration candidate
remain protected. Goal stays active for owner controls/import confirmations,
roaming, hosted/security gates, migration/cloud and physical-image acceptance.

# Console recovery request copy — 2026-10-31

Hosted b195719 CI41243265310 passes foundation, formatting, strict clippy,
full all-feature workspace tests and smoke, then refuses the missing console
message for the new invalid_request code. Added a reload instruction to the
existing authored error catalog. All92 console tests pass locally; the same
existing coverage test fails on unchanged b195719 before the entry is added.
This follow-up changes no Rust, API, worker, policy or authority byte. It is a
console message fix, not a device/UI journey or production acceptance.

Below records the runtime qualification. The first task is fully cleaned;
small follow-up CleanOS gphoto-recovery-console-copy-1793482298507956000-85466-0
owns console test logs/tmp and exact source-preparation checking until closeout.
Final source pin, cleanup/evidence and PR readback are retained privately. No
phone/Pi access or installation. Root-reset proof preservation, owner review,
current hosted gates and the remaining PRD acceptance remain open.

# Unconfirmed-copy isolation — 2026-10-31

Hosted a7d5788 run41236588580 supplied a contrary collection witness: seven
worker cases passed, one deletion barrier failed after an unacknowledged copy
left the destination requiring recovery. The local eight-case pass remains
valid only for its own run. The destination now holds uncertain unowned paths
and independent local-file collisions while unrelated new files can advance.
Full admitted source proofs remain unchanged; held files never acquire deletion
ownership from later success. The whole link remains incomplete. Restore
permission releases a hold only when the destination is absent. See
docs/unconfirmed-copy-recovery.md for safety, API and rollback boundaries.

A local-authenticated read-only recovery endpoint reports bounded redacted
counts, with null observations when unavailable. It runs metadata reads outside
the lifecycle lock and preserves admission through cancellation. No companion
review/mutation, overwrite approval or cloud confirmation is implemented here.
Two pinned Unicode libraries provide conservative case/normalization holds;
all pre-existing dependency resolutions remain unchanged.
Existing explicit destination-root replacement resets folder policy. A hold-safe
archive/refusal and owner review remain required before any production install;
never use root repair/reset as an unconfirmed-copy bypass.

Final full node suite:325 passed. Dependency advisories, bans, licenses and
sources pass. Android26 ARM64 node/JNI checking and foundation pass. Focused
real-worker recovery/restart case passes and fails on a7d5788 as expected. Full
nine-case concurrent transfer suite passes9/0,0 ignored/filtered in1180.77s, at
two-test concurrency, original60s file deadlines and the real fifteen-minute
schedule. Final strict clippy and actual receive-only Pi-adapted lib/native-entry
check pass with exact8 modifications/4 generated files and two existing entry
warnings. The48,709,632B owned source cache is independently removed. OpenAPI
routes and pinned lint pass. No linked/signed bundle or device install is claimed.

The same hosted run passes every named Covalent API37 instrumentation test
(86 baseline plus the real SAF journey), then Interlink's separate permission
journey fails at ADB input with a broken pipe. Both container package contracts
pass before the Caddy go-jose High advisory refusal. These are still open gates;
no suppression, automatic rerun or production-ready claim is made.

Solstice is away. No phone/Pi connection, production install, account, pairing,
original media, release or gold image changed this iteration. CleanOS task
gphoto-uncertain-copy-progress-1793478524895342000-61124-0 owns current temporary
builds, real worker and fixtures. Its Go SDK/archive are already removed; active
Rust/worker resources remain until checks finish. Shared ADB76421/5037, stopped
Colima, SDKs, keys and previous migration candidate are preserved. Final source
pin, evidence, cleanup and draft PR readback follow successful qualification.

# Independent continuous fanout — 2026-10-31

Draft separates continuous destination generations while preserving pending
writers' immutable proof/deadline. Source-only retained state is bounded to one
entry per destination, with one preparing inventory. Per-peer commits/reports,
expiry, restart, member revocation, settings cancellation and aggregate summaries
are handled together. Settings cancellation restores the common generation used
by manual/scheduled clients. Receiver wire fields remain unchanged. Old source
rollback refuses retained state; do not strip journals to bypass that refusal.
Only complete unchanged source authorization permits generation-only server reuse.

Both new regressions fail on prior5747776 at the intended boundary. Focused
candidate worker test passes two post-inventory edits while a peer is offline,
unchanged source-server receipt, source reopen/returning-peer catchup and original
preservation.318 full node tests, full strict clippy and foundation pass. A test
fixture's mistaken member-only pause assumption was corrected to member removal;
its earlier317/1 failure is retained. Final full eight-case worker concurrency,
Android26 ARM64 production node/JNI checking passes; actual Pi adaptation checking is pending. Original
file deadlines/concurrency and real fifteen-minute scheduled test remain enabled.

Prior hosted41234049724 is terminal: real-worker6/1 fails online fanout;
both full image/package contracts pass before strict Caddy go-jose High refusal;
API37 fails at emulator startup missing the settings service, before application
admission diagnostics. Older generation0 Manual Run Now failure remains separate.
See docs/sync-runtime-progress.md. No new dependency or advisory suppression.

Solstice is away and ADB inventory is empty. No physical-device, production,
account, pairing, media, release or gold-image change. CleanOS task
 gphoto-independent-fanout-1793475630128666000-35596-0 owns the temporary builds,
worker, fixtures and logs. Go SDK/archive removed after exact worker rebuild;
remaining compiler/worker resources stay needed for running checks. Shared ADB
76421/5037, stopped Colima, SDKs, signing keys and old migration candidate remain
preserved. Final evidence, cleanup, source pin and PR readback follow this stage.

# Receiver-copy progress and remaining batch stalls — 2026-10-31

Functional source53d1004147ccee0ebcdfb4076455d8cd0f27bd89 preserves unchanged
receiving jobs across destination-only configuration/generation updates, holding
their exact runtime/root/SAF/worker leases. Active root, peer, policy or generation
changes retain the stop boundary; source authorization still replaces its
read-only SFTP server. Finished work wakes the existing service without starving
independent health probes. Active jobs own atomic policy staging; every fresh
copy retains strict policy validation. SAF reservations unwind on prepare errors,
avoid double counting within a runtime and remain counted across overlapping
runtimes. No new dependency, process or persistent schema is introduced.

Android settings reuses the exact request/change ID only after an explicitly
typed retryable503 busy refusal, with bounded backoff. Unknown responses are not
replayed. Harness acceptance/settings uses that same protocol contract; original
file deadlines/concurrency remain.316 node tests,223 actual Android JVM tests,
full all-target/all-feature clippy, foundation, Android26 ARM64 production node/
JNI and the actual Pi adaptation library/native-entry check pass. No APK or
production bundle is linked/installed as acceptance; generated-entry warnings
remain. Worker tree9eec016c4843e1d82167cbb932e356ab030adb12 and Mac workerf68c6ae
are unchanged from the qualified worker refresh.

Full seven-case worker run:6 pass,1 fail (collection deletion barrier),0 ignored,
0 filtered. Real15minute scheduled transfer and natural worker shutdown pass.
A separate paired run passes collection/remount/permission recovery but fails
online fanout after the other destination stops. That contrary witness remains
open even though fanout passes in the complete run. The source owns one active
folder batch: stale-index failure at an online destination cannot get a fresh
inventory while another destination stays pending. No journal erase, forced peer
completion, longer file deadline or serial-only acceptance is used. The first
wake-only candidate also exposed an unrelated-copy restart and retryable-busy
fixture failure; its already-failed remainder was explicitly cancelled, not
claimed passed. Temporary timing traces are fully removed. Baseline remount
arrived at60876ms after a60000ms deadline; it remains failed.

Prior hosted CI41228352491 on1778cf9 builds/validates both container packages,
then strict Grype rejects go-josev3.0.5/v4.1.4 in Caddy for GHSA-mgcc-cww5-fp2q.
No suppression or source-provenance weakening. API37 passes86 baseline tests,
then the actual SAF Manual Run Now journey fails with both nodes at generation0
and no pending request. This differs from the older missing-service error.
New bounded saved-request/button-state diagnostics compile; no local device
execution or assumed click-race fix is claimed. See docs/sync-runtime-progress.md
and artifacts/evidence/sync-concurrency/ for exact scope and remaining work.

No phone/Pi access, production installation, media/account/pairing mutation,
merge, signed release or gold-image claim. Solstice stays outside this work.
Next: isolate source fanout generations without disturbing pending writers;
resolve concurrent collection timing and Android manual admission; fix Caddy's
separate container gate; then continue companion owner/import controls and
hardware/migration/cloud/gold acceptance. Preserve old journals and originals.

# Maintained worker and concurrent-sync qualification — 2026-10-31

Updated chi5.3.1→5.4.0 and x/net0.60→0.62 (MVS crypto0.58 plus
related x modules), all four module-digest consumers and every macOS binary/
notice/signing verifier together. Exact Go1.26.10 worker tree is
9eec016c4843e1d82167cbb932e356ab030adb12. Actual macOS ARM64 and Android26
ARM64/x64 workers build with full target notices and provenance. Mac packaging,
ad-hoc signing and repeated byte-identical builds pass. Full node library:
312 passed; denied-warning clippy, foundation, workflow and collector contracts pass.

Strict source govulncheck1.9.0 reports zero vulnerable imported packages/calls.
One required-module advisory, GO-2026-5932, concerns OpenPGP code absent from all
three exact compiled target graphs; no suppression. Stripped binary scans fall
back to possible module-level symbols and are not misreported as executed-code
findings or as clean scans. Sanitized hashes and scope: artifacts/evidence/worker-security/.

Previous CI41221509985 passes shared Rust/security, Android foundation and both
Mac gates. Both container images build but the final guard refuses a stale
packaged-evidence reference. Regenerated canonical references with the production
collector, first reproducing every old sealed field exactly; unchanged Caddy/
Go stage inputs justify preserving their installed-database digests. Added an
early source/reference hash check. Full exact-current image gates remain required.
The API37 emulator reaches Interlink permission recovery, then adb reports
missing-service/exit20 on input tap; no cause or passing acceptance is inferred.

Executed all seven maintained-worker cases: five pass, collection/remount and
three-node delivery fail under two-test concurrency. Both failing scenarios pass
separately on the final worker. A paired final-worker reproduction still fails
(initial transfer and mount return); fresh status shows running batches with
pending destinations at the original sixty-second file deadline. Cause remains
open. Added bounded failure status snapshots and the existing redacted error
sink to the test helper, retaining waits/assertions, and added the complete
seven-case gate to CI. No serialization, filtered passing acceptance, retry mask,
production shutdown rewrite or journal deletion is applied.

The scheduled case intentionally waits a real fifteen minutes and passed in the
full run. An additional redundant diagnostic was cancelled after discovering
that cadence, without a successful result claim. Stack sampling established no
shutdown deadlock. Temporary shutdown instrumentation was removed.

No phone access, production installation, media/account/pairing change or release.
The private appliance source may advance after published source identity and
adaptation checking; gold/physical/cloud confirmation gates remain open.
Task-owned SDK/builds/fixtures are tracked privately and cleaned after this stage.

# Runtime provenance follow-up — 2026-10-31

Exact-head hosted CI41220252198 passed the new Python package resolution and
BusyBox compilation/wire checks, then refused Alpine runtime evidence because
the OpenSSL upgrade also selected musl1.2.5-r24 while the source lock described r23.
The strict guard is preserved. Runtime now explicitly pins both musl and its
utilities to r24, with installed-version assertions and the complete corresponding
aports tree fd05fc094264bce4fb76efc6c7f61ba4abfcb609. The reviewed recipe adds
CVE-2026-6042/CVE-2026-107891 inputs and replaces the older iconv patch input.
Every recipe Git blob, APKBUILD SHA512, tree digest and source/notice bound is
checked; unrelated origin records are unchanged.

Actual disposable containers assert their APK architectures for arm64 and amd64.
All non-BusyBox runtime package metadata matches the lock, accounting explicitly
for the image's documented removal of apk-tools/libapk/zlib. Local BusyBox
replacement is excluded from this base-stage comparison; full image acceptance
remains hosted work. Full source/notice/bundle collection passes using real
hash-locked sources and explicitly synthetic installed/builder databases.
This qualifies source packaging, not actual final-image installation. Collector,
verifier and container-contract tests pass. Rust source/Cargo/toolchain inputs are
byte-identical to3ebbecf, whose full suite/clippy/Android checks already passed;
no duplicate Rust rebuild is needed for this container-only follow-up.

Six default-branch Go alerts select already-patched OpenTelemetry minimums on
this branch. The remaining worker go-chi/chi v5.3.1 alert GHSA-j6qf-3m9c-vwj3
needs its own reviewed5.4.0 dependency/source-digest refresh and worker validation.
No route-reachability exemption or alert dismissal is applied. Solstice remains
offline; no device changes occur.

# Appliance build security refresh — 2026-10-31

Updated the Tokio dependency floor and lock from1.53.1 to1.53.3 for
RUSTSEC-2026-0386. No workspace AsyncFd call sites exist; no unsafe code or
advisory suppression is added. Other lockfile entries/edges are unchanged.
Alpine builder/test stages now pin Python3.12.16-r0 with its matching runtime
assertion. The standalone BusyBox builder also uses the shipping builder's
OpenSSL3.5.9-r0. Existing source verification, notices and runtime gates remain.

Replaced the audit-check publisher with checksum-pinned native cargo-audit0.22.2,
using the same read-only permissions for every PR, including forks. Strict audit
warnings, failed downloads/checksums/version checks and cargo-deny remain fatal.
No Check API token or permission escalation is required.

Validation: full all-feature workspace tests pass, including312 node library
tests and actual local QUIC transfer/deduplication tests. Seven maintained-worker
integration tests are explicitly ignored; not claimed executed. Full workspace
all-target/all-feature clippy with warnings denied, formatting, foundation,
workflow lint and Android26 ARM64 node/JNI library checks pass. cargo-audit0.22.2
rejects the old lock for the exact Tokio advisory and accepts the new lock;
cargo-deny0.20.2 advisories/bans/licenses/sources passes with its configured
non-fatal duplicate-version warnings. Twelve exact build packages resolve and
Python's version assertion passes in disposable containers requested for both
linux/arm64 and linux/amd64. The Linux audit archive's checksum, member extraction
and static x86-64 ELF are independently verified; execution here uses the official
macOS build. Sanitized input/test bindings: artifacts/evidence/security-refresh/.

No phone access, device change, signed release, full container image acceptance
or gold-image acceptance. Hosted exact-head CI remains the release boundary.
Task containers and pulled image are removed and independently absent, and the
pre-existing Colima VM is restored stopped with its images/containers preserved.
Compiler scratch stays active only through ROM source-pin/adaptation checking;
cleanup evidence is maintained by the private appliance workspace.

# Appliance controller bond and signed read-only status — 2026-10-31

Added a separate local-owner controller bond and covalent-appliance/1 transport
on the existing QUIC endpoint. Sync pairing alone does not grant control. The
owner approves a full paired public identity with a generation; replacement
requires revocation. Exact grant retries are idempotent, stale consent cannot
reinstate authority, and peer revocation immediately blocks control. Private
atomic state and an exclusive lifetime lock refuse damaged/aliased/special
records and duplicate stores. Missing nullable fields fail closed.

The local bearer API can grant/revoke a bond and request the paired appliance's
signed, redacted sync status. Requests remain loopback-only, bounded to4KiB;
the remote wire uses pinned TLS, signatures, freshness, replay admission and
identity-bound replies. No remote mutation, root IPC access, source deletion or
cloud-backup claim is introduced. Reuses current dependencies, endpoint and
stream admission; no extra APK/service/thread is added.

Validation: lint with warnings denied;312 full native-node host tests; Android26
ARM64 production check and test build;9 selected security/API/actual-QUIC tests
on the physical Android36 Pi5. Both current-source host and Pi tests pass. API
coverage matches58 operations and428 references; isolated route fixtures also
cover a non-versioned path after an API path. The checker no longer assigns
that path's method to the preceding API. Test source bindings and a sanitized
receipt are in artifacts/evidence/appliance-control/.

Recovery follow-up keeps the controller on its dedicated protocol even when the
folder service is absent. Owner status remains authenticated, reports unavailable
sync with an explicit null record count, and refuses contradictory signed data.
Schema cases and actual Pi dispatch exercise this boundary. Core node/identity
startup failure and independent privileged host control remain separate gaps.

The canonical local workspace preserves the latest existing companion adaptive
work (base4a89c2f). Its Rust/Cargo inputs matched Pi-pinned1e549763 before this
change. Original synced work is preserved. Temporary physical test data, binary,
ports, Ethernet configuration and connections are removed and verified. Task
scratch ownership and final cleanup are recorded in the private appliance workspace.
A mixed Homebrew/rustup compiler
cache was discarded only within task scratch; final checks use one pinned
rustup1.97.1 toolchain. Shared toolchains/caches are retained.

The installed Pi is unchanged0.2.3; no phone access, app installation, production
restart, migration or release. Local owner approval UI, companion dashboard,
privileged settings/feature controls, attachment/preview-bound import consent,
camera previews, roaming and booted upgrade/recovery remain open. Do not widen
root IPC permissions based on an ordinary sync grant or this native status bond.
See docs/appliance-control.md for the exact read-only boundary.

---

# Adaptive companion status and home screen — 2026-10-21

Software-first UI milestone: a retained manual/paused photo link stays configured
when its worker-folder list is empty. Destination choice and repair remain
explicit; setup does not imply cloud backup. Source/destination and Refresh
adapt to narrow/large-text screens. The serif/logo home title stays on one line,
and short startup/retry content can scroll.

Exact debug/test builds and both modules' debug lint pass in120.291s. Two native
component regressions pass in3.517s JUnit / 4.852s wrapper; full foundation passes
in45.722s. An in-place update on the owned API37 emulator keeps UID and all five
checked records unchanged; all eight native objects match the prior APK.
The initial runner stops on adb's progress heading after successful app install;
its failure is retained, and a guarded continuation installs only tests and runs
instrumentation once. No data clear or original media/account operation.

Headed checks cover the actual InterlinkActivity home, normal and 200% system
text at320dp portrait width, retained paired source setup, keyboard-open pairing
and landscape controls reached by scrolling. The top bar stays fixed. Display
settings are restored and the app is force-stopped. Get started enabled the
owned synthetic host. Its replay-request file advances on host open; core/TLS/
engine identities and folder-sharing trust remain byte-identical and one paired
device remains visible. No new pair or folder grant is created.

Release-package verification passes in153.950s: unsigned APK/AAB, native
provenance, both release/debug lint and the dedicated license/SBOM inventories.
The APK is28,856,202 bytes; production signing and installed minified-app
acceptance remain open. This UI milestone
does not replay the full native journey or close the prior intermittent address
transfer stall. Hosted9330537 CI39651980156 and CodeQL39651980176 remain in flight
at this checkpoint; seven CI lanes and Kotlin CodeQL have passed. Their run is
left intact. The new milestone can be published separately without cancelling it.
See docs/interlink-adaptive-status.md and artifacts/evidence/companion-adaptive-status/.

The appliance runtime/image and physical devices are unchanged. Private Interlink
CI39652182849 atfc49db4 cannot start due to account payments/spending limits.
Real Pi/phone roaming, physical imports/cameras, exact cloud-confirmed cleanup,
production signing, natural updates and gold-image acceptance remain open.

---

# Dedicated companion native verification — 2026-10-21

The real Android folder journey now builds under the dedicated Interlink
identity and its Expressive theme. It pairs first, chooses both folders with
its synthetic peer offline, reconnects using retained trust, then exercises
native copies, address/restart recovery, lost-grant repair and link settings.
Companion-specific checks retain destination copies after source deletion and
keep destination deletions; ordinary Covalent retains its opt-in policy checks.
The generated bridge, test theme and test activity remain debug-only.

Build/lint and both instrumentation classpaths pass. Full foundation validation
passes in62.888s; all11 helper resolver checks pass, including explicit package
selection, path injection and changed native bytes. Three individually verified
Google Maven artifacts are pinned without changing prior hashes or policy.
The unchanged debug companion APK is b2bb9709a4b0c2739bb832d3d99651d2e1959efbea16b80ef76e2461aa83df86;
its eight native libraries match the previously exercised production bytes.

Two actual failed device results stay preserved: the first generic journey
expected deletion controls absent from this safer photo UI; its policy-corrected
fresh run then timed out after a coordinate tap left DocumentsUI's ALLOW dialog
open. A new exact-button accessibility action is dispatched once and still
requires the real persisted folder grant. The confirmation-corrected run obtains both grants and transfers manual/Unicode
files, then fails waiting for an authoritative changed address (184.330s JUnit).
The next test correction guards the address input/confirmation readiness and
adds bounded endpoint/button diagnostics at that exact deadline. Both classpaths
compile and the new isolated run passes the full dedicated-package journey
in458.364s JUnit / 466.038s wrapper, including both acknowledged-cycle retention
checks and removal. Exact sources, APKs, UID, boot/framework and all previous
failed evidence verify. No old pairing state or data is cleared. This single
pass does not close the earlier intermittent post-address transfer stall.

Hosted fed6107 CI39644102561 is terminal failed: seven software lanes, all86
baseline tests (72.693s) and the ordinary-app SAF journey (422.116s) pass. Both
CodeQL languages and policy pass. Permission recovery hits the same62-byte
emulator read-color-buffer-DMA abort, correlated in live and DropBox reports
2312ms after the prior successful tap; three identity digests survive Android
framework restart8816. All73 archive entries and its GitHub digest verify.
No renderer experiment or gate exemption is used; this remains a release block.
See artifacts/evidence/api37-fed6107/result.json.
The appliance runtime/image and physical devices are unchanged. Current private
Interlink CI39644383871 at eb98079 cannot start due to account payments/spending
limits. Physical roaming/imports, exact cloud-confirmed cleanup, production
signing, natural authenticated updates and gold-image acceptance remain open.
Next: actual companion entry/scaffold and large-font checks; correct the top
summary that still asks for a source when a retained paused/manual photo link
has an empty worker-folder list. See docs/interlink-native-journey.md and its
retained evidence directory.

---

# Interlink companion and appliance robustness — 2026-10-21

The dedicated photo companion now reveals source-folder/link setup only after a
retained confirmed peer exists. Ordinary Covalent and incoming/repair selection
rules stay intact. Its exact new debug APK builds and passes lint, then updates
in place on the owned emulator with the same UID and no data clear. The actual
unpaired screen shows pairing controls and no premature source/share controls.
Paired/offline/repair branches are reviewed in source, not exercised by this
small UI smoke. Package alignment/signature and all eight unchanged native
libraries verify; route and full foundation validation pass. See
docs/interlink-progressive-setup.md.

Hosted0564034 CI39635433377 is terminal: five non-device lanes, all86 baseline
tests (73.139s) and the full SAF journey (426.305s) pass. Shared foundation fails
at the archived-client gate already corrected in e01dae1; macOS UI is cancelled;
permission recovery and release gates fail. CodeQL Java/Kotlin passes; Swift is
cancelled and its policy check skipped. Swift log retrieval returns404, so its
cancellation cause is not established. The permission report now correlates
live capture and DropBox with system_server1785/TID2018 signal6, 1983ms after
Open settings returns0, followed by Android's framework restart8838. Its 62-byte
abort SHA21459e59cfa55a94251c82e5d7e559328fced4bbbff93a4f139eb68d27d56bee
exactly matches the existing workflow's known read-color-buffer-DMA assertion.
The native stack includes mapper.ranchu.so; all three identity digests match.
Artifact12364873359 verifies SHA548c6b4a9de8420de0fdd3bdda09ae29b99c3cffc67942263e427ae878fa17d5
and all73 entries. No new fault, renderer change or exemption is used to obtain
this diagnosis. See artifacts/evidence/api37-0564034/result.json. A supported
permission-test environment is still required; release/hardware gates stay shut.

The intermittent post-address Android transfer now retains both endpoints'
bounded state if its existing deadline fails, correlated to the exact original
request ID. No retry, input replay, deadline change, permission/data reset or
production APK change is introduced. The separate owned ARM64 diagnostic
journey passes in437.554s (JUnit435.768s): real manual/Unicode/address/restart
transfers, lost-grant repair, settings, pause/resume, deletion and removal. Its
boot/framework, app APK and captured sources are unchanged. The earlier failed
journey stays byte-identical; the new failure branch is not exercised. The
intermittent stall remains unresolved, and release acceptance stays closed. An initial instrumentation build omitted the packaged-engine
flag and fails before pairing; its corrected build and receipt stay separate.
See docs/android-address-recovery-diagnostics.md. Primary0564034 CI39635433377
continues; e01dae1 and this follow-up stay on separate branches meanwhile.

Hosted source0564034 CI39635433377 has a terminal shared-foundation failure:
three retained historical .source snapshots are classified as undeclared API
clients. The follow-up gate admits only those three exact paths and SHA-256
values. Modified bytes, removed API text and adjacent unknown files still fail;
current shipping clients keep all route/method checks. The actual route gate,
existing fixtures, three new negative cases and full macOS foundation pass.
See docs/frozen-client-evidence.md and
artifacts/evidence/frozen-client-evidence/manifest.json.

The follow-up publishes on codex/frozen-client-evidence while the original
primary run's Android/container work continues. Primary PR51 stays at0564034;
no push cancels its active jobs. No app, native worker, Pi image, physical device,
account, pairing or media change. Address-change transfer recovery and the
previous hosted framework abort remain unresolved; release gates stay closed.

Android share acceptance now handles the node's explicit temporary lock refusal
without making the user repeat setup. It keeps the exact destination and
authenticated request, permits at most six attempts with 3.1s total backoff,
and never replays uncertain network results or other sharing operations.
Cancellation stops retries and preserves the interrupt flag. The server's
nonblocking mutation lock and durable destination journal remain intact.

The original client fails the new deterministic busy-response regression.
All 222 JVM tests now pass, including ten new acceptance cases.
Explicit typed error fields are required; inferred defaults and coerced values
cannot authorize replay. Earlier source/results remain separate. Covalent and
the dedicated Interlink debug APK compile, pass lint, and pass native package,
16-KiB alignment and signature checks. Seven native-path checks pass on macOS
and root Linux. QA now explicitly supports ARM64 while retaining its x86_64
default; damaged ARM64 bytes cannot be replaced by valid Intel bytes.

The earlier full SAF journey passes on fresh emulator-5606, including transfers,
restart/address changes, lost-access repair, shared settings, pause/resume,
synthetic deletion semantics and removal. Its boot/framework identities and
source/failed-preflight receipts remain unchanged. Final strict-source checks
stay separate: the reused paired fixture cannot expose its first-time pairing
form; a new fresh fixture then exposes a chooser/status readiness race in QA.
Both failures remain terminal. The corrected test waits for the real displayed,
enabled chooser before one click and uses the same production APK, without
clearing data. The final-source run passes pairing/acceptance and the first
manual transfer. The changed peer/worker ports preserve identity and the new
address becomes authoritative, but the next requested file misses its deadline.
This remains a failed journey (200.369s; JUnit 198.698s), with unchanged
boot/framework, sources, production APK and original failed receipt. Later
recovery stages are not reached. The next software investigation is this
address-change transfer; its cause is not yet established.

The chooser-test rebuild also records a separately diagnosed macOS offloaded
Go source file. The exact owned blocked Go-list child is terminated after a
matching executable is restored from its local archive outside cloud storage.
The new build passes in 1m2s with pinned Go1.26.10 at
/Users/michaelwong/Library/Caches/Covalent/toolchains/go1.26.10-0b5183492292/go.
The failed build and source/archive hashes remain separate.
See docs/android-folder-acceptance.md and
artifacts/evidence/android-acceptance-busy/manifest.json.

Parent 69ff050 CI 39622053150 is terminal failed at acceptance; its seven
non-device jobs, CodeQL, version sync and 86 baseline Android tests pass. Its
permission/native-report phase was not reached. The prior framework abort
remains unresolved. The new source needs hosted CI before release. Initial
wrong-module and wrong-Go build attempts remain separate. No physical device,
original media, account, Pi image, production signer or release gate changed.

An actual Android 37 native fixture exposes a diagnostic parsing bug: report
headers include an optional parent PID between victim PID and TID. Both readers
now share a strict grammar accepting that bounded numeric field while ignoring
its value. Existing correlation/privacy requirements remain intact.

On fresh emulator-5600, one separate owned native fixture aborts once. The 8a17a76
reader rejects its real in-window header; its original failed receipt remains.
A separate read-only follow-up matches that same existing report after the fix,
without replaying the fault, and keeps boot/framework PID/start-time identity.
Only the exact verified owned executable/directory is removed. All 73 focused
macOS/Linux checks pass. See docs/interlink-native-report-format.md and
artifacts/evidence/native-report-parent-header/manifest.json. This affects QA
diagnostics only; it does not establish or fix the hosted framework abort.
The earlier 8a17a76 CI run 39615977362 finished failed. The tested format fix was
promoted after that terminal result; its 69ff050 run failed earlier at folder
acceptance as recorded above.

Hosted d09d08e run 39609173300 is terminal: seven non-device lanes, 86 baseline
tests and SAF pass; permission recovery and the release gate fail. The observed
system_server1823/TID2059 receives signal 6 after Open settings returns 0 and
before the failed Permissions tap. Android restarts the framework. Three
synthetic identity hashes remain unchanged. Both DropBox reads are complete
and nonempty but neither correlates a report. The cause and mismatch reason
remain unknown. Artifact 12352067988 verifies ZIP SHA-256
4ab147806460971083683d5e962d81ef169f740598f2c1b3a4489ddc0089df65;
all 72 files/tested sources are bound in artifacts/evidence/api37-d09d08e/result.json.

The diagnostic follow-up adds bounded fixed rejection counts without retaining
rejected text or changing admission. All 70 focused macOS/Linux checks pass,
including five new counter/privacy/strictness cases. The local API 37 probe is
unavailable because its exact emulator5592 transport is absent; no fixture is
restarted, and the earlier smoke is not relabeled. See
docs/interlink-permission-qa.md. No app/runtime or release change is claimed;
physical devices, image, accounts, pairing and media remain unchanged.

Hosted bf25758 run39601425204 is terminal: seven non-device jobs, 86 baseline
tests and SAF pass, while permission recovery and the release gate fail.
The live recorder sees system_server1775 signal 6 after Open settings returns0
and before the failed Permissions tap. Its live five-second drain yields no
recognized native dump; no cause is established. The three synthetic identity
digests remain unchanged. Artifact12347813609 has verified ZIP SHA-256
df2f80f7ac8de66682e248228689288c037861a0c261b94b48b81809522dbceb.

The diagnostic follow-up adds independent bounded reads of the existing Android
DropBox native reports on a failed owned-emulator journey. Matching requires the
same boot, capture-window native timestamp, observed PID/TID/process/signal and
complete recognized service output. Raw reports stay in memory. Shared decoding
keeps only approved metadata; private paths, symbols and messages stay discarded.
Byte/deadline/read failures and source/boot drift cannot produce a match or
relabel failed permission recovery. No APK runtime, renderer flags, acceptance
criteria or device data are changed. See docs/interlink-permission-qa.md.

All 65 focused tests pass independently on macOS and Linux: 32 permission
harness, 27 report-reader/parser and six forward-cleanup checks. The report
checks include actual owned child processes for EOF, nonzero exit, byte limits,
silent hangs and overlong-line suffix rejection. Final-source read-only API 37
smoke reads both real empty report tags, preserves the boot, framework PIDs/start
ticks and all three app identity hashes, and stops the live log client cleanly.
Its impossible synthetic victim only exercises no-match classification; it is
not a real native crash or proof that hosted crash details are available.
Source/evidence manifest: artifacts/evidence/native-report-fallback/manifest.json.

The shared Android unlock gate now closes a missed-unlock registration gap and
prevents repeated/concurrent notifications from starting the same action twice.
Credentials and existing opt-in/ownership checks stay intact. Seven deterministic
gate tests pass; five checks fail against the retained former-callback model,
which is not an old-APK reproduction. See docs/interlink-startup-recovery.md.

The full Android build passes in 6m55s: 212 debug/two release unit tests, both
apps, lint, native provenance and package checks. Thirty permission-harness and
six cleanup host tests pass. Five actual Android storage checks pass; sealed-
storage cases use a context double rather than a real locked user. On fresh
Interlink_Unlock_API 37/emulator5592, both actual permission routes pass with
unchanged identities/framework. The final corrected reboot journey passes:
one reboot, native ready API and foreground service return with Activity
unresumed, 21.126 seconds stable, same APK/UID/three identity hashes, verified
owned-forward absence/removal, and final pass publication after cleanup.

The first reboot's native behavior passed for20.373s but its overall receipt
remains failed because cleanup mistook an absent forward for an error. A separate
same-boot follow-up proves native readiness, no remaining forwards, exact owned
removal/readback and already-absent handling. The corrected full journey uses
new receipts. Original failed build/admission/reboot evidence stays intact.
Source-bound evidence: artifacts/evidence/android-unlock-startup.json.
Debug APK SHA-256: f7f6b62362c8abfaf5d4b132b9fa17c32a48a270ace11d314d94cdc8bfb6ab9b.

This is a software/emulator milestone. Hosted x86 API 37 permission recovery,
real first unlock, phone/Pi hardware, roaming and production signing remain
open. Original Android fixtures, signed-in devices, drive, image and source
photos are unchanged. No release or main merge is promoted; the goal stays active.

Hosted d57deda run39562643351 is terminal. Seven non-device jobs, all 86
baseline tests and the SAF journey pass (437.290 seconds); permission recovery
and the combined release gate fail. The new recorder sees system_server1751
receive native signal 6, 1.472 host seconds after Open settings returns0 and
before the failed Permissions tap. Android restarts the framework itself.
All three synthetic identity digests remain unchanged. Five seconds of continued
capture produce no recognized native dump/header/frame/abort-summary events.
That absence does not establish why the native abort happened or why details
were unavailable. Permission restoration also fails while services are missing.
Artifact12329751390 is retained with verified SHA-256
7d767dcce5f8f3b790a41f190721e843f9f8973152f7146bacb09300ef713927;
artifacts/evidence/api37-d57deda/result.json binds this exact source and separate
gates. The original recorder/source receipt and preceding failures remain intact.
No app/runtime change, input replay, guest restart or release acceptance occurs.

The native framework recorder now retains bounded approved dump metadata after
an observed fatal signal in a known process. It correlates the separate logger
and victim PIDs, refuses unknown/mismatched/malformed headers, and emits only
fixed platform-library names, relative code offsets, bounded build IDs, fixed
symbols and redacted abort-family/hash/length. Raw messages, thread names,
registers, arbitrary paths/symbols and private libraries remain discarded.
Fault/context/frame counts are bounded; omitted frames remain visible. Capture
can drain the same stream for at most five seconds after a known native fault;
stream loss during that drain still fails continuity. No input is replayed or
acceptance deadline extended; no guest/app/service is restarted.

All thirty host tests pass with resource warnings treated as errors. Eight new
regressions include real child-process delayed records, loss during drain,
privacy, victim/logger correlation, malformed context reset and bounds. The
final-source read-only Android36 smoke on the retained boot reads two idle
lines and one safe capture marker, stops its owned client/reader cleanly and
preserves boot ID plus framework PIDs/start ticks. It has no native dump and
is not proof of the API 37 cause or permission recovery. Its source-bound receipt
is artifacts/interlink-qa/native-framework-smoke-1792566867768376000.json;
the earlier-source smoke is retained separately. See docs/interlink-permission-qa.md.

Hosted 1e54976 run39558337954 is terminal: seven non-device jobs, all 86
baseline tests and the full SAF journey pass (415.979 seconds). Permission
recovery fails while opening app settings. Its Open settings tap returns0;
2.024 host seconds later the known system_server651 receives native signal 6,
and Android starts system_server8025 on the same boot. All three synthetic
identity digests remain unchanged. Timing establishes no cause or trigger.
The previous e0dfc1c abort preceded its failed tap; the two runs are retained
separately. Artifact12327728727 has verified ZIP SHA-256
3fd06c816c2d000efac7ecb971306c7be264f1739326d7586ecdf8636e67a404;
artifacts/evidence/api37-1e54976/result.json binds the source and distinct gates.
The recorder follow-up was pushed separately as codex/native-framework-details
while this exact run remained active, avoiding concurrency cancellation. It
then advanced the review branch for the d57deda check recorded above.
Permission recovery and the release gate remain unaccepted.

Hosted e0dfc1c run 39552976561 is terminal. All seven non-device lanes,
all 86 baseline instrumentation tests and the complete SAF folder journey pass
(the latter in 422.361 seconds). The corrected idle/manual observation now has
actual hosted journey evidence. The permission journey remains failed. Its
continuous recorder sees system_server PID657 receive native signal 6 at
06:40:10.236, before the failed Permissions tap starts (1.508 host seconds
later). The tap and subsequent permission restoration report exit20/missing-service.
This establishes an earlier core-framework failure, not its cause or trigger.
All three recorded synthetic identity digests remain unchanged at failure.
No tap is replayed, deadline extended or framework restarted by this follow-up.
Artifact 12325408311 is retained with verified ZIP SHA-256
7f28229f6fbf26d388bd2ab20df9e84bfe058c58919f9290e87b894bd2e7710d;
artifacts/evidence/api37-e0dfc1c/result.json binds the exact source/run and
separate passing/failing gates. The earlier focus/manual failures remain
retained; this run does not establish their causes. No runtime change, main
merge, device installation or production release is made by this evidence.

Manual pairing now reports a missing local transport endpoint as the existing
409 peer_endpoint_unavailable response. Previously pair/accept alone mapped
that normal offline state to 400 invalid_contract, misleading clients into
reporting a version/request problem. The appliance's real offline-startup test
exposed this gap; its older pinned native core is not replaced by this change.
The new real-router regression reproduces 400 before the repair, then verifies
missing endpoint/certificate refusals, authentication, reuse of the same valid
invitation after endpoint restoration and unchanged identity/trust settings.
All 303 node library tests and 10 actual network-pairing tests pass. API route
inventory, OpenAPI lint and Rust formatting pass. No APK/device update or
release is published by these tests. The 07f1295 hosted run 39549469177 is terminal: seven non-device lanes pass,
but one of 86 baseline tests fails at Espresso pressBack because its root window
has no focus (CovalentAppTest.kt:96). The SAF and permission journeys are not
reached. No cause is established and no product regression is inferred.
Artifact 12323623061 is retained with verified SHA-256
a9543799a00d849f4f640a1ab8f9a89bbe1523d6a350432f91ce82afd6616090.
The complete release gate remains closed.

Software work continues first at the user's request. Hosted 97bf199 run
39541339765 is terminal: seven non-device lanes and all 86 baseline device
tests pass. The SAF journey fails at Manual Run Now after exact grant repair
with folder_sync_unavailable. Its later permission/continuous-recorder journey
is not reached; the combined release gate fails. Artifact 12320881159 is
retained with verified ZIP SHA-256
6f04dd0b96400d406ffd2335db78f4be4fcb9808a0e02a0579c121309f562867.
No cause is assigned to the unavailable repaired API.

The 2c5fe4d run 39546668502 is now terminal: seven non-device lanes and all
86 baseline instrumentation tests pass, but the SAF journey fails before its
manual mutation. The exact repaired grant and READY share are present; sync is
AVAILABLE, STOPPED and has no issue. The added RUNNING requirement is wrong for
an idle manual link, whose worker starts only after the explicit run. Native
service tests and Android status contracts both define that valid idle state.
This finding does not explain the older 97bf199 manual-run rejection.
The downloaded artifact 12321909085 is retained with verified SHA-256
fa22c9f93935222acd693d50194f0be5bc4b22b1456055e73a609d6718642b83.
Its native diagnostic log is empty; the permission journey is not reached.

The corrected observation accepts only AVAILABLE with STOPPED or RUNNING and
no issue, together with the exact repaired grant/share checks. The same refreshed
connection must satisfy all conditions for the unchanged five seconds within
90 seconds. Transient errors reset that interval; no manual mutation is retried.
Safe errors now identify the allowlisted API code and clear prior errors after
a successful read, so historical 503 responses cannot masquerade as the latest
sample. The test APK builds in 11 seconds, all 18 focused Android lifecycle/status
unit tests pass, and the native idle/manual-batch contract test passes. Test APK
SHA-256: 82f8813947c6d99c030886cdbb8efe1de5f4e6feaab1ca2c6b9e22bb1b6b8885.
This is a test-observation correction, not an app-runtime change or a full SAF,
permission-journey, physical-device or release pass. The retained 07f1295 hosted
run was allowed to finish before this follow-up advances PR51. The corrected
idle-manual observation was also pushed separately as codex/idle-manual-repair;
it does not change app runtime or claim to repair the unrelated focus failure.

Earlier hosted source d9eb263, run 39526795075, is terminal: seven non-device lanes and all 87
baseline/SAF tests pass, but the permission tap reports exit20/missing-service.
The subsequent framework observation has no system_server PID. The full gate
fails; neither crash cause nor tap delivery is established by that evidence.

The current follow-up starts a bounded, redacted framework log stream before
APK installation/permission mutations, waits for actual stream readiness and
records phase times around the one input attempt. It captures only approved
crash/watchdog/low-memory/process metadata; no raw logs, messages, arguments,
account labels or paths are retained. Events and remembered PIDs are each
bounded to 256; dead/reused PIDs lose attribution. Capture remains active through
failure observation and cleanup. Premature loss invalidates an otherwise passing
journey. Only its owned adb log client is stopped; no guest restart, input replay
or acceptance bypass is introduced. All 22 host regression tests pass, including
real managed subprocess readiness/loss/cleanup checks with resource warnings
treated as errors. The first real Android36 read-only smoke captures 428 lines,
retains one safe phase event, stops its reader/client cleanly and keeps the same
boot/framework PIDs. The final-source Android36 read-only smoke captures 396 lines, stops its
reader/client cleanly, reports unchanged source bytes and preserves boot ID and
both framework PIDs. This is recorder validation, not a permission-journey pass.
A separate historical crash-only observation attributes a fatal TimeoutException
to the previously observed system_server PID5529, followed by application
DeadSystemException failures. It establishes neither the timeout component nor
the trigger; no Covalent causation is inferred. The latest API 37 hosted journey
remains required. App/APK runtime bytes are unchanged.

Earlier hosted source 126cec9
run 39520547422 is terminal: seven lanes pass; the API 37 lane passes all 86
baseline tests plus the SAF journey, then fails at the permission harness's
Open settings tap (exit 20). The combined release gate fails. Its error output
was discarded; cause and tap delivery remain unknown, not an established app
regression. Private evidence: hosted-126cec9-failure.json and the retained ZIP.

The permission harness now records safe failure categories, exit codes and
output hashes, then observes fresh allowlisted UI state, framework IDs and
identity hashes before cleanup. Nonzero input still fails; there is no input
replay, guest restart or bypass of either recovery route. Cleanup failures no
longer discard the primary error/receipt. Nine no-device regression checks pass
and join Android foundation CI. The final headed API 37 ARM64 run passes both
real grant routes, native ready/foreground-service checks, unchanged three
identity hashes and stable boot/system_server/surfaceflinger. Its receipt binds
the actual harness SHA-256. App runtime/APK bytes are unchanged. Evidence:
artifacts/interlink-qa/api37-input-diagnostics-final.log and
api37-input-diagnostics-final/permission-recovery.json.
Latest-source hosted acceptance remains required. See
docs/interlink-permission-qa.md for what the diagnostics can establish.

Hosted 1481c16 run 39514337288 passes Android foundation, Rust/contracts, both
containers, dependencies and both macOS jobs. Its device job passes all 86
baseline tests and the SAF journey, then fails at the added permission
harness's ADB grant command after opening Settings (exit 20). The full gate
fails; the grant failure's cause is not established. The QA harness now uses
the actual Permissions → Nearby devices → Allow controls and returns through
Settings without launching the Activity or starting its service. Back actions
are bounded independently of unavailable hierarchies, and the screen after
the final Back is observed. The headed API 37/ARM64 rerun passes both Settings
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
jobs in run 39504560766: Android foundation, API 37 device, both complete
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
The exact committed `6177379` minified APK passes API 37 cold launch and initial
permission denial/grant checks on the separate headed Interlink_QA_API 37 AVD.
Its hosted run 39504560766 now passes all software gates above.

Permission recovery follow-up: a real revocation on API 37 reproduced a startup
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
passes on the headed API 37 ARM64/4 KiB AVD. The minified QA APK passes both
recovery routes and installed-byte readback. After an actual emulator reboot,
the foreground service and UDP 18887 listener return before opening the
Activity; the launcher remains foreground. All three protected identity-file
hashes remain unchanged after minified recovery and reboot. QA retains the
existing Android Debug signer. This documentation commit does not rebuild or
relabel those runtime bytes. Latest-head hosted follow-up remains separate.
Receipts: artifacts/interlink-qa/permission-recovery-6b31fde-build.json,
api37-permission-recovery-6b31fde/permission-recovery.json and
interlink-6b31fde-qa.json in the same private evidence directory.
The full cross-app gate caught two missing explicit API 37 guards and the
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
