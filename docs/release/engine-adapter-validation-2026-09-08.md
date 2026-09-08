# Maintained-engine adapter validation

Date: 2026-09-08. This records a working implementation boundary, not a released
folder-sharing workflow. [ADR 0007](../adr/0007-maintained-folder-sync-engine.md)
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
licenses and sources. Hosted exact-revision checks remain required.
Raw logs and reproduction probes remain in ignored validation artifacts.

## Remaining integration and release work

The adapter is not yet enabled by `NodeRuntime` or exposed through the local
API/native apps. Callers still supply already-authorized desired state. Durable
folder invitations, current peer-grant/revocation reconciliation, periodic
capability checks, native folder permissions/bookmarks, recovery-as-copy and
production engine packaging remain required. The installation-root creator
must durably create its parent entry before treating creation as complete.

Arbitrary same-UID filesystem replacement is outside this portable boundary.
macOS guardian `SIGKILL` containment remains unproven; Android/Linux use a
parent-death guard. The expanded Android guardian test runs separately at
commit `d20403b815dbff5bcc16c53bf241d9a0dc4f209c` in
[run 34229120120](https://github.com/thekozugroup/Covalent/actions/runs/34229120120)
built both engine ABIs with their repeated exact hashes, then failed before
instrumentation because the guardian dependency check rejected `libdl.so`.
The pinned NDK linkage must be assessed and the actual gate rerun. This does
not establish execution of this Rust controller on Android, foreground-service survival,
existing-folder access, final install/upgrade or release readiness.
