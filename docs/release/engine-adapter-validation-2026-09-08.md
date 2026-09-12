# Maintained-engine adapter validation

Date: 2026-09-08. This records the maintained-engine implementation and real integration
proofs. Native package/UI integration and release acceptance remain active. [ADR 0007](../adr/0007-maintained-folder-sync-engine.md)
selects the backend and defines the remaining product work.

## Implemented boundary

The Unix Rust adapter now creates and reopens a per-installation Ed25519 engine
identity through Covalent's existing platform key protector. The authenticated
envelope is bound to the canonical installation root. Missing, copied,
wrong-key or damaged identity state fails without generating a replacement
against an existing database. An exclusive installation lock remains held by
the controller and the process reaper through actual worker exit.

Each session creates a fresh mode-0700 temporary directory, mode-0600 engine
configuration/certificate/key files and a random private API credential. The
engine uses an owner-only Unix socket on Linux/Android and pinned numeric
loopback TLS on macOS as described below. The Unix client rejects symlinked endpoint
ancestors, checks directory/socket identity before and after connect, verifies
the peer UID and sends the key only in a sensitive request header. Exchanges
have a five-second whole-request deadline; responses are bounded at 2 MiB,
with a separate 8 MiB allowance for complete configuration. Redirects and
compressed responses are rejected. The positive full-folder scan endpoint has
an explicit exception: `/rest/db/scan` alone can run for up to 24 hours, inside
an aggregate 24-hour initial-scan task. Its owner can cancel the request and
reap the worker. All ordinary API exchanges retain their five-second limit.

Configuration admits only explicit device/folder/member sets and direct
numeric addresses. It disables discovery, relays, NAT traversal, browser,
telemetry, crash reporting and upgrades. Startup verifies the actual pinned
version, certificate-derived device ID and effective security, membership,
retention and filesystem settings. Missing or contradictory controlled fields
fail closed; unrelated upstream defaults may coexist. Overlapping roots,
filesystem root and roots containing private state are rejected.

Startup now renders every authorized folder active while pausing all peers and
disabling all sync listeners. One owned task requests a complete scan of each
folder, revalidates retained roots around each request, and requires fresh
health for the exact complete folder set. Only then does the controller verify
the still-inert configuration, apply its exact desired listener/peer state and
verify the resulting configuration and identity. Status reports
`initialScanning` until that promotion succeeds. Cancellation, changed desired
folders, failed scans and failed promotion reap the old worker; a restart scans
the complete replacement set again. A passing fake-backend lifecycle test is
not recorded as real-worker acceptance.

## Checkpoint 26 integration checks

Before the Android Folders integration, the combined initial-scan, server UI
and Docker address changes passed 841 Rust tests across 22 top-level suites,
strict workspace Clippy, formatting and foundation contracts. Nine isolated
entrypoint tests include custom host ports, IPv4, IPv6 and invalid ports.
The server UI passed 101 web tests and an actual delayed browser form submission;
peer-supplied markup remained literal text and the console had no browser
warnings or errors. The delayed browser proof used a bounded fake node.

The Android Folders screen and its private grant journal are now integrated.
Personal debug builds expose an explicit special-storage-access choice before
the raw-folder picker; release builds retain the unsupported state and do not
declare that broad permission. SAF backup access remains independent. Grant
records are committed before network mutation, preserve explicit JSON nulls,
and reuse a pending folder UUID. The foreground service serializes filesystem
and JNI work on an owned background actor, keeps foreground state through
restart, and retains the exact handle and process ownership through failed
stops. The integrated JNI rerun passed all ten tests and strict workspace
Clippy. Swift 6 source typechecking also passed. Android Kotlin/Compose,
instrumentation and complete native journeys still require hosted execution.

The initial-scan acceptance gate passed twice against the actual pinned macOS
arm64 worker and production `NodeRuntime`, with all five relevant integrated
production-source hashes matched before accepting the result. A bounded tree
of 120,000 empty files kept scanning observable: the listener remained unbound
and the recipient had no sentinel; pause cancelled the scan and reaped the
worker; resume promoted and transferred both directions. A cold restart repeated
the scan barrier. Both runs preserved selected files and released both ports,
private runtime directories and all owned processes; both temporary fixtures
were removed. This proves macOS localhost controller ordering, not Android
lifecycle, removable-storage behavior or a performance benchmark.

Evidence manifest SHA-256:
`c7425a2497dff5b5c56f9fa310534f65480199afdb368431b71e050c9f48ba61`.
Harness SHA-256:
`c26b4d5aa92ae93bcb83c177a0da82b9caf6c993b518fce30927905675d00a98`.
Result hashes:
`34ca186c155d39def5878d29c1535d11015f32e09fe0087531aba3f212f6d110`
and `4e376aaa655c4b35a058ce93a0dd19a6b63a1ee528c31db9f256b641777b5add`.

Both Docker architecture jobs now include an isolated two-node packaged-folder
test using unique labelled volumes, a unique network, dynamic loopback HTTPS
ports and resource-limited rootless containers. It covers consent, retry,
forward transfer, pause/resume, durable restart, reverse transfer, local removal
with file preservation and exact-resource cleanup. It has not yet executed on
checkpoint 26; syntax validation alone does not satisfy the Docker gate.

The supervisor verifies manifest digests through bounded no-follow file reads,
rechecks executable identity at launch, and owns one direct guardian with a
stdin lifeline. Its dedicated OS reaper starts before the worker and retains
the private runtime directory, installation lock and folder descriptors until
confirmed child reap. Cancellation/drop closes the lifeline. A bounded stop
timeout means still stopping, and keeps ownership intact. Only the existing
OS home and explicit non-secret worker environment settings are retained.

## Actual macOS session tests

Both tests used the official Syncthing v2.1.3 macOS arm64 executable, SHA-256
`6743a0efbf9d39784c7fe2e925505cd0a839458346444aa28620341e47ba12b8`.
The guardian source SHA-256 was
`c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579`;
CLT clang built it with strict warnings and a macOS 15.0 minimum deployment target.

| Scenario | Result |
| --- | --- |
| Two complete managed sessions with a cold database/identity restart | 14 checks passed: real version/identity/effective-config verification, private API health, repeated stop, confirmed reap, runtime cleanup, exclusive installation ownership and protected identity retention |
| Two independent managed installations sharing one folder | 9 checks passed: exact 65,536-byte initial transfer, reverse file creation, deletion propagation, private API health, both workers reaped and all owned fixtures removed |
| Effective configuration against the real upstream REST API | Initial 5,515-byte and desired 8,094-byte responses accepted; controlled-field mutation tests reject security/membership/retention drift |

The first session attempt failed before launch because temporary-directory
creation required explicit private permissions. The next exposed upstream's
initialization-time requirement for the existing OS home after environment
clearing. Both failures were preserved, corrected and followed by the passing
real session runs. No global home setting was changed. Failed and successful
test fixtures were removed only after no owned runtime lease remained.

