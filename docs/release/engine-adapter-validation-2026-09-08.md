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
engine uses an owner-only Unix socket. The client rejects symlinked endpoint
ancestors, checks directory/socket identity before and after connect, verifies
the peer UID and sends the key only in a sensitive request header. Exchanges
have a five-second whole-request deadline; responses are bounded at 2 MiB,
with a separate 8 MiB allowance for complete configuration. Redirects and
compressed responses are rejected.

Configuration admits only explicit device/folder/member sets and direct
numeric addresses. It disables discovery, relays, NAT traversal, browser,
telemetry, crash reporting and upgrades. Startup verifies the actual pinned
version, certificate-derived device ID and effective security, membership,
retention and filesystem settings. Missing or contradictory controlled fields
fail closed; unrelated upstream defaults may coexist. Overlapping roots,
filesystem root and roots containing private state are rejected.

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
Actual execution inside the signed app sandbox is still a separate gate.

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
Both actual architecture builds, image vulnerability scans, rootless runtime
and the unchanged 96 MiB image budget still need hosted evidence. Compiled
third-party notice aggregation remains a release gate.

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
node to remove its invitation; expiry/renewal and current peer-connectivity
presentation remain usability work.

Arbitrary same-UID filesystem replacement is outside the portable boundary.
macOS guardian `SIGKILL` containment remains unproven; Android/Linux use a
parent-death guard. Personal-use macOS ad-hoc and Android debug signing remain
the accepted scope; Developer ID/notarization and production Android signing
are deferred. Atlas is offline and the user accepted Docker validation in its
place. No physical Atlas installation or completed release is claimed.
