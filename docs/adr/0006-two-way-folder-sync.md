# ADR 0006: Two-way folder synchronization

Status: in development — core foundations implemented; network folder sync not shipped.

## Context

Covalent currently provides immutable backup snapshots and explicit restore. It
does not reconcile edits from multiple devices. A Syncthing-like folder feature
therefore needs a separate, versioned protocol and local apply subsystem; it
must not reinterpret a backup, restore job, provider replica, or management
bearer token as synchronization state.

Syncthing's [Block Exchange Protocol](https://docs.syncthing.net/specs/bep-v1.html)
and [synchronization overview](https://docs.syncthing.net/users/syncing.html)
are reference behavior only. Covalent will not claim BEP compatibility.

## Decision

### Folder authority and keys

Each participating device has a distinct cryptographic writer identity. A
folder has a random immutable ID, explicit members, and member roles:

- **Read** may receive folder state and content over the authenticated session.
- **ReadWrite** may also publish operations.

The separately pinned membership authority may change membership and shared
policy. It is not an ambient role granted by a read/write invitation.

Membership is a folder-scoped signed grant, separate from pairing and
device-wide transport roles. Version 1 has one explicit membership authority
per folder, signing a linear membership-epoch chain. Read/write invitations do
not delegate this authority; competing admin histories cannot silently choose
a winner. Genesis contains only the authority, which remains a ReadWrite
participant under its pinned writer key in version 1. Authority transfer and recovery require a separate authenticated
protocol before they are exposed. Loss of that authority freezes membership
changes; changed membership then needs an explicitly created new folder and
new invitations. Data already received by a removed member cannot be recalled.

Removing write permission requires a signed transition proposal and a durable
freeze receipt from every member that will remain. Each receipt commits its
causally verified frontier and the losing writers' exact counter/digest tips.
The authority obtains and verifies the complete causal closure of the joined
frontiers before signing the next epoch and cutoff. The frozen writer inputs
remain paused across restart until the resulting epoch or a signed abort. A
member that is unreachable cannot remain in that transition: wait, or explicitly
remove it too. The first version does not perform unilateral forced write
revocation that could split already-accepted histories.

Delayed old-epoch writes from continuing writers remain eligible. A writer
whose permission ended is limited to its authenticated chain at or below the
agreed tip and full-clock cutoff. Previously accepted shared operations remain
valid because all surviving members supplied their history before that cutoff.
Over-cutoff input is rejected; a survivor presenting it has broken the freeze
promise and requires review. Removed devices retain all local bytes. Their
unshared edits require explicit review, a new writer identity, and bootstrap
before republishing. Removing a member with read-only access needs no write
freeze, but immediately ends its future folder access.

Version 1 synchronizes directly between explicitly authorized folder
participants over mutually authenticated QUIC. These devices are authorized to
read plaintext in the selected folder; read/write grants govern each exchange.
Existing opaque backup providers remain a separate backup/restore capability.
They do not receive plaintext sync paths or content, become folder participants
automatically, or determine conflict winners. Version 1 does not require an
untrusted encrypted sync relay or group-key distribution service.

A future opaque sync-storage capability would require independent random keys
per encryption epoch, authenticated recipient key bindings, standard recipient
key wrapping, and a defined re-encryption/bootstrap policy. Deriving new epoch
keys from old keys would not revoke a removed member. Those future requirements
are not a reason to invent cryptography for direct peer sync.

### Replicated state

Operations carry a folder ID, per-install writer ID, strictly increasing persisted
counter, membership epoch, bounded causal version vector, signed body, and
content references. Transfer is encrypted and requires an explicit folder grant. The receiver verifies all of them before
retaining or applying an operation. A duplicate is idempotent; the same author
counter with a different digest is equivocation and is quarantined.

Version 1 keeps a causal register for each canonical relative path, with entry
kind, content and metadata references, and operation identity. It does not yet
claim stable entry identity across renames. Deletion creates a tombstone. A
version vector that dominates another replaces it. Concurrent edits retain all
bytes: a stable order over operation IDs selects the original-path version and
places the others at deterministic conflict paths. Concurrent delete/edit,
move/edit, directory/file, and ancestor/descendant cases retain content rather
than silently dropping it. Wall-clock time does not decide a conflict.

The first milestone represents a rename as authenticated delete plus create
with preserved history. Stable-entry-ID rename is deferred until its merge,
crash, and cross-platform semantics are specified and tested.

The first version retains all tombstones and authenticated operation history,
with explicit count and disk quotas that pause work visibly when exhausted.
There is no collection by age. Compaction requires an authenticated checkpoint
that preserves causal closure, equivocation evidence, and deletion history;
that later protocol must prove non-resurrection before it can replace records.

### Scanning, transport, and apply

Scanners publish changes only after a complete authoritative scan. Watchers,
mtime, size, inode, and document IDs are hints; content hashing confirms a
publication. Any scan error, permission loss, cancelled scan, traversal limit,
cursor failure, mutation during inventory, or watcher overflow publishes zero
deletes and preserves the prior index.

Peers exchange authenticated epoch/frontier summaries, then fetch bounded
missing signed operations and content. New members receive a single-use,
authority-signed read-only bootstrap permit bound to their transport and writer
keys. They replay the full verified history through the permit frontier,
including tombstones and conflicts, and journal-apply its live content before
signing a bootstrap receipt. Only a subsequent membership grant activates them.
Interrupted bootstrap is resumable and remains inactive. A changed authority
head requires catch-up and a replacement permit before activation.

The accepting epoch commits the exact bootstrap receipt. Its applied frontier
becomes a minimum causal context for that writer's subsequent publications;
the member cannot omit known tombstones by presenting a fresh writer counter.
Regaining write permission also requires an up-to-date bootstrap receipt. A
removed writer ID is never silently re-added or reused.

Receipt, local application, and peer acknowledgement have separate frontiers.
A device is up to date only when the root has applied the shared frontier,
connected peers acknowledge it, and no conflicts or pending errors remain.
Offline peers remain visibly waiting.

Remote apply is journaled before a user-file mutation. The private journal
records a plan and inventory digest, staged content, per-entry completion, and
the resulting frontier. Unix apply uses no-follow traversal, revalidation,
staging, fsync, and atomic rename. SAF apply uses a per-entry app-private
journal and a provider capability probe. If safe replacement or rename is not
available, it uses copy-on-write conflict siblings or leaves a visible pending
action; it never assumes atomicity. Apply-origin operations update the index
before the next scan so they do not feed back as local edits.

Paths are UTF-8 NFC on the wire. Each platform detects normalization and
case-fold collisions before publication or apply. Unsupported target names
remain visibly pending with their original spelling and retained source bytes;
they are not silently renamed. Representable conflicting entries use the
deterministic conflict plan with an actual no-clobber target probe. Internal
journal and staging storage lives outside the shared user folder.

### Platform promise

Unix uses event watching only as a rescan hint. macOS uses security-scoped
access and FSEvents hints; lost scope pauses the folder. Android's source of
truth is an authoritative SAF rescan. `ContentObserver` is only a foreground
hint; [WorkManager periodic work](https://developer.android.com/reference/androidx/work/PeriodicWorkRequest) is inexact, and force-stop prevents background
work until the user next launches the app. The product must show that state.

## Acceptance gates

Implementation may graduate from planned only after all of these pass:

1. Two and three writer model/property tests converge for every delivery order
   and partition, including duplicates, replay, equivocation, old epoch, and
   removed-member operations.
2. Concurrent edits, delete/edit, move/edit, and path-shape conflicts preserve
   every byte with identical conflict paths on all peers.
3. Tombstones survive until all active acknowledgements or explicit removal;
   removal revokes folder access and rejects later removed-member writes.
4. Crash injection at every journal phase reaches one exact result without
   duplicate conflicts, accidental deletes, or generated feedback operations.
5. Failed/partial scans publish zero deletes. Bounds hold for entries, vectors,
   batches, checkpoints, and hostile provider responses.
6. Unix proves two-node convergence on explicit writable mounts. macOS proves
   bookmark/FSEvents recovery. Android proves SAF failures, process death,
   background delay, and force-stop status.
7. Existing backup and restore tests still pass; a sync failure cannot mutate
   immutable backup history.

## Open engineering decisions

- Integrate the bounded signed records and exact causal-history checks with
  peer exchange. An authenticated checkpoint/compaction protocol remains
  outside version 1; claimed dependencies still require their signed history.
- Connect deterministic conflict projection to actual platform capability and
  no-clobber checks. A pure path plan does not prove a destination is writable.
- Decide when stable-entry-ID rename supersedes delete-plus-create.
- Decide shared exclusion-policy versioning before selective sync is offered.
- Implement and model-check the survivor freeze barrier and full-history
  bootstrap specified above, including interruption and disconnected edits.

These are implementation decisions, not approval blockers for this planned
direction. They must be resolved before their respective protocol or platform
code lands.

## Implementation checkpoint

The core now contains bounded signed-operation, membership, bootstrap and
freeze record codecs, version vectors and causal registers, exact-history
frontier checks, deterministic conflict projection, a read-only Unix inventory
scanner, and encrypted private event-log storage. The event log holds one
folder lock, checks quotas before append, syncs before exposing committed
state, and requires reopen after an uncertain write. Only a valid incomplete
physical EOF frame can be repaired; a complete corrupt record halts replay.

The concrete event machine retains exact epochs and permanent writer/key
history. It accepts bootstrap permits and receipts, bootstrap-backed Add and
Read-to-RW upgrades, ordinary Read removal, and operations under uninterrupted
historical write grants. New writers must include their bootstrap frontier in
subsequent operations. Advancing the membership head retires all pending
evidence for the old head while retaining exact duplicate/equivocation history.
Receipt signatures are checked before classifying equivocation. Prepared
changes remain bound to one engine instance and revision, with independent
operation, path, epoch, pending-evidence and conservative index-byte quotas.

The local publisher derives a folder-global counter and full clock and exposes
signed bytes only after durable append. File publication additionally requires
an unforgeable complete-content receipt matching the file's digest, length and
executable mode, plus the log's exact folder/installation/generation. New log
handles require empty replay state.

Local content storage uses independent sync-only XChaCha20-Poly1305/HKDF keys,
keyed chunk/manifest locators and folder/installation/generation authentication.
Bounded ordered manifests exclude executable mode from cache identity. Fixed
4 MiB chunks are verified individually and as a complete ordered file before
issuing a retention receipt. Private staged records are synced and promoted
without replacing an incumbent; both parent directories are synced, destination
first. Reopening counts all final and abandoned staging files against finite
quotas. This first store never evicts content or cleans staging implicitly;
explicit reclaim of proven unreferenced staging remains future work. Quotas
describe logical encrypted bytes and objects, not filesystem overhead or RSS.

Ready-state startup can now request existing-only private locks and content
storage. That path never creates a missing child directory or lock and still
validates complete inventory, quotas and retained objects. Initialization keeps
its separate creation-capable path. The coordinator must select these modes
explicitly; a failed ordinary reopen is not permission to reinitialize storage.

Each local sync installation now creates an immutable protected record with
independent installation/generation identifiers, a fresh writer signing key,
separate event/apply log keys, and a content-generation secret. Creation requires
a fresh private root and never replaces an incumbent. Reopen authenticates the
bounded canonical record and syncs it before returning keys. Entropy failures,
including secret wrapping, return fixed errors rather than panicking. A real
handoff test retains the outer lock while opening child stores and proves that
content and folder-global counters survive repeated close/reopen cycles.

Authority and local transport pins now live in a separate immutable protected
record bound to the exact folder, installation, generation, writer and local
signing public key. Creation requires only the installation record and held
root lock to exist; opening never substitutes a key from incoming history.
Malformed, missing or changed trust fails closed. A stable opaque setup
commitment binds these authenticated identities for the upcoming readiness
marker. Replay configuration derives from these durable pins. This authenticates storage of the setup workflow's
trust decision; live transport still has to prove private-key possession.

File application can obtain a bounded operation view only from fully admitted
history, bound to exact event bytes or to an authenticated journal's exact
operation ID/digest. Historical acceptance alone does not authorize applying
an obsolete projection; the applier must still check current registers and all
desired journal fields. A bounded ordered lookup detects live descendants so
creating an ancestor file cannot hide child entries absent from disk.

Committed event history can now be read in pages of at most 256 records and
4 MiB plaintext, with one bounded frame of read/decryption scratch. Opaque local
cursors are bound to one open handle and cannot survive reopen or cross into
another log. Reads authenticate each frame and require exact accepted-history
duplicates, recheck the lifetime lock and file length, and return no partial
page on failure. Apply journals are excluded. The continuation can read later
appends after reaching the current end. The runtime must separately authorize
each transmission; these local cursors and page ordinals are neither wire
tokens nor peer acknowledgements. Future write-loss replay must retain exact
historical duplicate classification even after changing a writer's live grant.

The first Unix applier now creates absent files/directories from a current,
fully admitted operation and durably verifies an existing directory. Encrypted
Intent, StageReady and Applied records bind the operation, root and staged inode.
No-follow parent traversal, bounded portable-name checks, verified retained
content, file/parent sync and no-replace promotion precede success. Interrupted
retries reuse the same intent; reopening against a different root fails before
restaging. Unproven stages are preserved as pending rather than guessed safe.
Current applied files are revalidated with bounded, cancellable hashing. Machine
quotas include a fixed initial index allowance and per-record owned storage.
These are logical accounting bounds, not measured RSS guarantees.

Existing-file adoption now verifies a matching incumbent without rewriting its
bytes or permissions. The intent records its exact inode and ordinary permission
bits. Hashing and sync use the same held no-follow descriptor; named identity,
parent and current projection are checked before durable success. Matching
existing directories can also be adopted, but a missing directory is never
created by adoption. Observed content, mode or identity changes become durable
conflicts; transient read/stat/sync failures keep the original intent retryable.
Special-mode files, symlinks and hard links are rejected. Cancellation after
filesystem sync cannot append Applied. Adoption proves the incumbent's state,
not that its content is retained for a peer or bootstrap acknowledgement.

Create promotion rechecks staging immediately before the no-replace operation
and verifies the promoted object afterward. This is not an inode compare-and-swap;
an observed substitution in the syscall window becomes a preserved conflict.
The applier reports tombstones as unsupported. Missing/unsafe parents and portable-name
collisions discovered before an intent are visible errors, not durable conflict
records. The applier now uses controlled encrypted-log replay with pause/cancel
checks between bounded frame reads and state transitions, followed by cancellable
filesystem reconciliation. A started valid EOF-tail repair finishes sync before
honoring interruption. Other callers must opt in; synchronous filesystem calls
and individual bounded transitions are not preempted. Rechecking every current
applied file is a conservative correctness barrier that still needs performance
measurement. It does not implement an applied acknowledgement frontier.

Android now has a metadata-only SAF observer using direct resolver queries and
two matching bounded scans. It preserves opaque document IDs and returns no
observation after null/loading/error cursors, partial or virtual documents,
malformed rows, traversal limits, cancellation or observed mutation. Entry
reservations include pending ancestor siblings, and string/aggregate quotas are
charged before retention. Preliminary portable-name checks defer authoritative
Unicode handling to Rust. These observations prove neither file content nor an
atomic provider snapshot and cannot authorize adoption, deletion or sync success.
Generic SAF mutation semantics still require a separate implementation decision.

Network folder sync remains unshipped. Write-loss/freeze transitions and their
history reconciliation still fail closed. The full folder coordinator, peer
exchange, safe replacement and deletion, conflict materialization, native setup,
Android content verification/application and the
multi-device acceptance gates above remain outstanding.
Existing Backup and Restore behavior remains independently tested.
