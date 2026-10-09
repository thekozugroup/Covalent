# Maintained sync engine evaluation

Status: feasibility evaluation completed sufficiently to select the maintained
engine for implementation in [ADR 0007](../adr/0007-maintained-folder-sync-engine.md).
No completed Covalent sync workflow or release is claimed. Date: 2026-09-08.

The requested product needs automatic two-way sync with simple native setup.
Covalent's custom signed-history implementation has substantial tested safety
foundations, but does not yet provide a complete network sync runtime. This
evaluation tests whether a maintained engine can reduce the remaining
implementation and maintenance work. The Android execution proof and the
security-model comparison informed the implementation decision below.

## Pinned inputs

The test used the official [Syncthing v2.1.3 release](https://github.com/syncthing/syncthing/releases/tag/v2.1.3).
Downloaded archive lengths and SHA-256 digests matched that release's asset
metadata before extraction or execution. Extraction checked relative paths,
entry types and aggregate size, and selected the executable's exact archive
path. No installation, updater, launch agent or system service was created.

| Input | Archive bytes | SHA-256 |
| --- | ---: | --- |
| macOS arm64 ZIP | 11,269,720 | `e0f0d8df05bf0118c48c6515214a96bf3a3f11dbd115f56c3c0b52251b3f71aa` |
| Linux arm64 tar.gz | 10,893,415 | `a5c046965b590a8de2f8c8c16a0dbf9201d99600b0cafd604040232b603e4586` |

The Linux executable was a 25,209,976-byte static ELF with SHA-256
`a42a1c983f4eb613cd2c295439a62946a57282ec1df27189768f1a66bcf28fa8`.
These are upstream evaluation binaries, not Covalent release artifacts or a
replacement for the project's signed provenance and dependency gates.

## Local two-process result

Two isolated macOS arm64 instances passed:

- Authenticated private API access; unauthenticated configuration requests
  were rejected on both instances.
- File creation, an exact 1 MiB random payload, nested Unicode paths and an
  empty directory.
- An edit in the reverse direction, propagation of deletion, and recreation
  after deletion.
- Retention of the prior version after a remote overwrite or deletion.
- Cold restart retaining pairing and discovering new offline files on both
  sides.
- Deliberately disconnected edits to the same file, followed by convergence
  with both different contents present on both devices.
- Final idle folders with zero reported errors or required items.

Private configurations were generated offline and sanitized before startup.
All sockets were loopback-only. Global/local discovery, relays, NAT traversal,
telemetry, crash reporting, browser launch and upgrades were disabled. Only
explicit disposable folders were shared.

## Mac to Atmos Docker result

The real cross-machine test used macOS arm64 and one temporary Linux arm64
container on Atmos. The peer connection used authenticated engine TCP over the
Tailnet. Management was available only through SSH loopback forwarding.

The test passed a 64 MiB random-file hash comparison, edits from Atmos to the
Mac, deletion from the Mac to Atmos, retained overwritten/deleted contents,
concurrent edit preservation on both peers, container and Mac process restart,
and reconciliation of offline additions in both directions. Both folders
finished idle, with zero reported errors or required items.

The 64 MiB transfer took 18.179 seconds from scan request through confirmation:
**3.52 MiB/s**. This includes scan and polling overhead. It is an observed test
rate, not a final performance-gate pass or a general network throughput promise.
The remote container had a 0.75 CPU limit, a 512 MiB memory/swap ceiling, 64-PID
limit, read-only image, dropped capabilities and `no-new-privileges`. One final
idle sample was 15.07 MiB memory, 0.31% CPU and 13 processes/threads; this is not
a measured peak-memory bound.

Cleanup removed the exact owned container, image, remote directory, local
fixtures, processes and SSH tunnel. All **21 pre-existing containers and 35
pre-existing images** remained. No prune, service changes or Atlas access was
performed. Raw temporary identities and test files were removed; sanitized
JSON results and the evaluation scripts remain in ignored local artifacts.

## Adversarial local behavior

A separate pair of isolated, directly owned macOS workers exercised permission
errors, a missing folder marker, both edit/delete timestamp orders,
file/directory and ancestor/child conflicts, and version recovery. The first
run is retained as **26 passed, 2 failed**; the corrected/coordinated run
recorded **29 passed, 0 failed**. These are distinct runs, not a retroactive
replacement of the initial failures.

The permission check initially mistook inaccessible content for absent content.
Authenticated file-index inspection and a later exact-byte read confirmed that
the protected file was retained. Syncthing did propagate the parent's `0000`
mode, temporarily preventing peer reads. The same scan reported its permission
error while still committing successful sibling edits and deletions. Covalent
must show the folder error and must not promise an all-or-nothing scan.
Removing `.stfolder` caused the scan endpoint to return HTTP 500 and prevented
three missing local files from becoming peer deletions in this test.

The other initial failure exposed an actual version-recovery race: the restore
API returned success, but an active peer restored the newer replacement before
the recovered bytes were announced. A second test paused both peer exchanges,
restored and verified the selected bytes, scanned locally, and then reconnected;
both peers converged on the selected version. A production controller needs to
serialize recovery against peer activity and verify the resulting files before
reporting success. An API success response alone is insufficient; this finite
proof does not establish a universal restore transaction.

A subsequent controlled test paused only the restoring node's peer connection
and verified disconnection and idle state before restore. It recorded **24
passed, 6 failed** checks. In both ordinary replacement and concurrent remote
edit cases, the restore endpoint returned an empty error map, but the selected
old bytes never appeared during a 15-second isolated observation and were
absent from both current folders and both complete bounded version-store
searches after reconnection. The selected archived copy was lost inside these
fixtures. The exact internal scan/index cause is not yet isolated. Covalent
must not expose this stock in-place restore operation. A separate experiment
is testing copying a retained version into an exclusive new filename while
preserving the archive and the current file. The earlier successful both-sided
pause remains evidence only for that particular run.

The new-copy experiment subsequently passed **28 checks, zero failures** in
14.254 seconds. With active peers, the selected archive was opened through
directory-relative no-follow descriptors, copied into an exclusive new ordinary
filename, hashed, and synced to disk. The exact recovered bytes converged on
both peers while the current original and archive remained intact. A concurrent
remote edit of the original also survived independently. A pre-existing copy
name returned `EEXIST` and preserved every existing file. Both workers and the
private fixture were removed. The versions API supplies timestamp and size,
not a digest; the adapter must derive the pinned archive path safely and compute
integrity from the held descriptor. Alternate versioners, same-second archive
collisions and filesystem aliasing remain outside this experiment.

The tested ordinary-file contents survived both edit/delete timestamp orders,
file/directory conflicts and ancestor/child conflicts, either as live contents
or conflict copies. The test explicitly configured unlimited conflict copies
and simple versioning with 100 retained versions and no age-based cleanout.
This does not establish immutable history or unlimited version retention.

Both runs stopped their exact workers, restored permissions and removed their
private fixtures. They made no production, Docker or server changes. Device
removal, reserved/case/Unicode names, active symlink races, pruning beyond the
configured limit, arbitrary I/O faults and Android adversarial behavior remain
outside this proof.

## Additional feasibility checks and decision

Two additional isolated macOS checks passed after the initial evaluation:

- The management API works through an owner-only Unix socket beneath a private
  directory. Missing and incorrect API keys were rejected. Graceful shutdown
  released the socket; the test directory was removed.
- A 10,000-file transfer recovered after `SIGKILL` of the receiving worker
  while only part of the folder was applied. Both peers converged, and every
  destination file matched its individual expected SHA-256. The 10,240,000
  bytes across 100 directories took 57.343 seconds from scan through restart
  and convergence. This is a scaling/recovery observation, not a final
  performance-gate pass. Post-convergence RSS samples were 93,680 KiB and
  64,944 KiB; peak memory was not measured. Both processes and the fixture were
  removed.

The first attempted crash test timed out on receiver readiness after targeting
the launcher. Tagged `cmd/syncthing/main.go` and `monitor.go` show that
`--no-restart` still runs a monitor and separate worker. The corrected test set
the pinned `STMONITORED=1` control to own the worker directly. The first failure
remains recorded and is not evidence of file corruption; the direct-worker
rerun is the successful crash-recovery evidence. A Covalent supervisor must
own, stop and reap the actual worker, including under forceful termination.

A separate macOS sandbox capability test also passed with a generated folder
under Documents. An ad-hoc-signed AppKit application used the current Covalent
sandbox entitlements, launched an inheriting helper, and that helper forked and
executed a second inheriting writer. Access before selection was denied; the
actual system folder picker was observed. After selection, the writer read the
sentinel and synchronously wrote the expected bytes. A cold app restart resolved
the persisted security-scoped bookmark and repeated the write, while the
unselected sibling remained denied. The initial result JSON was overwritten by
the cold restart; the preserved result, exact fixture hashes and control flow
record that limitation. This is capability evidence, not execution of Syncthing
inside a final signed Covalent app. All test bundles, fixtures and container
data were removed; macOS retained only its protected container metadata file.
No proof process remains.

The separate Android branch cross-built both executable ABIs with the pinned
source/compiler and passed ELF checks: arm64 was 30,787,168 bytes and x86_64 was
32,646,984 bytes. The first build exposed AGP 9's rejection of a Provider in the
assets source-set API. The next exposed the Android `anet` dependency's documented
private Go API/linker requirement. The following build reached packaging but
stopped on missing coroutine metadata verification; the standalone test now
uses the repository's already-verified coroutine runtime. Actual API-37 execution
was still pending at that point; those builds alone did not establish Android
feasibility.

The fifth standalone build packaged and installed both APKs after two missing
AndroidX JAR checksums were verified against exact publisher bytes. API-37
instrumentation then aborted with a guest system crash before producing a test
result. The sixth run reuses the production suite's existing guest preparation,
records framework PID stability, and captures bounded crash diagnostics on
failure. That run timed out before instrumentation because the shared readiness
helper still targeted the production app's Compose activity. The seventh run
supplies the helper's existing package/component overrides for the experimental
APK and its non-direct-boot platform Activity, preserving every readiness check.
The first crash's cause remains unconfirmed; neither packaging nor a corrected
harness counts as an executable runtime pass.

The seventh run subsequently passed the exact API-37 instrumentation test,
`extractedHelperRunsPrivateOfflineApiAndRetainsIdentityAfterColdRestart`, with
`OK (1 test)`, successful instrumentation completion and unchanged framework
PIDs. [Run 34224544802](https://github.com/thekozugroup/Covalent/actions/runs/34224544802)
tested commit `57160682685d62aa5ac53c0efe3f48fb1698fc1e` and retained both ABI
build hashes. This proves the bounded x86_64 emulator execution/lifecycle/API
case. It does not prove arm64 device runtime, API-26 runtime, storage access,
foreground-service behavior or final Covalent packaging.

This proves upstream-engine behavior under these test conditions. It does not
prove Covalent UI integration, full Android lifecycle/storage, managed-document editing,
install/upgrade, final artifact performance or release readiness.

Syncthing's device identity, index, conflict selection, versioning and device
removal semantics differ from [ADR 0006](../adr/0006-two-way-folder-sync.md).
Its [versioning](https://docs.syncthing.net/users/versioning.html) preserves
remote changes according to configured retention; it does not automatically
archive a device's own local edits. Its [synchronization model](https://docs.syncthing.net/users/syncing.html)
also has reserved filenames and filesystem-specific limits. Existing Covalent
pairing, protected recovery kits and signed apply receipts do not automatically
cover another engine's state. Two engines must never manage the same folder.

The immutable Android executable, private control API and bounded lifecycle
proof support selecting this approach. ADR 0007 defines the beginner-facing
pairing, folder and recovery workflow and the remaining authorization/lifecycle
gates. Existing backup and restore remain independently validated while the
Covalent controller is implemented.

The initial Rust managed-session implementation now passes actual macOS cold
restart (14 checks) and two-way file transfer/reverse creation/deletion
convergence (nine checks), with generated protected identities, complete
effective-config verification and confirmed guardian/resource cleanup. See
[the adapter validation record](engine-adapter-validation-2026-09-08.md).
NodeRuntime, invitation and native UI integration remain unfinished.
