# ADR 0007: Maintained engine for automatic folder sync

Status: accepted for implementation, 2026-09-08. The complete Covalent workflow
is not yet implemented or released.

## Context and evidence

The required product is intuitive automatic two-way folder synchronization on
macOS, Android and Docker, including the user's Unraid deployment. Existing
encrypted backup and owner-loss recovery remain useful independent features.
The custom protocol in ADR 0006 has tested local safety foundations but lacks
the complete network runtime, filesystem reconciliation and native workflow.

The [maintained-engine evaluation](../release/sync-engine-evaluation-2026-09-08.md)
verified ordinary two-way edits/deletes, offline reconciliation, conflicts,
version retention, cold restart, interrupted transfer and 10,000-file content
integrity on macOS. An isolated Mac-to-Atmos Docker test passed across real
machines. An API-37 Android test then proved execution from the immutable
installed native-library directory, authenticated private API access, graceful
shutdown and identity retention after cold restart. The exact named test passed
with unchanged framework PIDs in [run 34224544802](https://github.com/thekozugroup/Covalent/actions/runs/34224544802).
This is feasibility evidence; Android storage permissions, background lifecycle,
final packages and Covalent's own sharing workflow still require validation.

## Decision

Use Syncthing v2.1.3 at commit
`946e2b83a1f6c6ae119427c09e0a5802940b82ff` as a pinned, privately supervised
transfer engine behind Covalent. Build with the reviewed toolchain and dependency
manifest. Disable its own GUI exposure, browser, updater, telemetry, crash
reporting, global/local discovery, relays and NAT traversal. The first version
connects explicitly approved devices over direct LAN or Tailnet addresses.

Covalent owns the user experience and authorization. Its existing authenticated
pairing channel binds the public engine device ID and direct address to the
Covalent peer. Choosing a folder and recipient creates one invitation; the
recipient chooses a local destination and accepts. Pairing alone shares no
folder. Paths and management keys stay local, and no user enters an engine ID.
Legacy peers without a retained transport pin need the existing verification
ceremony before they can receive a folder invitation.

Store desired membership and folders durably before applying changes. Regenerate
the controlled engine configuration before cold startup, reconcile every
revocation before network access, and stop sync if that reconciliation fails.
Unsharing stops future participation without deleting either user's local
files. Already received data cannot be recalled. Do not run two engines over
equal or overlapping roots.

Management uses an owner-only Unix socket and a random API key in request
headers. The controller verifies the actual engine identity, version and
effective configuration. The worker runs directly (`STMONITORED=1`) under an
owned guardian with a parent lifeline, descriptor allowlist and exact-child
termination/reaping. Linux/Android also require a parent-death guard. Arbitrary
guardian `SIGKILL` on macOS remains a distinct lifecycle limit to address and
test, not a portable guarantee.

The engine identity is distinct from Covalent's recoverable owner identity.
Use the existing platform key protector for its durable encrypted envelope and
private runtime materialization for stock-engine key files. Owner recovery
creates a new engine identity and explicitly reconnects folders. Final crash
cleanup and encrypted-storage boundaries must be tested; a copied complete
installation cannot be promised clone detection.

Keep the macOS sandbox and system folder picker/bookmarks. An isolated signed
capability proof passed through two inheriting helpers, including a cold
bookmark restart; the final Covalent bundle still needs the same test. Android
existing-folder sync uses an explicit Settings grant on supported versions and
a connected-device foreground service for user-approved ongoing communication
with paired computers. Permission loss pauses the affected work and requires a
complete local rescan before exchange resumes. Existing SAF backup/restore
remains independently available.

## File recovery and changed semantics

The maintained engine's index, conflict selection and filesystem behavior govern
automatic sync. ADR 0006's custom signed-operation epochs, freeze receipts,
all-or-nothing scans and immutable history are not claims for this backend.
Its experimental modules remain isolated and are not another active sync engine.

Show partial scan errors and permission problems. Preserve ordinary conflict
copies, use explicit version-retention limits, and surface disk pressure.
Versioning retains remote replacements/deletes; it does not archive every local
edit. Sync is not a substitute for independent encrypted backups.

Recover a selected old version **as a new copy**. Do not expose the stock
in-place restore endpoint: controlled testing observed success responses while
the selected archive was lost. A separate 28-check test preserved the current
file and archive while the recovered copy converged, including a concurrent
edit and destination collision. The adapter must derive the configured archive
path safely, hold a no-follow file descriptor, hash/copy to an exclusive new
file and sync it before reporting local recovery. Upstream version metadata
contains no digest. Recovery makes no global atomic-transaction promise.

## Remaining release gates

Complete the Covalent pairing/invitation/controller/native UI path; validate
startup, shutdown, parent death, revocation, permission loss and file recovery
through it. Verify final macOS, Android and both Docker architectures, install
and upgrade, then measure performance on those artifacts. Atlas is offline;
the user accepted Docker validation for that deployment.

Preserve MPL-2.0 notices and distribute the required covered source. Complete
the target dependency/license inventory, vulnerability reachability review,
SBOM validation and signed artifact provenance. The initial source scan emitted
four module-level x/crypto findings. The affected SSH/OpenPGP packages are
absent from the verified Darwin target imports; those explicit dispositions
retain the original findings. Android target-scoped assessment remains pending. Neither this decision nor passing feasibility tests
marks the project 100% complete.
