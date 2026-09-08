# Folder synchronization

Status: planned — not available in released Covalent builds.

Folder synchronization will be a separate feature from Backup and Restore. A
backup remains an immutable recovery point. A sync folder keeps selected local
folders converged between devices that members explicitly authorize.

The first implementation connects authorized folder devices directly. Each
participating device has access to the chosen folder's contents. Existing
encrypted backup devices remain a separate choice; storing backup copies does
not automatically give a device access to sync folders. An untrusted sync relay
is outside this first milestone.

## The intended flow

1. Choose a folder.
2. Choose the devices allowed to join it and their roles.
3. Sync.

The app validates that the chosen folder is writable and safe to manage before
it calls the folder active. Each device has its own identity. Pairing a device
does not share folders automatically: the owner must explicitly approve the
folder, device, and role.

Users will see a folder name, local path, members, role, last complete scan,
last mutually acknowledged sync, pending changes, conflicts, and a specific
paused/error action. “Waiting for a device,” “permission needed,” and “up to
date” are different states.

## Expected behavior

- Edits made while a device is offline are synchronized after it reconnects.
- Edits to different files merge.
- Concurrent edits to the same file keep every version. One stays at the
  original path and the others receive stable, readable conflict names.
- A delete racing an edit retains the edited bytes as a conflict; it is never
  silently treated as a deletion.
- Renaming is initially represented as delete plus create with retained
  history. It is not presented as lossless rename tracking.
- Unsharing a folder stops future synchronization and does not delete local
  files.
- Removing a device ends its future folder access. Removing write permission
  first requires the remaining devices to agree on the history they have
  already accepted. If one cannot be reached, the app waits or asks the owner
  to explicitly remove that device too. Previously received files stay intact;
  later edits from a removed device remain local for review.

Synchronization will not report success merely because a transfer was sent.
It reports up to date only after the local folder has applied the shared
state, connected participants acknowledge it, and no unresolved conflicts or
errors remain. Offline participants remain visibly waiting.

## Safety rules

Only a complete successful scan may infer a deletion. If access is revoked, a
folder changes while being scanned, a provider cursor fails, a scan is
cancelled, or a watcher overflows, Covalent pauses or rescans and publishes no
deletions.

Downloaded changes are verified, staged, and journaled before user files are
changed. Restarting after interruption resumes or safely reconciles that
journal. Sync folders use path confinement and no-follow handling on Unix.
On document providers that cannot safely rename or replace files, Covalent
uses a visible pending/conflict path instead of assuming destructive operations
are atomic.

An encrypted backup provider remains outside the sync folder. It stores backup
objects through the existing backup protocol and does not receive sync
operations, grant folder access, or resolve sync conflicts. Direct sync
participants are explicitly authorized to read the chosen folder.

## macOS and Android

On macOS, a revoked folder permission pauses the folder until the user grants
access again. File-system events are hints; Covalent verifies them with a scan.

On Android, SAF rescans are authoritative. `ContentObserver` helps while the
app is active but is not a recursive change log. [Periodic WorkManager work](https://developer.android.com/reference/androidx/work/PeriodicWorkRequest) is
inexact, and Android force-stop prevents background work until the app is
opened again. The app will show that limitation rather than promise continuous
background sync.

## Release criteria

This feature will remain planned until Covalent proves deterministic
multi-device convergence, conflict-byte preservation, safe deletion handling,
member-removal admission barriers, crash recovery, bounded hostile-input handling, and
platform-specific filesystem behavior. Backup and Restore remain available
independently throughout that work.

For reference, the product behavior takes inspiration from Syncthing's
[Understanding Synchronization](https://docs.syncthing.net/users/syncing.html)
and its [Block Exchange Protocol](https://docs.syncthing.net/specs/bep-v1.html).
They describe an external reference implementation; this planned protocol is
not Syncthing/BEP compatible.