The focused adapter suite contains 37 tests covering request bounds/auth,
cancellation, effective configuration, protected identity, installation
corruption/copy/replacement, and supervisor ownership. The local all-feature workspace passes 723 tests across 22 top-level suites
with zero failed/ignored tests (child-process probe output is not double counted),
strict workspace Clippy, formatting and foundation checks. cargo-audit scans
293 dependencies with warnings denied; cargo-deny passes advisories, bans,
licenses and sources. Checkpoint `92685a3bccf12057227d9eaa64597150f450d29e`
subsequently passed every [hosted software gate](https://github.com/thekozugroup/Covalent/actions/runs/34230314134),
[CodeQL and policy checks](https://github.com/thekozugroup/Covalent/actions/runs/34230314216),
and release-version synchronization. That is the adapter baseline, not the later integration source.
Raw logs and reproduction probes remain in ignored validation artifacts.

## Durable consent and production runtime integration

The current integration introduces signed offer, acceptance and source-commit
records, an authenticated durable journal and automatic delivery over the
separate `covalent-quic/4` protocol. A recipient chooses its own folder before
acceptance. Both nodes persist their required consent before starting an
eligible worker. Signed records bind the confirmed Covalent identity, exact
retained transport and canonical engine identity/direct address. Backup roles
are not required for folder sharing. Exact retransmissions survive restart;
revoked/re-paired grants cannot reactivate old permissions.

The serialized service closes the old worker's lifeline before waiting for
reaping, then derives new settings from the current durable journal. Failed
launches retain the committed decision and report attention; periodic outbox
reads do not automatically restart a worker stopped for a health failure.
The runtime owns delivery, health observation and shutdown. Authenticated local
API routes expose redacted status and explicit offer, accept, pause, remove
and retry operations. Invalid installed packages preserve the backup runtime
while reporting unavailable folder sync.

The native package integration snapshot passed **814 Rust tests across 22
top-level suites**, including 505 core and 228 node tests, with zero failed or
ignored. Workspace Clippy with warnings denied, formatting, foundation checks,
47 runtime/OpenAPI operations and 334 resolved schema references pass locally.
Current macOS sources also typecheck together using the installed CLT SDK.
Fresh hosted native and exact package execution remain required.

| Real execution | Evidence |
| --- | --- |
| Serialized managed service | 26 checks passed with exact pinned macOS executables: pending consent starts no worker, two-way exact bytes, fresh folder health, pause/resume, cold journal/identity restart, revocation, retained files and complete cleanup. Result SHA-256 `d00cf6fa34b83e763ffb6e4b3d1276db9c6250050f226d0ceea959e3525e3076`. |
| Complete NodeRuntime and HTTP/QUIC flow | Two real production runtimes paired over the network API; unauthenticated mutation returned 401; an exact retry retained the invitation ID; the recipient observed automatic v4 delivery and accepted through HTTP. Initial convergence, a paused edit withheld for seven seconds, resume, recipient cold restart with reverse convergence, source peer revocation and file preservation all passed. Both runtimes stopped, sync ports were reusable, owned runtime directories were empty and the fixture was removed. Result SHA-256 `f9120b52f4b89bf8b24342271f645d7e71712d7db4748fc508a9c86483baa5b4`. |
| Actual engine health and recovery as a new copy | Six checks plus cleanup passed: exact version listing, preserved current and archived files, new-copy recovery, and refusal of a destination collision. Result SHA-256 `31adecbc7539e93588cfc6bb0de4c659f0d9cdd794c97607aa0e72cee4662e10`. |

Version recovery intentionally creates a new file. The upstream in-place
restore experiment consumed a selected archive; it is not the product's
recovery operation. The bounded recovery primitive verifies a complete version
listing, opens archive paths without following symlinks and writes a fresh
exclusive destination with file/directory durability. Native recovery UI is
still required.

## Android executable and dependency evidence

At `024afda5c3b87da907f0deb1533ff8d481368928`,
[Android proof run 34237780913](https://github.com/thekozugroup/Covalent/actions/runs/34237780913)
passed both the target supply-chain job and the exact two named API-37 tests:
`guardianOwnsDirectWorkerAndStopsItOnOwnerOrGuardianDeathThenRestarts` and
`extractedHelperRunsPrivateOfflineApiAndRetainsIdentityAfterColdRestart`.
The framework process identities remained stable. Both guardian ABIs depend
only on Android's platform `libc.so`; the earlier `libdl.so` check failure was
resolved with the reviewed linker configuration before the passing run.

| Android ABI | Syncthing bytes / SHA-256 | Guardian bytes / SHA-256 |
| --- | --- | --- |
| arm64-v8a | 30,787,168 / `ac00818b067068f975ddc0fe41d69090465de4d88f9ce2ba73502e2ce0c6d850` | 14,912 / `cbbd6c6c9ada29b8bd8ad82e46d84da27d01e5ecc97823194fa15ae0549fc73d` |
| x86_64 | 32,646,984 / `6b06dff4bde028e7cfe3fd9bc47793528f9e3be7ef9923a7e302489da05e6d9b` | 14,616 / `249d6b42e310b0570f9db28fe811442d33e1101ef68f98c106435632b21dd2ab` |

The scanner examined actual Android target dependency graphs from an owned
export of the pinned commit after generating the two official GUI assets.
Four known `golang.org/x/crypto` findings were retained with exact checked
package-absence dispositions (SSH/OpenPGP); this is not a zero-vulnerability
claim. New, changed, package-level, called or incomplete findings fail the gate.
At `5c157f37ce8b172de633ff3e9ed95b680e8a8c94`,
[proof run 34241597202](https://github.com/thekozugroup/Covalent/actions/runs/34241597202)
also passed collection of 95 license/notice candidates across 60 compiled
modules (465 arm64 and 467 x86_64 packages), with no missing root-license
evidence. This is an inventory requiring review and notice aggregation, not
a license-approval conclusion. The same two runtime tests passed again with
stable framework process identities.
The executable tests ran on x86_64 API 37. arm64 execution, production JNI
controller integration, folder access and foreground-service survival remain
separate acceptance gates.

## macOS sandbox transport

The signed app-sandbox socket experiment found that both `NSTemporaryDirectory`
and Darwin's per-user temp function resolve inside the app's container. In the
fixture the base was 94 bytes and the proposed private parent was 106 bytes;
Darwin's Unix socket bound cannot accommodate the final socket. A unique short
`/tmp` parent was denied with `EPERM`. Result SHA-256:
`a173bd10a4c1d597be75100de8142805a058caa18b3cc7a711b70bd4db61fe50`.
Owned app bundles, data and processes were cleaned up.

This blocked the packaged macOS folder engine at checkpoint `68bf16d`. Native
host failures preserved backup startup and showed sync needing attention. The
following integration replaces macOS Unix control with numeric loopback TLS.
Each worker session receives a fresh API key and a separate fresh GUI
certificate; the client verifies localhost and the exact leaf before sending
HTTP. Private files stay in the authorized container directory. Linux and
Android retain Unix control. There is no unencrypted fallback or certificate
verification bypass.

The actual pinned worker accepted preseeded, per-run `https-cert.pem` and
`https-key.pem`: authenticated status, version and config reads returned 200;
missing authentication returned 403. A separate wrong-certificate loopback
listener failed certificate verification before receiving application bytes.
The proof removed its worker, secrets and owned fixtures. Result SHA-256:
`3c1c4c10ae381e8bff7eaa9c81f10e3337b98a27665e54e0cab136e49dd9d8f3`.
The production Rust client now has real matching-certificate, wrong-certificate,
CA-trusted/different-leaf, stalled-handshake and cancellation tests. Rejection
tests await their server and verify zero application bytes; the different-leaf
case also proves ordinary CA/hostname TLS succeeded before the pin rejected it.
The exact-key test checks the authenticated header. The production two-node
proof passed again using this transport, including bidirectional convergence,
pause/resume, cold restart, revocation and cleanup; result SHA-256
`a08b8c930f964f711cb232cea54efeba75bc48339a4eb93caed845d62d5be790`.
Actual execution inside the signed app sandbox now passes at production commit
`6a8be5a8b84b66367a1c03041990606ea91a05cb`. LaunchServices started an ad-hoc
signed app whose helpers inherited the production sandbox entitlements. Two
real pinned-engine sessions verified version, device identity and effective
numeric-loopback TLS configuration. A real folder reported no errors; fresh
mode-0600 HTTPS certificate/key material differed from the durable sync
identity. Both sessions reaped, the same durable identity reopened, and final
runtime/process counts were zero. The canonical container runtime parent was
108 bytes. Result SHA-256:
`14610477a838d3ed804c4ad96d4d38f9a88b2c6fc338eb36ebacab08a5a27abb`.
The signed archive passed deep strict code-sign verification before and after
extraction; archive SHA-256:
`aedd978fc9fea5fab324c23ac066dd26319e25aac51199cf79f1cba0228176af`.
This fixture used container-root data; cold user-selected security-scoped
bookmarks and an end-to-end native setup flow remain separate gates.

## Native and container integration checkpoint

Checkpoint `68bf16d` passed its hosted macOS app bundle and dependency-delta
jobs, but failed the aggregate software gate. Linux exposed the unsigned
`stat.st_mtime_nsec` type; Swift tests referenced file-private shared helpers;
iOS had a misplaced view modifier; Android lint consumed generated engine
assets without an ordering edge. The next snapshot fixes these exact findings.
Fresh hosted success is required before treating them as resolved platform
acceptance evidence.

The snapshot passes 825 workspace Rust tests across 22 top-level suites, then
adds an independently passing explicit TLS cancellation case. Strict Clippy,
formatting, foundation contracts, cargo-audit and cargo-deny pass. The new
Linux host discovery has three executable/manifest/runtime tests and the macOS
host has six, including long sandbox runtime paths.

The Dockerfile now builds the pinned engine with the existing pinned Go 1.26.7
image and packages the canonical guardian, immutable manifest and upstream
notices. Linux discovery uses owner-private `/tmp/cvs` and durable
`/data/folder-sync`; Compose and Unraid declare TCP 8789. The image fingerprint
now covers the engine build script and guardian source, with mutation tests.
The first combined images needed hosted build, runtime and budget evidence.
Compiled third-party notice aggregation remains a release gate.

Checkpoint `6a8be5a8` passed hosted macOS app bundling, iOS Tier 2, dependency
delta and both CodeQL analyses. Its aggregate software gate failed: Linux Clippy
flagged casts that are needed on macOS, Swift exposed one remaining private
test helper, Android API 26 could not reference the API-27 Java `O_CLOEXEC`
field, and both Docker packages stopped at the guardian-size check. The next
snapshot corrects these findings; it requires a fresh hosted run.

An isolated, resource-limited arm64 Docker check on Atmos measured the pinned
builder's static-PIE guardian at 242,888 bytes before stripping and 67,128 bytes
after removing debug/symbol tables. The package now strips at link time and
retains its existing 128 KiB ceiling, 64 KiB ELF load alignment, RELRO,
immediate binding and non-executable stack. The final binary had no loader or
shared-library dependency and executed its missing-argument refusal (exit
64). Its SHA-256 was
`e03a0e48a8b355d0b7fc5fde9c0e08e64f32055a2c909d1b77fbbee10dba353f`.
The owned container, newly pulled image and temporary folder were removed;
all 21 pre-existing containers and 24 pre-existing images were preserved.
This is guardian evidence, not a complete container acceptance result.

The next snapshot passes all 243 node library tests, including three new
provider-disable checks, and strict workspace Clippy. The combined macOS
shared/app Swift 6 source typecheck passes locally; hosted Swift Testing and
Android lint/device acceptance still require the next run.

Android's API-26 path passes the existing arm64/x86_64 Linux `O_CLOEXEC` value
atomically to `open`, with an additional `F_GETFD` assertion on API 30 and later
where Android exposes `Os.fcntlInt`.
The constant is documented by the
[Android 8 UAPI header](https://raw.githubusercontent.com/aosp-mirror/platform_bionic/android-8.0.0_r1/libc/kernel/uapi/asm-generic/fcntl.h).
No non-atomic open/flag-setting fallback is used.

The native runtime now has a separate local backup-provider admission flag.
When disabled, it closes storage connections before any request stream and
omits the chunk-storage discovery capability, while retaining pairing,
folder control and the owner's local backup/recovery API. Native hosts must
stop the previous runtime before changing this flag. An actual trusted QUIC
storage refusal followed by a successful signed pairing probe verifies this
boundary without deleting provider data.

The bounded notice collector and its six regression tests are integrated.
Its actual Android target reproduction collected 60 modules, 95 candidate
module texts and 109 total payload files (321,396 bytes); the manifest digest
is `194ab907dccf3da2bb94e70d942887b14571ebb4763523d851098b3012d579ea`.
The [target inventory review](../security/syncthing-target-license-inventory.md)
retains the Linux-specific graph, NDK runtime and shipped-notice gaps.

## Combined image measurements

At `d19e189f`, both native Docker builds now finish and pass the image contract.
The amd64 lane also passes its rootless/read-only runtime checks. Both images
exceed the previous backup-only 96 MiB ceiling:

| Architecture | Uncompressed image | Pinned sync worker | Stripped guardian |
| --- | ---: | ---: | ---: |
| amd64 | 122,164,736 bytes (116.51 MiB) | 31,959,550 bytes | 30,344 bytes |
| arm64 | 113,254,912 bytes (108.01 MiB) | 30,262,604 bytes | 67,128 bytes |

The combined product now uses a 128 MiB image ceiling. This is an explicit
scope adjustment for adding the maintained sync worker, not a performance
improvement or a skipped budget check. The check still measures each actual
uncompressed image. Node/CLI limits remain unchanged. Final notices, security
scans, two-architecture acceptance and performance optimization remain open.

## Checkpoint 27: real server console journey and package corrections

The Chrome server-console acceptance used two actual `NodeRuntime` instances,
mutually confirmed network pairing, and the pinned macOS arm64 worker and
guardian. Every folder mutation was driven through the production browser UI.
The test offered a named folder, accepted a separate destination directory,
verified exact file bytes in both directions, paused the source and withheld a
new file for seven seconds, resumed and transferred it, cancelled an explicit
removal confirmation, then stopped sharing and verified all existing files
remained unchanged. A later remote file stayed absent from the removed source
for seven seconds. Both nodes stopped, both listeners were released, both
private runtime directories emptied, and the test folders were removed.

The test also found that replacing the entire folder list on each health poll
could erase an incoming folder path and keyboard focus. Rows now retain their
controls until the underlying consent or pause state changes. The actual test
kept a typed destination path and focus across multiple polls. An inline,
labelled removal confirmation also retained focus across polls; cancellation
restored focus to the original Remove button. Neither browser reported console
warnings or errors. The two Chrome test tabs were closed.

Evidence is retained in the ignored artifact directory
`web-real-node-journey-20260908-2ea937b4`; `result.json` SHA-256 is
`b45b5b79085864b3d8b90d080296df2d55cf9fb6d48d17790cbb117909e37a2f`.
Its source hashes identify the exercised working tree. A subsequent narrow
placeholder-copy correction handles an unlocked node with zero paired devices;
it does not change the exercised two-node flow. An earlier in-app-browser
attempt passed transfer/pause/resume but could not complete native confirmation
through its browser automation APIs. That partial attempt is recorded separately
and does not substitute for this complete Chrome result.

Hosted checkpoint 26 passed Rust/contracts, dependency checks, the macOS app
bundle, macOS integration/UI, and iOS. Both Android lanes stopped at two Kotlin
compile errors: a missing Compose Button import and an SDK-unavailable
O_DIRECTORY constant. The fixes preserve the existing nonblocking, no-follow
open and immediate descriptor directory check.

Both Docker architectures passed their packaged runtime and 128 MiB image
budget checks: amd64 measured 122,259,968 bytes and arm64 113,321,472 bytes.
The new sync harness failed during private secret setup because a capability-
restricted root process changed the directory owner before its final file-mode
operation. An isolated, read-only Ubuntu container reproduced the denial and
verified the reordered initialization with the same CHOWN/FOWNER capabilities.
Its temporary container was removed; existing server resources were untouched.
Complete packaged Docker sync is still awaiting fresh hosted execution.

Linux target notice generation now runs in each exact build architecture and
installs readable combined texts plus bounded target evidence. The runtime
checks the combined texts, manifest and retained evidence hashes before launching
a worker. Missing/tampered text and symlinked evidence-directory regressions
pass. Exact target generation, final sizes and notice classification remain
open; details are in [Linux target notices](../security/syncthing-linux-target-notices.md).

## Checkpoint 28: current peer status and Android notices

The service now observes connections to exactly its configured paired workers.
Fresh connected, disconnected and paused states are redacted to Covalent peer
identities; missing or stale observations become unknown. A normal disconnect
does not stop the surviving worker. Android, macOS and the server console use
this state without claiming that an idle or connected peer has received every
file. Older status bodies decode to unknown in the updated native clients.

A real two-node pinned-worker proof completed 34 checks and observed
`connected → disconnected → connected` while the uninterrupted node remained
running. All owned processes were reaped. Its ignored artifact directory is
`peer-connection-live-proof-5f9d7b12`; `result.json` SHA-256 is
`384c4d51788b789386443500da3554c89c8205d156ee978ce704810366f23c5f`.
This evidence covers connectivity observation, not invitation renewal, remote
removal notification or address migration. Swift manual invitation decoding now
also retains the signed transport binding when forwarding an invitation.

Hosted checkpoint 27 passed Rust/contracts, dependency checks, macOS app
bundling, macOS integration/UI and iOS. Android compiled, then three unit tests
failed on outdated JNI descriptors and a user-facing technical term. The updated
checks pin the full native method signatures, including the debug-only listener
port override used for isolated device tests. Production retains its fixed port.
The full Android folder journey is still being integrated with offline removal
and does not yet count as device acceptance.

Both exact Docker architectures generated and verified Linux target notices,
passed their image contracts and remained within the 128 MiB budget. Their new
folder journey reached mutual pairing and acceptance, then timed out awaiting
the first file. The next harness retains bounded, allowlisted lifecycle and
connection states before cleanup; it does not retain tokens, folder paths,
labels or raw logs. Complete Docker synchronization remains a release blocker.

Android now generates notice inventories from each actual CGO-enabled target
with the pinned NDK compiler and build tags, retains source fingerprints and
ships complete combined texts. The native Settings viewer checks bounded asset
sizes, hashes and strict encoding before displaying them. The build uses private
Go caches and removes them after completion. These additions still require exact
hosted generation, Kotlin tests and device execution; they do not establish legal
approval of every dependency. See [Android notices](../../apps/android/OPEN_SOURCE_NOTICES.md).

Local integrated validation passes 262 node library tests, 11 JNI tests,
102 browser tests, strict workspace Clippy and foundation checks. Apple HIG
conformance, native access repair and complete platform journeys remain open.

## Checkpoint 29: container home and native command corrections

Checkpoint 28 Docker diagnostics report `workerLaunch` for both paired nodes on
both architectures, before any health observation. Code review found that the
image's `adduser -H` creates an account whose real home directory is absent,
while worker admission requires that directory. The image now creates
`/home/covalent` with the account's ownership and mode 0700. A separate rootless,
read-only image check verifies the runtime HOME and directory ownership/mode.
Fresh complete synchronization must still confirm the correction.

The hosted Swift suite ran 115 tests and found one new round-trip assertion
comparing raw UUID letter case. Foundation encodes the UUID in uppercase;
the signed protocol operates on the decoded UUID value. The assertion now
accounts for that representation while preserving every other transport field
exactly. The existing macOS app bundle gate passed.

Checkpoint 28 Android compilation and JVM tests passed, but six lint errors
blocked device execution. The correction scopes the storage-policy exception
to the existing personal debug permission, uses configuration-aware Compose
string resources and uses the AndroidX URI extension. No broad lint suppression
is introduced. Both actual Android target inventories generated 311,314 bytes
with combined SHA-256
`9975031cbf6a1eba399c084bef50a4164a21f6b086c91a2ae244d8282de3979e`.
The shipped notices viewer still needs device execution. Checkpoint 28 CodeQL
completed successfully for every configured language; this does not complete
the broader dependency review.

The View menu now includes Folders with Command-4, retaining the three existing
section shortcuts. Root's read-only CUA inspection also observed the real
NSOpenPanel used by the production-manager proof: native sidebar, column view,
search, selected test folder and Cancel/Choose controls with accessibility
roles and labels. This proves the system picker, not complete Apple HIG
conformance for the app or permission-repair screens.

## Checkpoint 30: Docker restart mapping and temporary-file cleanup

[Checkpoint 29 CI](https://github.com/thekozugroup/Covalent/actions/runs/34266408065)
passes Rust/contracts, dependency review, both macOS jobs, Android foundation
and iOS. All 74 existing API 37 tests pass by exact test name, including the packaged
notice reader. The new complete folder journey is not part of that run.
Both Docker architectures now pass mutual pairing, consent, initial file
transfer, seven-second pause withholding and resumed convergence. This confirms
the worker-home correction. The remaining failure occurs while the harness
waits for the restarted recipient API; the other node stays running and
reports a fresh connected peer.

The harness cached Docker's ephemeral published HTTPS port across stop/start.
An isolated Atmos test reproduced port reassignment from 32768 to 32769. It
reused an existing image solely for a bounded shell/sleep process, with no data
mounts or application health check. All 21 existing containers and 24 image IDs
remained unchanged; the exact labelled temporary container and network were
removed. Proof result SHA-256:
`4eda031d5896f1461dbfe8a8503f4fda86bb01a02ee535f64a7d168acbffeb47`.
Harness SHA-256:
`2575f8e7eac9571826d7610d2b25b2ede2c74117c8bf72e809e22f12d9877b77`.
The sync harness now re-reads the exact owned container's loopback mapping after
each start, retains the original TLS CA and server-name checks, and compares the
complete public transport identity across restart. A bounded local fixture
also rejects foreign ownership and non-loopback publication. A fresh complete
Docker run is still required; this isolated port proof is not a sync result.

Temporary cleanup removed 48 obsolete publication payload files only after
verifying their contents against reachable published Git trees. It also removed
22 inactive Rust/Go build-cache directories. The removed files total
11,277,687,504 logical bytes; this is not a claim about physical APFS space
reclaimed. All 104 retained test executables, source/results and report artifacts
were hash-verified unchanged. Current tests, worktrees, toolchains and validation
manifests remain. Detailed cleanup receipts are retained in the ignored
`artifacts/validation-2026-09-08/temporary-cleanup` directory of the primary
checkout. Future cleanup continues after exact resource ownership and process
reaping have been verified.

## Verified macOS manager fixes in checkpoint 30

A real signed, sandboxed LaunchServices run used the production app model and
local node manager to choose an external test folder through NSOpenPanel,
restart with the retained scope, complete signed folder consent, synchronize
both directions and restore the committed share after a cold relaunch. The
peer endpoint and certificate stayed unchanged through both scope inheritance
and cold restart. A second file synchronized, removal preserved both copies,
and every production-owned helper, guardian and worker was reaped.

This established three production corrections: normal and recovery peer
listeners use UDP 8787, the app declares the persistent app-scope bookmark
entitlement, and the manager starts access on the exact resolved bookmark URL.
Path standardization is used only for deduplication; creating a derived URL
before starting access lost its security-scope association after cold launch.
Three focused Swift tests cover the corrected contracts.

Result SHA-256:
`bfb660405cd129a2485bb984151e83c133079f3edc42cbd64b1305b0bc44ff2f`.
Verification SHA-256:
`da54d006133923f53aeb5a61beb03c3d7e962c3fe5639939a08c95c98a924a24`.
Sealed signed app ZIP SHA-256:
`ec95363b71a0ce4ccffc5c0e68da3492dc8e557cda13c0f81d20cfb0cfec7eaa`.
The manager source differed from the production file only by the module import
needed to compile it in the isolated harness. The outer harness reaped the
exact second-node fixture after the cold sandbox could not signal a process
owned by its previous app instance. That fixture limitation did not affect
production manager-owned process cleanup. Cleanup removed another 683,820,403
logical bytes from nine owned temporary targets; frozen evidence remains.

The ad-hoc test app's default data-protection Keychain query failed with
`errSecMissingEntitlement` (-34018). The proof therefore used the existing
persistence seam with a private fixture file. It establishes folder/manager
behavior, not working secure default-store startup for personal builds. That
startup issue is an explicit open release gate. Full native permission repair,
HIG conformance and migration from older ephemeral peer addresses also remain.

## Remaining integration and release work

The macOS folder screen now consumes the authenticated API, uses confirmed
peer names, retains persisted native folder bookmarks across helper restarts,
and distinguishes consent, scan, offline and error states. Native execution
acceptance, permission restoration UI, Android folder setup and foreground
operation, recovery-copy UI, and final Docker packaging are not complete. The macOS package builder has local repeated-build,
arm64/dylib, inherited-sandbox, ad-hoc signature, manifest and tamper evidence;
the integrated app still needs hosted build and actual install/upgrade proof.

A clean health response is not evidence of an atomic full scan. The actual
upstream partial-permission experiment showed that readable siblings can be
published while another subtree is unreadable. Permission restoration needs
an explicit scan-before-exchange barrier and its own execution proof. Neither
zero remaining bytes nor idle state alone proves an offline peer has received
all changes. Local remove preserves files but does not yet notify the other
node to remove its invitation; expiry/renewal and network-address migration remain usability work.

Arbitrary same-UID filesystem replacement is outside the portable boundary.
macOS guardian `SIGKILL` containment remains unproven; Android/Linux use a
parent-death guard. Personal-use macOS ad-hoc and Android debug signing remain
the accepted scope; Developer ID/notarization and production Android signing
are deferred. Atlas is offline and the user accepted Docker validation in its
place. No physical Atlas installation or completed release is claimed.


## Checkpoint 31: complete Docker execution and native repair integration

Acceptance is now **70%: 14 of 20 verified milestones**. Both Docker jobs in
run `34270349326` pass all seven complete packaged-runtime checks at commit
`04a2d0c80edc5b812daf76bc9011fae42c9df327` (tested merge
`0f9872fd4d260a11b17868549f2cd2b20079e829`). Jobs `102210076000` (arm64)
and `102210076621` (amd64) verify hardened nodes, signed pairing, explicit
folder consent and full scan, forward transfer, pause/resume, full retained
transport identity on cold restart and reverse transfer, removal with files
preserved, and complete cleanup. The compact acceptance record SHA-256 is
`86d963f4773561ba2b3fc2612f611c9601a9d6cb72fb40a3ae654b944455c097`.
Atlas remains offline; this is the user's accepted Docker/Unraid path.

Checkpoint 30 also passes Android foundation and the existing API 37 device
suite, Mac integration/UI, the Mac bundle, Rust/contracts and dependency review.
Its optional iOS UI job ran both tests: the workflow passed, while the Home
contrast audit failed. XCTest provided no element identity. The failure image
makes the light-accent Refresh glyph the narrowest candidate; a Refresh-only
label-adaptive tint is included for a fresh exact audit. This is a hypothesis
until that hosted audit passes. No finding is suppressed. The release-candidate
aggregate passing does not erase the failure or establish full completion.

### Default secure storage for personal macOS builds

The default production key store now identifies actual ad-hoc signing through
the Security framework and uses the login Keychain's default application ACL
for new personal keys. Provisioned builds prefer data protection for new keys;
existing keys from either implementation are retained. Conflicting records,
locked or denied access, and missing keys for existing protected state fail
closed. No plaintext persistence fallback or broadened Keychain ACL is used.
The platform distinction follows Apple's
[TN3137](https://developer.apple.com/documentation/technotes/tn3137-on-mac-keychains).

An owned, persistent, secret-free file lock covers complete load/create,
legacy migration and rotation operations across app processes. The lock is held
from reading both Keychains through deriving and persisting any change.
A blocked-rotation regression uses two independent file-lock handles: the
second caller receives Busy before generating a key, then retries against the
first caller's durable history. Descriptor-relative
no-follow traversal rejects redirected lock paths; nonblocking contention
returns an actionable error. Unused and error-path secret buffers are erased.
Regression tests exercise differently signed concurrent first launches,
independent file-lock handles, symlink rejection, denied downgrade access and
actual deallocator-observed buffer erasure.

A fresh ad-hoc signed App Sandbox test compiled the exact production source,
without injecting persistence. It provisioned and reused the hierarchy, rotated
while retaining old keys and the API token, and retained the exact complete
hierarchy after a second LaunchServices process. It then deleted only its own
fixture item and verified it absent. Current production source SHA-256 is
`d205160146d501371dfab56c534450a7fe57f5919157cbd688550d10dae83808`;
sealed signed app SHA-256 is
`74e9b045d65130453af550cbec50fece90790b89233bdf41a9c1232af0a5dab6`.
The integrated Swift suite passes **140 tests**; strict Swift formatting passes. Sources, bounded boolean
results and cleanup receipts are retained under the ignored
`mac-keychain-transaction-proof-e7a318cd` evidence directory. Its owned Keychain
item was removed by the signed fixture, and the app/results/lock were cleaned
after both fixture processes exited; the sealed app and bounded evidence remain.

The earlier raw Keychain probe also confirmed that a changed ad-hoc executable
cannot silently read the prior executable's item. Actual authorized upgrade
and complete default-manager startup/repair remain separate acceptance gates.

### Recover folder access without forgetting consent

The backend now retains an authenticated worker-free coordinator when folder
access is unavailable. It can list redacted consent, remove a share while
preserving files, and journal one exact replacement root without starting a
worker. Preparing a healthy-runtime repair validates the selection before
stopping its worker. The journal binds preparation to its instance and revision,
refuses implicit multi-peer root changes, and retains uncertain repair state.

A changed root requires a scoped index reset. Covalent starts a distinct
network-inert worker with all folders and peers paused, verifies its exact
configuration, requests the selected folder's reset through the authenticated
private API, and requires successful response plus owned exit code 3. It creates
and fsyncs the marker through the retained root descriptor only afterward.
After exact reaping and revalidation, it clears the durable intent and repeats
the normal full initial scan before allowing exchange. Lost responses, failed
reaping, wrong exits and root replacement retain a retryable intent.

The real pinned v2.1.3 two-runtime proof passes **35 checks**, including an empty
replacement root receiving both original peer files without propagating
removals, unchanged signed consent and folder identity, no further old-root
writes, active repair, worker-free idempotent removal and cleanup. Result
SHA-256 is
`b6afb93b88a389fabe19d063d304195ccc6ae19a074128b20787d2d0b7565482`.
The exact nine-file reset-delta manifest is
`5c3d69e4fea94c4e1e811dd9288b5ff62413bab2c744a5bc37f182b60a64dd0c`.

The initial live proof used one folder. Investigation of a stalled second
invitation found that status requests performed worker health I/O while holding
the same mutex used for delivery and consent. Status now reads the owned health
task's last observation. The health task waits a full cadence after each probe,
preventing slow-probe catch-up from continuously occupying that mutex.

A new real two-runtime proof, with 150 ms status polling and no diagnostic
pause, passes **43 checks**: both same-peer folder invitations, both transfers,
empty-root repair, continued unrelated-folder operation, offline removal and
cleanup. Result SHA-256 is
`871bfd89e95564008cb793caa15146079de5d8063ee97cd0d311c6f4b91b8b43`;
the exact two-file delta manifest is
`8a4f71d0b81d845e02182dfa1244f82d1e61a734962bd17d03ab7a1520e1a239`.
A 500-poll service regression also verifies second-offer delivery without extra
worker probes. The service suite passes **33 tests**. This bounded liveness
result does not complete invitation renewal or address-change journeys.
Paused shares can renew the identical retained root without changing pause,
consent, revision or index state. Moving a paused share to a different root
still requires a separate supported transition. The two-file follow-up manifest
is `d2f8f11d1b09bd664f50bc3adf5f6504e92aa4e55281e735e1a72a583b84a567`.
The final integrated node suite passes **277 tests**, including paused same-root
repair. Strict workspace Clippy and the complete foundation checks also pass.

### Native flows and cleanup

Android now offers folder sync directly from first launch and pairs the phone's
own node without changing backup-server credentials. The pairing flow displays
and confirms the signed comparison code. Its device test uses actual local UI
address entry and confirmation, with an isolated second node accepting through
its API, before testing bidirectional files, pause/resume, cold restart, revoked
access and file-preserving offline removal. The derived suite contains **76
named device tests**; Kotlin compilation, lint and this expanded device suite
still require fresh hosted execution. No local Android SDK execution is claimed.

The Mac repair UI uses a native grouped Form with independent, share-specific
Choose Again and Remove buttons, a system folder-only picker and a confirmation
sheet that states files are kept. Native accessibility inspection confirms
separate actions; Command-4 opens Folders. XcodeGen's source now retains the
bookmark entitlement when regenerating the project. Full actual repair,
keyboard/VoiceOver, resizing and appearance acceptance remain open.

Cleanup continues only after resource ownership and inactivity checks. In
addition to the checkpoint 30 cleanup receipts, the repair agent removed two
owned Rust caches totaling 8,630,750,708 logical bytes, the Keychain test removed
151,843,910 logical bytes of fixtures/cache, and isolated Mac source-build
work removed 245,597,469 logical bytes of superseded outputs. The follow-up
source-integration verification removed another 2,251,173,285 logical bytes
of its own completed build caches and temporary outputs. iOS diagnosis removed
149,297,416 logical bytes of downloaded/extracted intermediates after retaining
the failure image and bounded test evidence. Proof artifacts,
source manifests and current build inputs remain. Existing Atmos services were
not changed. Logical bytes are not a claim of physical APFS space recovered.

## Checkpoint 32: source-built Mac worker and corrected platform test fixtures

Checkpoint 31 (`04a7137cf8b162f0a063280c06e74999a6d04789`) ran as merge
`18800b6dd6a0477e0b857a8b2706e1dc82a664ce` in CI `34276509487`.
Both complete Docker jobs, the Mac app bundle, Mac integration/UI, dependency
review and iOS Tier 2 pass. The unchanged iOS Home accessibility audit now passes
with the Refresh-only tint correction.

Two test assumptions prevented an all-green result. Linux passed 270 node tests
but recycled the inode in a remove-and-recreate fixture; the test now retains
the original directory by renaming it and verifies that the replacement has a
different identity. The corrected focused Rust test passes locally. An isolated
Atmos fixture verified this Linux identity precondition 100 times and removed
its temporary directory; it did not change existing services. The production
path/device/inode checks are unchanged. Arbitrary same-UID inode replacement
remains outside the documented portable boundary.

Android compiled and ran 145 JVM tests, with 144 passing. The repair-route test
compared JSON member ordering rather than its fields: the actual request had
exactly the intended fields and values in another order. It now compares the
parsed exact key set and values and checks POST plus bearer authentication.
This failure stopped the API 37 job before any of the expanded 76 device tests
ran. Fresh hosted JVM, lint and device results remain required.

### Real production Mac folder repair

The sealed checkpoint-31 manager proof completed four actual LaunchServices
sandbox launches using the production default Keychain store, app model,
manager, guardian and existing pinned worker. It paired two nodes and synced,
restored a paused folder after an injected invalid bookmark, kept the share
paused until explicit resume, repaired a new replacement root through durable
reset/full scan, and restored that grant after another cold launch. Final
removal retained both copies. Peer port 8787 and the exact TLS certificate were
stable throughout. Result SHA-256 is
`1b94db139938aaf56dc1a9622f1c98ed71b059ddc4391320c0ee313b2d991c1d`;
sealed app SHA-256 is
`d5b50a8368914df32f3b1381082db6cab6a56e6914e1dc0e9dd4549e62ac81ae`.

Each launch reaped its manager-owned local processes. The final sandbox could
not signal the fixture peer inherited from the first launch; the outer harness
reaped that exact three-process fixture tree. Cleanup confirms zero remaining
owned processes, exact fixture Keychain deletion, and 375,980,032 logical bytes
removed from owned app/build/container/temp files. This is recorded as a fixture
limitation, not a successful in-sandbox cross-launch signal operation. Evidence
and the sealed app remain; no production Keychain item was read or removed.

### Mac source package and notices

The Mac worker now builds from exact Syncthing v2.1.3 commit
`946e2b83a1f6c6ae119427c09e0a5802940b82ff` with a checksum- and size-verified
private Go 1.26.7 archive. The personal builder obtains those exact inputs.
The result is arm64-only, targets macOS 15.0, retains its content UUID and has
28,141,682 bytes with SHA-256
`4df1dea892fac3c9d1c0e822827c9a4c57711dcddaaf56a61edaab41d5337bbb`.
Two real source-package runs reproduce its exact executable and notices.
An early-invalid-archive regression verifies private build state is cleaned.
Package copying rechecks the unsigned executable before signing; the runtime
verifier binds the source descriptor, target manifest and notice index.
Settings now exposes a bounded, verified Open Source Notices reader.

The frozen 16-file integration manifest is
`162017fb684b0828a6a32fa6f19d99b5e9d5069e5a53e5d52d37fe0061166b17`.
The integrated Mac-host suite passes 7 tests, strict workspace Clippy passes,
and foundation/package/notice-reader contracts pass. The new exact worker also
passes the real 43-check two-runtime proof, including two folders, bidirectional
transfer, access repair, scoped reset, unrelated-folder continuity and cleanup.
That proof is outside App Sandbox; result SHA-256 is
`42b041366029d791c24abbb8bc391468d678fd5a76a7f3a4b787d48961979a42`.
New-worker sandbox execution and hosted source-build byte equality remain
separate gates.

A target audit reconciles 465 packages, 58 compiled modules and 105 notice files,
with every module evidence hash verified. Fresh source and exact-binary
`govulncheck v1.7.0` scans report four module-level x/crypto advisories; their
openpgp/ssh packages are absent from the exact target graph, and neither scan
has package or symbol traces. This supports package-absence disposition for
this target, not a blanket no-vulnerability or complete reachability claim.
Four compiled MPL dependencies still need explicit recipient-facing exact
source access; that packaging work remains open. Audit manifest SHA-256 is
`68a44946d3c32bd93050b43c0266ae6fdfca867818bf97df36529eaf67fb73a0`.
Its owned caches/source cleanup removed 417,380,299 logical bytes after retaining
compact evidence. The acceptance ledger remains **70%: 14 of 20 milestones**.


## Checkpoint 33: explicit renewal, native grant handoff and corresponding source

Checkpoint 32 (`1ba4e9f2856c841a5154af9aa312fa5b333fc907`) ran as tested merge
`3cad2e730a4bda175f63123be11fe5a773f1d64b` in CI `34278383979`. Rust/contracts,
both complete Docker jobs, Mac bundle/integration/UI, Android foundation, iOS
Tier 2, dependency review, CodeQL and release versions pass. The Mac hosted
source build reproduced worker SHA-256
`4df1dea892fac3c9d1c0e822827c9a4c57711dcddaaf56a61edaab41d5337bbb`,
including the expected 295,173-byte checkpoint-32 notice text.

Android foundation and the device preflight record 145 passing JVM tests. The
API 37 suite completed 59 of its 76 named tests. Test 60, the complete native
folder journey, changed MANAGE_EXTERNAL_STORAGE from its own instrumentation
process; ActivityManager explicitly killed the app UID for that permission
change. No native journey completion is claimed. The host must change permission
between completed instrumentation phases and verify actual cold-start recovery.
The retained failure artifact is `10077571093`, SHA-256
`83e348da68937a8297f1c8bac3a979485880d82b9d9524e561e69d3d89300508`.

The integrated replacement keeps all 76 source-derived test names: 75 run in
the baseline process, and the same folder journey test must pass independently
in setup, denied and restored processes. The host starts the ordinary Activity
after setup, observes exactly one packaged helper pair, changes the app-op and
requires that exact app PID and both helpers to exit. Denied cold startup must
remain worker-free while native cancellation/removal runs; restoring access
must not resurrect the removed share or change either retained file tree.
Permission and cleanup mutations require the exact owned emulator, installed
debug package and exclusive gate lock. Failed fixture cleanup retains its
private receipt, and app data is cleared only after successful restored-phase
cleanup. Shell syntax, exact result-contract tests, release guardrails and
foundation checks pass locally; Kotlin and device execution remain pending.

### Exact new-worker sandbox and HIG evidence

The production default Keychain, LocalNodeManager, app model, packaged helper
and new source-built worker complete two signed App Sandbox launches. Signed
pairing, explicit folder consent, first transfer, cold bookmark/identity
restoration, second transfer and removal with both copies preserved all pass.
The exact peer endpoint and certificate remain stable. Result SHA-256 is
`2a132d263d388f7cd5c1af35bb23667f463a6ac36c18d8df89014eb70d0673d1`;
cleanup SHA-256 is
`1818a4836e68581b0a5d4daa44b0bc8d75e1387db4a2d55b3c3ad133b5d04b6a`.
The final cold transfer preceded the first background health probe, so the
result deliberately does not claim fresh post-restart health. The sandbox
reaped its own local worker tree; the outer fixture reaped the exact peer tree.
Cleanup removed 376,160,256 logical bytes and the unique fixture Keychain item.

Checkpoint-32 HIG execution covers Command-4, native sidebar/Form, light and
process-local dark appearance, the 900-point width floor, separate exact-share
AX actions with labels/help, native folder-only NSOpenPanel, explicit Remove
and Keep Files confirmation, and Escape cancellation. The checklist SHA-256 is
`ff263591d3a558c09b6377ac02f629568d2e418025f356e680e6337b19090721`.
This is not a complete spoken VoiceOver or full Tab traversal claim. The user's
global appearance, keyboard-navigation and VoiceOver settings were unchanged.

Checkpoint-33 renewal views also compile as the full Swift 6 product and run
in a signed sandbox fixture at exactly 900 by 640 content points. Both expired
rows and the composer remain visible. The outgoing renewal action names its
share and explains fresh recipient consent; the expired incoming folder picker
is disabled with truthful guidance. Native file-preserving removal and Escape
cancellation produce no sync mutation. This review found one generic incoming
Remove accessibility label, now corrected to name the invitation for both
Remove and Decline. Result SHA-256 before that label correction is
`da1cd8bc455a5e1d5dbc64fa194786ed434dd470ab2107496ab6081f03188e73`.
The ineffective per-process keyboard-navigation override is recorded as a test
limit. Cleanup removed 338,874,932 logical bytes, reaped every owned process and
deleted the fixture token; frozen source and the signed app archive remain.

The personal upgrade fixture uses the exact production Keychain store and two
different ad-hoc executable/CDHashes. Build 1 provisions a unique login
Keychain hierarchy; Build 2 reaches the real SecurityAgent authorization
dialog. Automatic approval review rejects access to SecurityAgent. Authorized
upgrade completion and exact hierarchy retention after that authorization remain
unverified. The ACL retains one trusted application; the fixture changes no
ACL/global setting. Build 1 deletes its exact test item and verifies absence.
Result SHA-256 is
`49bd690cc8772492c2473e063d2b1965387dfb62f1ed38c35a534b4970f198d2`.

### Renewal preserves explicit consent and durable folder bindings

Authenticated `POST /api/v1/sync/renew` replaces an expired outgoing unaccepted
invitation with a freshly signed ID and later validity period. The source
retains bounded superseded ID/time metadata; retrying an old ID returns the
exact durable replacement. Old acceptance/commit messages cannot authorize
the replacement. A recipient's uncommitted old acceptance and root are retired
together, including a paused awaiting-commit state, so the fresh invitation is
Offered and needs a new folder choice and acceptance. Accepted source records
and removed shares cannot be renewed.

The status projection includes bounded `supersededOfferIds` relationships. Native
clients default missing metadata to an empty list and reject ambiguous
relationships before changing saved grants. Sender grants retain their folder
and rebind only through the authenticated relationship or exact renewal
acknowledgement. Recipient bookmarks/choices are retired without file deletion;
no new recipient consent is inferred from a matching label or an absent row.
Mac retirement persists the reduced bookmark set and restarts/reaps the helper
to release the old inherited sandbox scope before publishing fresh status. An
in-memory retry flag survives restart failure; cold startup uses the reduced
durable grant set. Sender ID-only rebinding does not restart an unchanged scope.

A same-host proof runs two real production FolderSyncServices and the exact
pinned Go 1.26.7 worker with two folders. Twelve checks cover renewal, unrelated
folder continuity, exact retry after source journal/service cold reopen, stale
acceptance rejection, no transfer before fresh consent, bidirectional transfer
using only the fresh destination, and an unchanged sentinel at the old root.
The host clock is unchanged. All owned workers, guardians and fixture roots
are removed. Result SHA-256 is
`5f15f4aab6ba55bc66635079ceed7db1f6fa27269f2b11ce8d6eb05c242113f9`.
This proves signed service delivery and real bytes on loopback; WAN delivery
and complete native renewal UI execution remain separate checks.

Local integrated checks pass 284 node library tests, 146 shared Swift tests,
107 web tests, strict workspace Clippy and formatting, and the exact
OpenAPI/runtime/client route contracts. A focused node regression also passes
after adding the paused-recipient renewal case. New Android renewal tests and
host-driven permission phases still need the next hosted build and device run.

### Deterministic corresponding-source archives

The common target inventory records exact Go module h1 sums, and builders run
`go mod verify`. The notice collector independently recognizes the copied
MPL-2.0 text and packages bounded, deterministic tar+gzip source archives. Each
archive has an exact source URL, module identity/version or Syncthing commit,
SHA-256, byte and entry counts. Archives remain separate from the bounded text
viewer and are streamed/rechecked by Mac packaging and final bundle validation.

The Mac target contains 58 modules and 465 packages. Five MPL archives contain
1,199 entries and 19,784,715 uncompressed source bytes, totaling 8,408,088
compressed bytes. Two independent builds reproduce the inventory, manifest,
combined text and all five archives byte for byte. The worker bytes are
unchanged. New combined notice SHA-256 is
`87e9c362fac963404229addf563d77dfccb23747a8e06a96c5b77d13b675c2fb`;
source-build descriptor SHA-256 is
`4e603f44b00ed564be92568c6615b7399f29c0a7ad8d1acb49ae094a9ede0cac`.
The isolated proof removed another 2,298,786,788 logical bytes of owned caches
and build state after retaining compact evidence. Actual Android/Linux target
packaging and final dependency classification remain open. Acceptance remains
**70%: 14 of 20 milestones**.

## Checkpoint 34: Android fixture nullability and exact checkpoint-33 results

Checkpoint 33 (`c97f214555a65b08258908b824de1e4c7cf07b03`) ran as tested merge
`a3320f36a2adfbe9cb4b45333a69e4c0078ba09a` in CI `34284223594`.
Rust/contracts, both complete Docker architectures, Mac bundle/integration/UI,
iOS Tier 2, dependency review and release versions pass. The hosted Mac builder
again produces worker SHA-256
`4df1dea892fac3c9d1c0e822827c9a4c57711dcddaaf56a61edaab41d5337bbb`
with the new 297,075-byte notice text and verified corresponding-source archives.
The complete images measure 133,809,664 bytes on amd64 and 124,833,792 bytes on
arm64, under the unchanged 134,217,728-byte budget. Only 408,064 bytes of amd64
headroom remain; future image changes must still pass the same measured gate.

Both Android lanes compile production Debug and Release Kotlin, then fail
instrumentation compilation at the same receipt constructor: checking nullable
`stage` membership in a set does not establish Kotlin's non-null type. The
fixture now explicitly rejects `stage == null` before construction. This keeps
its strict receipt validation and permits the compiler to establish `String`.
No test is skipped. The source-derived 76-name result contract still passes
locally; fresh JVM/lint/device evidence is required. The failed prebuild also
correctly leaves no freshness stamp, so the device gate refuses to run a stale
APK. Checkpoint-33 job IDs are `102255914745` and `102255914706`.

The final published Mac renewal view also passes a signed sandbox AX check
at exactly 900 by 640 content points. Incoming actions independently expose
`Remove Shared Receipts invitation` and `Decline Design Drafts invitation`,
with correctly disabled/enabled folder pickers. The native keep-files removal
sheet is canceled and no sync mutation is sent. Result SHA-256 is
`72f0c02963c8e3d918639b3dfba9d4c8e1b91356d714e780193ada212fcccf3d`.
All owned processes and fixture data were removed; the cleanup receipt records
344,776,704 allocated bytes, separately from prior logical-byte measurements.
The observed text-only Tab traversal follows the user's disabled system
Keyboard navigation preference. It is not evidence of a product defect; full
keyboard and spoken VoiceOver execution remain unverified, and no global
setting was changed.

Publication cleanup verified all 47 checkpoint-33 files and modes against the
reachable Git commit before removing 108 temporary payload files totaling
3,816,152 logical bytes. The source tree and compact publication/evidence
receipts remain. Acceptance stays **70%: 14 of 20 milestones**.


## Checkpoint 35: explicit removal state and native access retirement

Checkpoint 34 (`920dadc65c66f0d0645bb629d7c146fafc425097`) runs as tested merge
`5e327af735c0fac74ad45d7fdecb6adcd54d43d6` in CI `34285937246`.
Rust/contracts, Android foundation, both Docker architectures, Mac bundle and
integration/UI, iOS Tier 2, dependency review and release versions pass.
CodeQL run `34285937220` passes Java/Kotlin, Swift and policy. The Android API 37
device job `102261422991` fails before instrumentation: its read-only package
preflight cannot discover an accepted installed native-library directory. The
emulator boots and the prebuilt gate verifies the exact APKs, 150 JVM tests with
zero failures/errors, and lint without error-severity issues. Synthetic result-
validator fixtures in that log are not device-test passes. Native-library path
discovery and full device execution remain open.

The three removal interfaces use the authenticated optional boolean
`remoteRemovalPending` only on a `removed` row. They distinguish sync stopped
locally from peer acknowledgement, keep pending removals visible without
mutation controls, and explain that files stay on both devices. Missing legacy
metadata defaults to false; null, non-boolean and contradictory active-row
metadata fail closed. Removed rows can retain their explicit, bounded
`supersededOfferIds` relationship so a missed renewal response cannot preserve
an old folder-access record accidentally.

Mac reconciliation retires current and superseded removed bookmarks without
rebinding them, preserves unrelated and unbound access, and retires pending
repair bookmarks when consent is removed or an incoming invitation is replaced.
Its scope-retirement flag survives a failed worker restart, including removal
initiated on this Mac. A later refresh retries retirement before publishing fresh
status. Android reconciliation similarly retires both sides' old records and
matches a pending unbound offer by the complete peer/folder/label relationship.
It never binds such a pending offer to a removed row.

The frozen client slice passes 152 shared Swift tests, 108 web tests and full
production Mac application compilation. Regression journeys cover local and
remote removal, failed restart/retry, an interrupted pending folder repair,
persistent bookmark retirement, missed renewal, malformed metadata and unchanged
local file bytes. These model tests do not establish live sandbox helper reaping
or native removal UI execution. The four added Android JVM tests still require
the next hosted build. Proof SHA-256 is
`a9a9bc84107616e755266baf37bf40a683d86cc10abb36431536dfbe2bc54f59`;
the 17-file patch SHA-256 is
`dd0d320b05667cea0da563d8b12c19870e9b586b5f0bcdebc6540d75d7077840`.

CI now retains exact Docker engine distributions and image metadata from
unstarted, network-disabled temporary containers, removed by an exact-ID exit
trap. It also retains Android native link reports and the debug APK for final
cross-target review. Native build evidence remains separate from runtime tests.

Cleanup removes only completed owned outputs. The client worktree removed
323,229,273 logical bytes from its initial failed build cache, then 736,544,149
logical bytes after the successful tests and Mac compilation. Logs, source
hashes, patch and cleanup receipts remain. Publication cleanup verified all
three checkpoint-34 files and modes against Git before removing eight temporary
payload files totaling 231,000 logical bytes. No global caches, user folders or
unrelated server resources were removed. Acceptance remains **70%: 14 of 20**.


Checkpoint 35 also integrates the final ten-file backend removal slice. Fresh
signed folder-control requests bind requester, target, original offer direction,
folder ID and the bounded chronological invitation chain. An authenticated
withdrawal may overtake an offer or race a renewal; compatible chain prefixes
canonicalize to the same terminal refusal, while forks and cross-peer bindings
fail closed. A durable outbox retries until a verified acknowledgement commits.
Canonical removed summaries retain explicit current and superseded IDs under
the existing finite 4,096 retained-offer quota, so native access cleanup never
infers withdrawal from a missing row. Capacity exhaustion fails explicitly;
terminal summaries are not silently discarded.

The exact backend bytes pass 286 node-library tests, 26 focused sharing tests,
strict all-target/all-feature Clippy with Rust 1.97.1, formatting and the OpenAPI
check. Its real pinned-worker proof passes eight checks: two folders converge,
local withdrawal stops one, the recipient removes without echo, lost
acknowledgement and both cold reopens remain safe, both copies remain, and the
unrelated folder continues in both directions. The proof invokes the receiving
service directly; the signed control envelope is unit-tested separately. The
backend manifest SHA-256 is
`2e9cca4f56df9cdc13585697ba3bf5670b9d161575ee3e89f8386ee3650bb864`;
patch `4a11bed90fc97f8bdad7bf8e3c4cf7478a7edf8ec982716bab601d9678da53e1`;
live result `d33fae26938e974548a096f08bcb953bb7b5b2e9043acdffcf005e0859f4dd79`.
Both Docker runtime gates now additionally pause the owned recipient, remove
on the sender, restart the sender with a pending outbox, reconnect the recipient,
require actual signed delivery and acknowledgement, then cold-reopen the
recipient and verify both copies. These additional container checks are pending
the next hosted run. Cleanup unpauses only exact labelled fixture containers.

The Android preflight fix replaces unreliable dumpsys field discovery with the
exact PackageManager base-APK path. A bounded parser rejects foreign, duplicate,
split or malformed package paths and verifies both helper entries against their
packaged size and digest manifests. Before permission-fixture ownership is
established, the host confirms both installed helpers are executable and their
actual on-device SHA-256 hashes equal the exact prebuilt APK. Four local tests
pass, including tampering and malformed manifests; actual API 37 execution is
still required. Proof SHA-256:
`6246528d35239ac3a602677ceacbff771be7ca9bc7868c6eee251394cf4a5f3f`.

The Android NDK slice captures final Clang driver traces, LLD maps, dynamic
libraries and exact binary digests for Syncthing, the guardian and JNI on both
ABIs. It bundles complete pinned NDK NOTICE and NOTICE.toolchain files and binds
the notice hashes to the final link evidence. The Go wrapper forwards linker
probes unchanged and captures exactly one private final go.o invocation;
response files are bounded and validated. In-root NDK dot segments normalize,
while path escape fails. Ten provenance tests, 13 notice tests, package/JNI
fixtures and foundation checks pass. A real Go 1.26.7 external-link build through
the production wrapper and host Clang 21 passes and its executable runs. The
host shim omits the Android-only map flag, so actual Android LLD and both ABI
packages remain unverified. Evidence status is explicitly
`link-inputs-classified-review-required`, not a completed legal or security
review. The final 15-file manifest SHA-256 is
`50f2562d66ab54444aa6ebb06cd2524f793ebd6503b703dc8ca762036b205628`;
patch `f4e0f99033db0809e1dd03288e958fa2c3e9e50de2884a60fa0b396a1d7c8a38`.

The signed native Mac renewal stage passes all 22 checks against an owned pinned
TLS response fixture. Actual system folder pickers select distinct owned
roots; an actual retry click preserves the sender bookmark and grant identity
while rebinding the old invitation to its replacement. Cold launches retain the
new binding. The recipient makes a fresh choice; ambiguous replacement metadata
fails closed. All 22 production Swift files match published checkpoint 33 and
the retained frozen source bytes. The response fixture intentionally withholds
replacement status until the second POST to exercise the retry click. It does
not prove the production backend, signed peer, worker reaping or scope release.
Result SHA-256:
`8c9ce2981a33202b33e24968d9a2d926cfc8b22aaeb546f04aff6f59563f59ce`;
source manifest
`e0c41186840af0d8cb16b3267fe610487bd1bb2d1c3938998e97621598c6bcd6`.
Cleanup reaped three exact owned processes, closed both fixture ports and
removed 648,567,789 metered logical bytes. Compact proof, source and cleanup
receipts remain; two empty OS-managed container shells remain.


Independent Android review found that a saved bound grant and an interrupted
pending grant for the same tuple could otherwise reconcile to the same offer
key. The integrated correction reserves already-bound identifiers and checks
unique keys again before persistence. Its regression requires unchanged
persistent bytes for the existing-bound-plus-pending case. Removed cards also
suppress obsolete expiry copy. The three-file correction manifest SHA-256 is
`13e8393222bbd96e8fe34cd1340591a65c01b6e9122052c4916988b27edcb3a0`;
patch `edade56cfd1372b0c3d688bd871e62efd610350ab2c26bf38ff47e1c226c6c74`.
All five added Android JVM regressions require hosted execution. The integrated
OpenAPI check passes 49 operations, 344 references and 48 client operations
across 126 checked call sites. Integrated foundation and release guardrails
pass. No milestone credit is taken for queued hosted execution.


The exact integrated Mac removal sources additionally pass a signed native
16-check fixture and bounded source review. Actual pickers establish the old
grant and a distinct pending repair; a dropped mock repair retains both. A
pinned authenticated mock status with the replacement's explicit removed chain
retires both persistent arrays. The production AppModel retries a recorded
empty-grant restart after one injected failure. Native accessibility exposes the
exact folder/device, local stopped state and remote confirmation copy with no
per-share actions. Both owned files remain byte-identical. All 22 frozen Swift
files match the integrated source. This proves native UI and sequencing, not
actual helper reaping or spoken VoiceOver. Result SHA-256:
`ae41fe0d35af271b01fa542ff4fed2208307cb1789b2580f198d5e968ba7c514`;
source manifest
`f02c502d2222bb1d10dd122565b26c717fb642e7a963b3154d40014a73ad844a`.
Exact cleanup reaped two processes, closed the listener and removed 338,072,685
logical bytes across 3,804 items, retaining only compact evidence and one empty
OS-managed container shell. Independent review found no preflight defect;
actual Android installed-path/hash execution remains required.


## Checkpoint 36: repair build-evidence gates and scan both complete images

Base commit: `9a1c632edbf24252730ed0ef2fabe2a3aad2ba32`. Checkpoint 35 CI
run `34290941016` tested merge `2ce61306eff1038a883fd5ec197f05402b31554f`.
Rust/contracts, dependency review, Mac bundle, Mac integration/UI and iOS Tier 2
pass. Both Android jobs fail after successful JNI links for both ABIs, before
JVM/device tests, because the official NDK r27b `llvm-readelf` symlink is excluded
by `find -type f`. Both Docker jobs build the complete image and pass package
contracts, then fail while copying evidence into read-only directories. Their
new runtime removal checks do not execute. Earlier successful Docker acceptance
remains separately recorded. All CodeQL languages and the final alert-policy
gate pass, as do release-version checks.

The one-file Android fix uses the exact pinned host toolchain directory already
selected for both ABI Clang drivers. It accepts executable `llvm-readelf`
symlinks while rejecting missing/broken tools and mixed host toolchains. Local
shell, JNI build-command, native-package, JNI contract and whitespace checks
pass. The manifest SHA-256 is
`6f7cd2f55018c7af9b56bd73fe671a462e8b478c27dbb47ff013958f74706d9b`;
patch `253c124824d2b197640ad2759735bf1052183a4feab7d6807673a34d2fb22313`.
Actual NDK provenance and complete API 37 execution still require hosted gates.

Docker evidence now streams from the exact unstarted, network-disabled container
through GNU tar with delayed directory permission restoration into `engine/`.
The outer evidence directory stays writable for image metadata. An isolated
ordinary-user Atmos fixture passes exact bytes, restored 0555/0444 modes and
outer metadata creation with GNU tar 1.35. It executes no Docker container and
touches no service. Its exact temporary root is removed in a finally block.
The final eight-file CI/security proof SHA-256 is
`218271d6e25e1fe9d4070e169315c49344ac7f130744e0ca2ad2ebdda5bde79e`;
file manifest `da4407017c24e3ee021d45212fc74335017a8882adcf369fb4082f7edb9cd78d`;
patch `73fbecf3e55f24c7a7b3a433d917b18ccd244989123356a86787bc5a430ba4ae`.

Both CI architectures additionally scan their exact local image ID using
checksum-pinned Grype v0.117.0 release archives, matching the release lane. Review
found that the previously pinned action still executed an installer from mutable
upstream main; that action is now removed. Both official Linux archives and
member sizes were inspected, with compressed SHA-256 amd64
`38525dab1e06f162ebaa02f94d82d1f807076b011a44180cf2777edf1a7b9c26`
and arm64 `935f628bdf9331ffdd946931ea5fdb50045d3970ba52670cbeb44a88f127291b`.
Temporary downloads were deleted after inspection.

High and critical findings fail even without an available fix. The current
vulnerability database must pass update, age and hash checks. Strict bounded JSON
validation requires the exact image ID, scanner version and no suppressed
findings. Logs include bounded severity/DB summaries and up to 30 high/critical
findings; exact reports remain retained. The private release lane attempts and
uploads both architecture reports before a final gate; either failed outcome or
missing/invalid report blocks promotion. No registry publication, exemption or
ignore list is introduced by this checkpoint. Mock scanner/finalizer fixtures
and release guardrails pass under CI=true and GITHUB_ACTIONS=true, including
production rejection of the explicit test hook. Workflow/shell/Python parsing
and diff checks pass. Real current image scans and live database behavior still
require hosted execution.

Artifact metadata and normal job logs remain available. Materialized connector
artifact URLs returned HTTP 403 locally; an authenticated browser download
reached a storage page blocked by the client. No blocked storage-host retry or
alternate access was attempted. The owned browser tab was closed. Current ZIP
bytes are therefore not claimed as reviewed; retained local evidence and hosted
logs have their stated narrower scope.

Checkpoint 35 publication cleanup verified all 49 published file bytes and modes
before deleting 80 temporary payload files totaling 2,837,012 logical bytes.
Compact publication, extraction and validation receipts remain. Acceptance
stays **70%: 14 of 20**, with no credit for unexecuted fixes.


A separate real Mac manager proof of checkpoint 35 finds a repeated restart/busy
cycle when adding a second folder while an existing committed share is scanning.
The first share runs; retrying the second restarts the helper and recreates the
initial-scan barrier. A narrow native correction and real signed-app rerun are
active isolated work, not included in this build-evidence checkpoint. This is a
remaining daily-use limitation; passing hosted UI fixtures does not close it.


## Checkpoint 37: verified address refresh, truthful device status and build diagnostics

Base commit: `55732b847c252c6828bcc8da7ca8a66bf8571fce`. Checkpoint 36
run `34295214789` tested merge `9e1a9a4087d52a94ac4238ef4367f38a877363b6`.
Rust/contracts, dependency review, both Mac jobs, iOS Tier 2, all CodeQL
languages/policy and release-version checks pass. Both complete Docker architectures pass package contracts, evidence extraction,
artifact budgets and all nine actual hardened-runtime checks. Jobs
`102290243409` (arm64) and `102290243603` (amd64) execute signed pairing, consent,
full scan and two-way transfer, pause/resume, identity-preserving cold restart,
local removal, durable offline removal retry, signed remote acknowledgement,
recipient cold restart, preserved files and exact-resource cleanup.

The exact scanned images are arm64
`sha256:2c16f12e7cf20b17ae64152d03ba157fe60a27e9da66875433d27f6d4fd1296e`
and amd64
`sha256:7c8339336c50e95bb26f2c6d097e5e7743b1a0fa281acd1c0fbf774bdea22c1d`.
Both scans report vulnerabilities at or above the high threshold. The old
1 GiB private-directory bound then aborts before the JSON verifier prints the
bounded finding summary. No specific CVE or exemption is inferred from that
message. The two-file correction validates and summarizes the exact-image
report before aggregating report, disk, timeout and scanner failures. Every
failure remains fatal. The private cap is now 2 GiB, with finite headroom over
the current complete vulnerability database. A regression combines disk excess
and scanner exit 2, and verifies diagnostics plus cleanup. Scanner fixtures,
release guardrails and shell syntax pass. Manifest SHA-256:
`b9f1b7dc81be83ed1eebe30baa7de428dea36969b494f56bf9f576296402f517`;
patch `519d51f55c5295024a62b689385c25acc2e65417f980cc45c251e9d1bcdaba9c`.
Successful real image scans remain required.

Both Android checkpoint-36 jobs link JNI for both ABIs, then fail in the
final-link provenance collector before JVM or device tests. The collector
compares concrete map paths with concrete driver paths but omits a linker's
named-library requests. The two-file correction records exact `-l:filename`,
`-lname` and `--library=name` requests and accepts only a classified, resolved,
regular in-NDK input with the corresponding requested basename or concrete
path. Unknown inputs remain rejected, and missing rows are reported in a
bounded normalized form. Eleven focused tests, JNI contract and foundation
validation pass. Manifest SHA-256:
`5f4ec5cedcdd26900ebee76f0374c57e10bbc552c3e9ce458afba05c5c9e5614`;
patch `78fcf3b4db7ab19dbb08c342fd2cebdf074ca6cc66f37aa8c9ee552b3d5fbbba`.
The exact missing hosted map row is unavailable through current artifact access;
this correction still requires the actual pinned NDK rerun.

The reviewed 13-file address-refresh slice adds authenticated
`POST /api/v1/sync/peers/refresh-address` with an exact peer, expected address and
candidate address. A fresh signed challenge and pinned TLS leaf authenticate
that the candidate is the retained device. The old address is bound locally by
the retained transport and complete grant compare-and-swap, not a field in the
signed probe. The journal prepares and rechecks the full grant before and after
worker reap. Core advances durable route state; remembered providers rebuild
from current authorized trust; only then does the journal finalize and permit
another full initial scan. Historical signed offers and acceptances are never
rewritten. Pending provider/journal state cannot be bypassed through generic
start or retry; cold recovery handles the completed core transition. Pending
remote removals survive and use the new verified route.

Validation includes 303 node library tests, five core transport-update tests,
strict all-target/all-feature Clippy and OpenAPI route checks. The real
10-check proof uses two NodeRuntime instances and the pinned worker to verify
signed pairing, consent, route movement, two-way transfer, cold reopen,
revocation, file preservation and owned cleanup. The final provider-role
recheck is covered by a focused test and strict Clippy after the live proof.
Manifest SHA-256:
`04dc980f8018452c72825aaa3dbf559eae8d4c2e176d8a1a27eb8199672bd756`;
patch `526f16c4246f81a75490b52db2375706a1d997c962a405a08c94986d7b27a3ea`;
live result `b1e5749b45973cf2e317bded17a38261ac848790bbcd01d296b308eadf16f278`.
Native and web address-entry journeys are not implemented by this backend slice.

The six-file Mac status correction stops presenting every saved provider as
connected. Device and backup-detail rows use consistent native labels and
symbols for responding, failed last check and unknown state. Positive evidence
must include non-future timestamps and an unexpired signed window. A real
backend negative response has null timestamps, so it remains a last-check
failure without implying current reachability. Timelines age device labels and
backup-selection controls; final submission still checks current eligibility.
Revocation copy states that existing shared files remain on both devices.
All 154 shared Swift tests pass, with two endpoint integration tests skipped;
a standalone package compiles the exact full current Mac and shared source.
Native visual and spoken verification of the changed rows remain pending.
Manifest SHA-256:
`d8bfa39e45d3453e3cd86db349352781a4133745d18846da58c8f75d132d3328`;
patch `54dbdf7b2f32b08f3164e6162dbbdaf746db803e52de02aef4c3e3fba5499ed8`.

Cleanup removes only exact completed task-owned resources. Mac status work
removes another 1,046,845,005 logical bytes of completed build caches after
retaining source and test proof. A root audit separately confirms with all
active agents that 15 old Cargo targets are unused, verifies they contain no
tracked files and have Cargo cache markers, preserves historical diagnostic
logs, then removes 109,197,375,028 logical bytes across 297,167 items. These are
file sizes, not a claim about physical APFS space recovered. Current Mac proof
apps/caches, pinned worker/guardian, source worktrees and compact evidence remain.
Cleanup receipt SHA-256:
`1d0ddbc5b18bc84e78b6a434ee6c19673bbd31ecd36de25e46e504891aaadece`.
Acceptance remains **70%: 14 of 20**; second-folder Mac repair, full Android
execution, client address changes, vulnerability fixes, final accessibility,
upgrade and performance acceptance remain open.

## Checkpoint 38: real Mac removal, current-file validation and measured build fixes

Base commit: `87c322f6df85d929474be8ab0f83fe03afdf07d7`. Checkpoint 37 run
`34297173842` tested merge `0b6df2822940df1aac37e3f29af506fe7eea966d`.
Mac bundle, Mac integration/UI, iOS Tier 2, dependency review and release
versions pass. CodeQL run `34297173819` passes Swift, Java/Kotlin and policy.
Both Docker architectures pass complete packaging, extraction and all nine
actual hardened-runtime checks, including offline removal, signed delivery and
acknowledgement, both cold restarts, retained files and owned-resource cleanup.
The overall workflow fails; the following failures remain visible.

### Validate the files that will actually be published

Hosted foundation finds an undefined OpenAPI `BadRequest` response introduced
with address refresh. The initial checkpoint-37 local foundation result is
invalidated: its route-test fixture used `git checkout-index`, which copied the
old staged files and omitted the current unstaged edits. The corrected script
copies the current bytes and modes of every tracked file into its private
fixture, preserves symlink semantics, rejects missing or unsafe tracked paths,
and removes the fixture on exit. The response now uses the actual API error
schema and documents the 502 and 503 outcomes.

A valid unstaged correction passes while the original bad reference remains in
the index. A deliberately injected unstaged missing response then fails the
fixture; restoring the exact corrected file passes. Direct OpenAPI validation,
route fixtures and foundation validation pass on the corrected current tree.
Manifest SHA-256:
`2abc6f1e4b2da91a674f4a917b095bcfb6917a269f99cab7323db9f23e7d1959`;
patch `d89d07ed237e394fb16aeaf572618d131c6498f21aafe636ea7593403b999920`.

### Keep a live Mac helper through its initial scan

The two-file Mac correction retains a fingerprint of the launched folder
bookmarks and launch mode. It rechecks authenticated folder-service liveness
before reusing that launch; missing service, failed requests and backup-only
fallback invalidate it. Binding a bookmark to an invitation does not restart
the same capability set. Removing access still forces a restart. Offer and
accept retry only the typed retryable initial-scan response for at most
30 seconds, using the same request and helper. Other failures require the
normal explicit recovery flow.

All 158 shared Swift tests pass in the isolated checkpoint-35-based slice.
The sealed `g95c34f8` proof executes the production AppModel and manager with
that exact correction, two real NodeRuntime instances and signed sandbox
helpers. Two roots are selected through real NSOpenPanel dialogs. A removal
requested on the second node reaches the Mac over authenticated QUIC. The Mac
retires only the removed grant, replaces and reaps its old helper tree, and
continues transferring the unrelated folder. An unchanged-binary cold launch
restores only the unrelated grant and preserves the removal tombstone. A
direct inherited child probe is denied access to the removed root and retains
access to the unrelated root. The peer certificate and UDP 8787 remain stable.

An outer fixture observer records exact PID, PPID and executable paths because
the sandboxed app cannot enumerate its worker descendants. All owned processes
are eventually absent. The second node's internal stop does not complete; the
outer fixture stops that exact process. This is evidence for checkpoint 35 plus
the frozen busy correction, not a claim that the entire checkpoint-38 app has
executed the same journey.

Review manifest SHA-256:
`ef778b6ffd2880dd8feb523861e139843c84fc455fed0ed6837c2a8991ae3a32`;
patch `c1ee6c17a2f3c3f52a301dca140d3ae48e899deaae301e3fe71589b925505693`;
result `9c447672fb5738ac021b71905606c9051f0820a77ac77cc503ab7f4ce6252fe1`;
process snapshots `e5d8cdeee4921ad978e19955b1c56b262724e9268ba09fe35d3339aba8ff446e`.
The exact production AppModel hash is
`af782e6d797a426a9c8e48d683139cbece46b183e9c508d3e22c7ee762cde803`.

### Rebase the Android limit on the measured production objects

Both checkpoint-37 Android jobs pass final-link provenance and produce identical
release JNI sizes: arm64 9,481,568 bytes and x86_64 11,154,960 bytes. The latter
exceeds the old 11,141,120-byte limit by 13,840 bytes. Jobs `102296207486` and
`102296207553` stop before JVM, lint, APK creation or device execution. The later
emulator boot does not constitute instrumentation evidence: its APK is absent.

The one-file correction records those measurements and uses the established
worst-ABI-plus-12% policy rounded to 64 KiB: 12,517,376 bytes. The derived
universal package limit becomes 102,408,192 bytes. Size-focused release
profiles, fat LTO, stripping, export auditing, floors, per-ABI checks and the
separate helper limits remain enabled. Shell syntax and native package fixtures
pass. A complete hosted rebuild is still required.
Manifest SHA-256:
`36a3e7edbadbf25e0de14ef494b769988cc1c5dfcfd1f8bac4ed99d8bc111db5`;
patch `191e5920693324a9290d7f7a8de05fc115f0bc072444f414f10f5e621c56f286`.

### Bind vulnerability reports to the actual scanner format

The exact checkpoint-37 images are amd64
`sha256:51f3b372f0efacafc04b067c31bf0437820bc3088be0714d0b0139227a0b90da`
and arm64
`sha256:3061519cfd04bca661fba7876753e9d70a8e8e0c16f3079dc3325b149a2516b1`.
Both Grype scans report high/critical findings. Report verification then fails
because it expects a `docker:` prefix in `userInput`. The pinned
[Grype 0.117.0 provider](https://github.com/anchore/grype/blob/v0.117.0/grype/pkg/syft_provider.go)
removes that selector before passing the reference to Syft 1.51.0;
[Syft image metadata](https://github.com/anchore/syft/blob/v1.51.0/syft/source/stereoscopesource/image_source.go)
preserves the selector-free reference. The verifier now requires both
`userInput` and `imageID` to equal the exact requested immutable image ID.
The invocation still explicitly selects Docker. Different input, different
resolved image, wrong scanner and suppressed findings remain rejected.

Complete private scan data measures 2,273,193,984 bytes on amd64 and
2,264,055,808 on arm64. The finite bound is now 3 GiB. Exceeding it still fails
and still prints valid report diagnostics before aggregating fatal outcomes.
The regression combines a vulnerability and excess disk use, and separately
rejects incorrect input even when the resolved image ID matches. Scanner
fixtures and full release guardrails pass. Manifest SHA-256:
`c9aac71e2ac1b254f0420cec75197c5255c7dcde96b2dc68c0021ba7e2a17011`.
Actual CVE details, remediation and clean rescans remain open; no exemption or
successful actual-image scan is claimed.

### Expose saved addresses and retain evidence while cleaning up

The additive status field `peers[].address` projects only current confirmed,
unrevoked, identity-matching pinned peers, including peers without an active
folder share. Revoked and unpinned records are excluded. No private key or
certificate is returned. The two-file slice passes 304 node tests and strict
Clippy. Its manifest SHA-256 is
`1c23b548a16e2eaf4a77c923eb27b9e8a8ef36edc9f2818175150aa3ef8d5677`.
The OpenAPI field is merged with the response correction above. Seven client
files add precise address/folder error messages; web tests and 154 isolated
Swift tests pass. Android mappings and XML pass static checks; JVM execution is
not claimed for that isolated copy slice. Manifest SHA-256:
`afb574f838b34e1678f4c15a026994214ee235e8f64c34948a11d8160aacf8cb`.
Native and web address-entry interfaces remain separate ongoing work.

The Mac fixture removes its owned processes and filesystem data, retains compact
evidence and keeps one ACL-bound test Keychain entry with its matching signed
binary. The service is
`life.michaelwong.covalent.remote-removal-proof.g95c34f8.node-key`, account
`managed-node-kek-hierarchy`; no test API token or certificate entry remains.
No SecurityAgent interaction, ACL change or alternate identity is attempted.
Cleanup receipt SHA-256:
`8f3e6201b42e3ff91d1d9094f971f0453265a9d2ad16bc9528e4f508f682454f`.
Other completed scratch caches are removed after preserving manifests and logs.
The earlier 109,197,375,028-byte cleanup remains a logical-byte count, not an
APFS physical-space measurement. Atmos services remain untouched.

The combined checkpoint-38 source passes all 162 shared Swift tests in six
suites and current-worktree foundation validation. The combined Swift log
SHA-256 is `d37f7ebe2315cd6a6823acf29802cafaca59c73a1f800f2f8553857379ca9e63`.
This adds integration evidence without replacing the narrower source scope
of the real sandbox proof above. The 108 web tests pass with the error-copy
changes; new address-entry UI is not part of this checkpoint.

Acceptance remains **70%: 14 of 20**. The core is functional; complete daily-use
readiness still requires native/device address and folder journeys, vulnerability
fixes, final dependency review, upgrade/accessibility and performance acceptance.

## Checkpoint 39: address-entry journeys and concrete security findings

Base commit: `c4be5c504f458398eb13dbe052b7b5e6496aecf0`. Checkpoint 38 run
`34299573349` tests merge `fdf06aa6bcd54c8ff89fae36dd2bb10e89cbfef7`.
Rust/contracts, Android foundation, both Mac jobs, iOS Tier 2, dependency review
and release versions pass. Android foundation executes JVM tests, lint and all
debug/release/test package tasks successfully; the log does not report an
aggregate executed JVM count. Its JNI objects measure 9,481,632 bytes on arm64
and 11,159,376 on x86_64, below the reviewed 12,517,376-byte limit.
The API 37 device run passes 75 non-journey instrumentation tests. Its separate
setup phase executes one test and fails at `FolderSyncJourneyInstrumentedTest.kt:121`
when `PackagedSyncEngine.load` reports the packaged engine unavailable. Installed
native-directory, executable and exact-hash preflight passes before this failure.
Permission-denied and restored journey phases never execute. Both CodeQL languages
and repository policy pass. The aggregate software gate correctly fails.

Both Docker architectures pass all nine complete packaged sync checks,
including signed offline withdrawal, sender restart, durable acknowledgement,
recipient restart, preserved copies and exact-resource cleanup. The amd64
three-node backup/source-loss/corruption/repair/restore scenario also passes.
Exact-image Grype report verification now works and identifies four findings
on each image: one High and three Medium. The High is
[GHSA-vp52-pcj8-j9qc / CVE-2026-84304](https://github.com/advisories/GHSA-vp52-pcj8-j9qc)
in `google.golang.org/grpc v1.82.1`; the upstream advisory identifies 1.83.1 as
patched. The reviewed Caddy consumer dependency update is integrated below;
exact rebuilt-image rescans remain required. No scan exception is added.

The exact amd64 image is
`sha256:9e37ef11e0218daf46ebecb5bb6c866feed2a4007f2d9ee4667b82617a258a5f`,
134,071,808 bytes against the 134,217,728-byte limit. The arm64 image is
`sha256:49621c71ce6c23020b06da78f0840ba87bed594b87708c7f0eb87c7f94d54664`,
125,030,400 bytes. Grype 0.117.0 uses database v6.1.9 built
`2026-09-08T06:30:10Z`. The private-data and exact-image checks pass; the
vulnerability finding itself fails both jobs. Bounded Medium diagnostics are
now added alongside the existing High/Critical details so remaining findings
can be reviewed from job logs. Each group has its own 30-entry bound; Medium
rows cannot displace blocking rows. The original fatal threshold and all
report/identity checks remain. Scanner fixtures and release guardrails pass.
Manifest SHA-256:
`54001cc0b978adb4eea3685a9350526d989753701a60fbd5184363fdb62882cb`.

### Caddy selects the patched gRPC version exactly

The five-file Caddy slice updates `google.golang.org/grpc` to 1.83.1 and the
transitive versions required by its module graph. Docker inspects the built
binary metadata and requires the exact version; a synthetic 1.83.10 value is
rejected. Go 1.26.7 module verification, consumer compilation, vet, host
Caddyfile validation, container contracts, release guardrails and source
fingerprint mutation checks pass. `go test ./...` reports no consumer test
files; it is not the upstream gRPC suite.

Host and cross-built Linux binary metadata all select 1.83.1. With identical
stripping and trimpath flags, Linux amd64 grows from 55,099,554 to 55,156,898
bytes; arm64 remains 51,445,922 bytes. The amd64 image would retain 88,576 bytes
of budget headroom if Caddy were the only change. The exact rebuilt image,
including web changes, remains authoritative. No budget is raised.
Source manifest SHA-256:
`5f02ee1e58f91d0e0d394c576b01d1b19267e154f99824fde24ca09001c8f701`;
patch `9e30c9cd5c5837651687d8f309d2a54c647059b986de31f66693562011b9dac8`.

After retaining build metadata, graph diffs and review manifests, the owned Go
module/build caches, temporary XDG roots, baseline copies and six binaries are
removed: 67,088 files and 5,624,059,343 logical bytes. The private toolchain and
other worktrees remain. Cleanup SHA-256:
`e8f0f9ddc3b19616d64a558c3b5e2ae07cf2b712ed69d3ebc58bb1289596370b`.

### A real browser updates a real paired device

The four-file web slice adds saved device addresses to the Pair panel and a
labelled inline editor. Polling preserves the current input and focus. The
coordinator captures exact `peerId`, `expectedAddress` and `candidateAddress`
values, preserves them after ambiguous failure, and requires current status
to confirm the saved route. A typed changed-address conflict invalidates old
status and requires another explicit confirmation after refresh. Separate
access and request generations prevent logout/relogin or a delayed conflict
response from applying obsolete status. A failed provider refresh cannot turn
an already confirmed save into a claim that nothing changed. Input is disabled
while its request runs, and reopening an already-open editor preserves edits.

All 119 combined web tests pass. Deterministic asynchronous cases cover lost
responses, same-device access reset at both await boundaries, stale conflict
reload, unavailable fresh status and independent provider failure. A separate
six-check Chrome fixture verifies labels, focus, polling preservation, exact
request fields and truthful saved-address copy after provider refresh fails.
Final source manifest SHA-256:
`08685e874b82eb9ecbf04a4367e0f8ca9471d17480abb554e4a6f4487c4dd0ee`;
patch `0ac2f6f4deff89bcfa2937a96dc6cd1481aad27a28dde861fdfa605b046c2f1c`.
The integrated `app.js` keeps the checkpoint-38 error-copy header and has hash
`3449fdd3c8cb856f04bbdcb98c1731c288eb1288e2e2cd2ebad7fc2c99a17fe8`.

The final 17-check live proof uses the exact integrated web source and two
production NodeRuntime instances. After signed pairing and folder consent,
the second node stops and reopens with the same identity at new control and
worker ports. Chrome alone submits the address update; the harness contains no
address-refresh call. The real signed candidate probe, complete grant/pin
comparison, durable provider update and full scan succeed. Exact file bytes
converge both ways; a cold source reopen retains the new route. Revocation
preserves both selected copies. Browser focus and the candidate survive a
5.6-second real status poll, and success copy reports saved/checking without
claiming transfer completion from the HTTP response.

The first harness attempt encounters an expected transient 503 before browser
execution; the fixture is corrected to wait with a bounded retry and a fresh
run passes. During the successful browser run, the first immediate folder
status load also meets reconciliation. A second explicit unlock after three
seconds is a fixture action; this does not demonstrate token loss. Both first
and final fixtures are removed, both runtimes and their workers/guardians stop,
all owned ports are released and the browser tab closes. Scope is host
loopback; no Atlas, Atmos, Docker or internet-route proof is claimed.
Live manifest SHA-256:
`6847b76083040b0baa68754957a15f4c613aff056e219a269b206dadf2acc112`;
result `af9a72811d061fab964d6f9319f8063c6f56863e8526d8d6624bb4f139c58d9c`;
browser observation `e6876d83cb27070ba343a02b3bacae5312ec93a9d7355e27dda8e26d59eddfc7`.

### Android captures explicit retries and clears stale state

The 11-file Android slice exposes optional saved addresses, keeps older-server
omission compatible and validates present values. Its native Material dialog
shows the saved address, accepts bounded numeric input and offers explicit
verification, retry and cancellation. Invalid pasted control characters retain
the previous input and display an error instead of throwing from the UI
callback. IPv4-mapped IPv6 literals are accepted by the numeric precheck; the
backend remains the canonical authority.

A testable awaited coordinator keeps mutation, status reload and provider
refresh serialized. Ambiguous failure retains the exact old expected address
and candidate, including when status already exposes the candidate but the
durable transition still needs its exact retry. An explicit conflict clears
stale rows and requires a new selection after authoritative reload. A
successful save with failed follow-up says refresh is needed and clears stale
status. No address result asserts that the peer is connected, and no helper
restart is introduced by the client.

Eight JVM tests are added; XML, static contracts, exact-baseline patch checks
and combined foundation validation pass. Local Gradle/JVM/lint/device execution
is unavailable and is not claimed for these new files. This is separate from
the successful checkpoint-38 Android foundation run. Manifest SHA-256:
`ed20d1afa75003388b9e7e2c3eed26c16ea3372b21f3d0703a6e3c03a629db50`;
patch `387cf3ced5a89737f6029954e2e062773039b19a51f793f710aa3b15f55a18c3`.

### Android identifies the exact packaged-engine check on the next run

The checkpoint-38 setup failure is generic; timing alone does not identify its
cause. A two-file diagnostic slice keeps every extraction, manifest, ABI,
canonical path, regular-file, mode, executable, no-follow, descriptor/inode,
size, hash and private-runtime assertion in the same order. Only the debug
instrumentation bridge prints a fixed nonsecret stage from that same loader.
Manifest opening, bounded reading and parsing are distinct; helper checks
identify the precise open, identity, mode, read or digest stage. Production
still reports only absent, invalid or verified. No assertion is removed and
no fix is claimed before the next actual API 37 result.

Static stage contracts and diff checks pass. Kotlin compilation and the real
device journey remain required. Manifest SHA-256:
`cce9b6fc22312671cd998fee57d9487ef4c714db2b02d472c2e106321f02006a`;
patch `7464b8703c17495da627504b1a7d62b6db79c9dc821810e9db69b49fc133aaf2`.

### macOS uses a native address sheet with precise retry state

The seven-file Mac slice presents every saved peer in Devices, including a
paired peer without a storage-provider row. A native grouped Form exposes the
current address, a focused labelled field, adjacent trailing Cancel and Verify
and Save actions, and explicit failure/retry state. Saving is serialized with
folder mutations. Lost responses retain the exact request even if status
already exposes the candidate; only an acknowledged idempotent POST completes
the save. A changed-address conflict requires authoritative reload and another
confirmation. If reload fails, the captured stale address cannot be submitted.
Provider refresh failure retains accepted/reload-needed state and clears old
reachability. The editor never declares a peer connected from an address save.

All 171 Swift tests in six suites pass; the combined production Shared and Mac
sources compile with Swift 6. The exact final ad-hoc sandbox fixture opens a
provider-free peer, exposes distinct accessible controls, sends the exact three
request keys, dismisses after the acknowledged response and renders the new
address with Status unknown. Its HTTP server is synthetic: this is native UI
and client execution, not a NodeRuntime or signed-peer transfer proof. The CUA
Escape key attempt is unsupported, so no executed Escape or spoken VoiceOver
claim is made. Source manifest SHA-256:
`c34d0f81c8d51fa4f44505c173308b7faae69c86b07b8afcf6138c095f2d5299`;
patch `03428687915b1224b9692dbcc4d46200e17a01b3e4758f1ec6b190b5b2582398`;
result `f9e8d5374673f8bb82f2d4d13e896cae72394bf4ce7e4a405cbab3076648fa07`.

The owned app and listener stop. Completed Swift caches and combined-source
copies account for 1,416,589,615 removed logical bytes. A clean extraction of
the retained local signed fixture archive passes strict codesign verification;
LaunchServices added FinderInfo to the launched directory, so the directory's
later metadata is not used to claim an unchanged signature check. Archive
SHA-256 `2ecc34fbcbbb0c9d35e49d9bf585907d85eb98bb3fd85a92e4fe71e688f89b70`;
cleanup `44f6b90b80d64fe3785d6ec1356a37896ab5eb53a02bad077f98cb0d8095f2dc`.

### Completed nested caches are removed

A second cache audit confirms with owning agents that four old nested Cargo
targets and two old Swift build directories are unused. It verifies cache
markers, no tracked files, no command-line process references and no visible
open descriptors before removing 7,521,910,925 logical bytes across 23,505
items. Sibling source, logs, manifests and active build resources remain. This
is a logical-size count, not physical APFS space reclaimed. Receipt SHA-256:
`d2936835da873c60110f6c6dca4743911a2bfbac2b620faa83037e0e286b793c`.
The retained Mac test Keychain entry and matching signed binary are unchanged.
An Atmos tool-availability read finds no Java or Android SDK and changes no
files or services.

The final combined current-worktree foundation gate passes after all 33 files
are integrated. Its log SHA-256 is
`845ec2671418d5764d692464939ee9b2656e088a25e66bcaae24c655792387f6`.

Acceptance remains **70%: 14 of 20**. Native address acceptance, the complete
Android device journey, clean security scans, final dependencies,
upgrade/accessibility and performance remain open.


## Checkpoint 39 final evidence and next corrections

Checkpoint 39 is `6514c05f789e6914828dff658e92abd164122198`. CI run
`34303240555` tests merge `bc6e0b232ecdb8b5541f415183d196697dec7b6c`.
Rust/contracts, Android foundation, both Mac jobs, iOS and both Docker jobs pass.
CodeQL run `34303240584` passes Java/Kotlin, Swift and the zero-open-alert policy.
Android foundation logs record unit-test task success, not an aggregate test
count; no count is inferred.

The API 37 device baseline passes `OK (75 tests)`. The separate setup journey
fails 1/1 at the precise `runtime-directory` verification stage. It never reaches
permission-denied/restored phases. Android's normal `noBackupFilesDir` mode is
0771, while the existing verifier rejects its group-write bit. The next narrow
correction must require both owner and group to be the app UID and reject
world-write on the platform parent. Its private direct child must remain exact
0700. Installed helper paths, canonical containment, APK metadata, manifests,
executable identities and digests remain independently verified. A source fix
is not credited as a successful device journey before the rerun.

Both exact Docker scans report zero High and three Medium findings, all
CVE-2025-60876 in busybox, busybox-binsh and ssl_client 1.37.0-r30. The amd64
image is `sha256:9edc22278fe11b21a589ba4441b64bec47d7bd2aa989515c23eb6b9b0c0d4451`
at 134,145,536 bytes. The arm64 image is
`sha256:6878480c81e5574e04fa0e45ec6ab5ede4da1874810daaace778ce8b7d2635e2`
at 125,030,400 bytes. Both bind Docker source fingerprint
`d45b319419397f0f3962361e596d58e26b48b5ebeb23aa278fd58f256824979d`.
Grype 0.117.0 used database v6.1.9 built 2026-09-08T06:30:10Z.

Dependency review separately fails the newer
[GHSA-2v4p-qf9q-27wj / CVE-2026-84445](https://github.com/advisories/GHSA-2v4p-qf9q-27wj)
in gRPC 1.83.1. Upstream
[v1.83.2](https://github.com/grpc/grpc-go/releases/tag/v1.83.2) fixes it. Caddy's
compiled graph contains Smallstep's gRPC client but no `grpc/xds` server package
or `xds.NewGRPCServer`; the described crash path is absent. The dependency is
still being updated, without a waiver or a clean-security claim.

### Real native Mac address journey

The signed native fixture uses the exact 23 production Swift files from
checkpoint 39 against two real production NodeRuntime instances and the pinned
worker/guardian. Only the native UI submits the address update; the Rust harness
contains no address-refresh POST. Eleven checks pass: initial signed pairing and
consent, new recipient ports with retained identity, signed candidate probing,
durable routing replacement and full scan, transfer both ways, cold route
retention and continued transfer, file-preserving revocation, and runtime cleanup.
The Devices sheet dismisses after save and reports the new address responding.
This harness does not establish LocalNodeManager sandbox containment, upgrade,
Escape-key behavior or spoken VoiceOver.

Proof `mac-peer-address-live-two-runtime-01/proof.json` SHA-256 is
`e788bcab68a5fa0cd0784926509f771098f259516547da64c74088eddbf659e8`;
runtime result is
`eeaf359d5b2664df13b250beab8c0524e77c6766e56691daada38f8623f4b63f`;
source manifest is
`2c5542c857181cff24e89a00a218f46c86a1b15c320c60099aea4450c45b81b1`.
Independent root review is
`1fddcd007ea5cd65fb8a331da21d840384e9ffc5c5954669447d158aaf9aacab`.

The owned fixture app, disposable token and completed build caches are removed:
779,122,749 logical bytes. Cleanup SHA-256 is
`b839f7f7e390ba7ca06430f41d21a9c9de817a71e7f23a16ee05b355ccbe3d2e`.
All three fixture ports are free, both runtimes stop and reap their workers, and
no Keychain items or global preferences were created or changed. Logical sizes
do not measure physical APFS space recovery. The separately retained earlier
ACL-bound test entry and its matching signed app remain unchanged.


## Checkpoint 40 narrow corrections

The Android loader and debug journey bridge now share the platform-parent
permission policy: directory type, owner UID equal to the app UID, group GID
equal to the app UID, and no world-write bit. This accepts Android's normal
0771 parent while rejecting a writable foreign group. The private runtime child
still requires directory type, app ownership and permission bits exactly 0700.
Canonical paths, direct-child placement and all packaged-helper verification
conditions remain unchanged. The new JVM regression covers 0771 and 0700,
foreign group, world-write, regular files, child 0710 and foreign child ownership.
Local JVM execution is unavailable; hosted tests and the complete API 37 journey
must confirm the correction.

The separate gRPC patch selects exact 1.83.2 and its required minimal-version
module graph. Go 1.26.7 module verification, consumer compilation (`go test`
reports no test files), vet and the local container contract pass. The fresh
linux/amd64 Caddy binary is 55,165,090 bytes, SHA-256
`23435840dc97b7e9231900a99e9923b12a02bcee698daa374d838ed3618821b2`,
8,192 bytes above the checkpoint 39 binary. Its build metadata verifies exact
gRPC 1.83.2. The image budget remains 128 MiB. New container distribution
collectors and the pending BusyBox backport are separate work, not part of this
narrow rerun or a claim that security acceptance has passed.


## Checkpoint 41: Android pairing endpoint and native distribution evidence

Checkpoint 40 (`53ad71a64fca55b27703faf8bc20816d1f6808a1`) completes its
hosted checks in run `34306597129`. Rust/contracts, Mac bundle/integration/UI,
Android foundation, iOS, both Docker architectures and dependency review pass.
CodeQL run `34306597117` passes Java/Kotlin, Swift and the zero-open-alert policy.
Both exact Docker image scans report zero High findings and three Medium
BusyBox CVE-2025-60876 findings. The amd64 image is 134,153,728 bytes and arm64
is 125,030,400 bytes; the 134,217,728-byte cap is unchanged.

Android device job `102324478641` passes all 75 baseline instrumentation tests,
then fails the one native folder setup test with
`incoming network pairing did not complete before its deadline.` Permission
loss and restored-access phases do not execute. The parent-directory correction
passes the earlier runtime-directory failure, but this does not establish a
complete Android folder journey.

The setup test entered a loopback peer endpoint while the native runtime signed
its detected emulator guest address. Production requires the candidate to match
the signed endpoint and verifies the request source address. Checkpoint 41
corrects the test's UI input to the live emulator address using the native
selection order. Production pairing validation, permissions and timeouts remain
unchanged. The test also moves the second node's UDP peer and TCP worker ports,
verifies its retained identity, edits the address through Compose controls,
checks transfer in both directions, cold routing and file preservation through
the existing permission-loss/restoration phases. These changes require a fresh
hosted device run; static source inspection cannot pass the journey milestone.

The Android distribution collector requires six distinct native objects: JNI,
worker and guardian for arm64-v8a and x86_64. It matches their final-link records
to bytes in both debug and release APKs and verifies the exact NDK source
properties, notices and packaged notice manifest. Full-build mode requires a
fresh generated summary; prebuilt mode recomputes and compares it. Bounded CI
JSON records make the complete six-object identity evidence available in logs.
Seven adversarial collector tests pass; actual hosted NDK/APK aggregation is
pending. No local Android compilation or device execution is claimed.

Acceptance remains 14 of 20 milestones (70%). The independently reviewed Caddy,
Alpine and BusyBox work remains outside checkpoint 41. The isolated BusyBox
arm64 build now passes exact package version/ownership and all 13 network tests;
its final amd64/arm64 image integration, vulnerability classification and size
acceptance remain open. Mac spoken accessibility and personal-build upgrade,
final installation regression and performance acceptance also remain open.

### Checkpoint 41 terminal hosted results and isolated patched-runtime proof

Published commit `14850c7a74e6cf9e49b82cbff5a25186e11fb0a1` was tested as PR
merge `63ad397b66295b25a048176c7381082b7abc2915` in run `34693770786`.
Rust/contracts, both Mac jobs, iOS, dependency review and both Docker jobs pass.
CodeQL run `34693770832` passes both languages and its alert policy; release
version run `34693770796` passes. Android foundation job `103553591406` and
device job `103553591417` fail; aggregate job `103555640186` fails.

Both Android variants compile and assemble, then the distribution collector
reports `packaged native object differs from link evidence`. The build writes
no prebuilt stamp. The device job subsequently reports
`--verify-prebuilt requires a prior full ./scripts/check-android.sh`.
Zero instrumentation tests run on this checkpoint. The 75 passing baseline
tests from checkpoint 40 do not establish a passing checkpoint 41 device run.

Docker jobs `103553591461` (amd64) and `103553591466` (arm64) pass all nine
hardened runtime checks. The amd64 Compose journey also passes backup,
source-loss/corruption repair and restore. Their common source fingerprint is
`d235dce10288de1075e9541f5a54aa6dd362b2a29f2372dc9d5f7b27ff6dccd9`.
The images measure 134,153,728 and 125,030,400 bytes, respectively. Both Grype
scans report zero High and three Medium CVE-2025-60876 findings. These are
the published, unpatched BusyBox images; the separate backport does not change
their classification.

The unpublished main Dockerfile's `runtime-patched` target was built natively
on Atmos from local integration commit
`8055cdd55079d7c76e381283d7e6e7b07ec8af18` plus the frozen BusyBox wiring
(Dockerfile SHA-256
`a770f54b6a4d01fa967e49e5db87eb61799361d6b07237385020265416bc4be4`).
All 45 patches verify, all 11 APKs build, all 13 wget request-target checks pass,
and the three installed runtime payloads match their tested bytes. The raw
installed-package database confirms that all three local packages omit the
package commit field and `busybox-binsh` uses architecture `noarch`. This is
an intermediate arm64 runtime proof; final image checks, amd64 execution,
vulnerability classification and final image size remain open.

The retained result SHA-256 is
`21b59256a00a9c754105702f5062dfd8fcee2d58cee1bdbea7e70a55f27490a3`.
Exact owned test resources were removed. Atmos's before/after ID sets match:
20 running containers, 20 total containers and 25 images. Separately, 15
obsolete owned temporary entries totaling 245,253,648 logical bytes were
removed after retaining useful evidence. Physical disk space reclaimed was
not measured. Acceptance remains 14 of 20 milestones (70%).

### Checkpoint 42 Android final-link package preservation

`build-android-jni.sh` links `libcovalent_android_jni.so` with `--strip-all` and
records its final linked bytes before Gradle packaging. Checkpoint 41 preserves
the worker and guardian through AGP's strip step but omits JNI. Checkpoint 42
adds JNI to the same `keepDebugSymbols` configuration so AGP packages the
already stripped output. Package-to-link equality checks remain strict.
Mismatch errors now identify the variant, component and ABI and include bounded
expected/actual byte counts and SHA-256 digests.

Root source review and `./scripts/validate-foundation.sh` pass on the integrated
change, including seven distribution-summary tests, eleven link-evidence tests,
native-package regression fixtures and the JNI contract. The diagnosis comes
from the checked-in build sequence and the permitted checkpoint 41 job log;
fresh hosted APK verification must confirm it. No local Android compilation,
new instrumentation pass or milestone increase is claimed.
