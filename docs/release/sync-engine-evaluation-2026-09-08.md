# Maintained sync engine evaluation

Status: evaluation only; no production engine change or completed Covalent sync
workflow is claimed. Date: 2026-09-08.

The requested product needs automatic two-way sync with simple native setup.
Covalent's custom signed-history implementation has substantial tested safety
foundations, but does not yet provide a complete network sync runtime. This
evaluation tests whether a maintained engine can reduce the remaining
implementation and maintenance work. The Android execution proof and the
security-model comparison must inform a product decision before integration.

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

## Decision still required

This proves upstream-engine behavior under these test conditions. It does not
prove Covalent UI integration, Android execution, managed-document editing,
install/upgrade, final artifact performance or release readiness.

Syncthing's device identity, index, conflict selection, versioning and device
removal semantics differ from [ADR 0006](../adr/0006-two-way-folder-sync.md).
Its [versioning](https://docs.syncthing.net/users/versioning.html) preserves
remote changes according to configured retention; it does not automatically
archive a device's own local edits. Its [synchronization model](https://docs.syncthing.net/users/syncing.html)
also has reserved filenames and filesystem-specific limits. Existing Covalent
pairing, protected recovery kits and signed apply receipts do not automatically
cover another engine's state. Two engines must never manage the same folder.

Before selecting this approach, validate an immutable Android executable
package, process lifecycle and private control API; compare failure and
revocation behavior against the actual product requirements; and define a
single beginner-facing pairing, folder and recovery workflow. Existing backup
and restore remain independently validated during this evaluation.
