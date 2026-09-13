# ADR 0008: Rclone for simple one-way links

Status: accepted; sole active engine, release verification in progress, 2026-09-13.

The user requires rclone as Covalent's transfer engine. The product sends files
from one source to one or more destinations. It does not need a two-way conflict
engine. This supersedes ADR 0007; remaining verification determines release
readiness and does not reopen the engine choice.

Bundle upstream rclone v1.75.1 through `packaging/rclone`, with only the local,
SFTP, and WebDAV backends and the commands Covalent uses. Keep upstream transfer
code unchanged. Covalent owns the native interface, pairing, folder grants,
shared link settings, scheduling, status, and per-destination deletion policy.

Source access is read-only SFTP, authorized for an exact paired key and link.
The signed pairing binding carries the SSH key; a stored digest alone cannot
authorize a new transport. Older connections need an authenticated binding
update. Such a connection must not block unrelated current connections.

Android streams authorized SAF folders through a private loopback WebDAV
adapter. macOS retains native folder access and its menu bar interface. Docker
uses explicit folder mounts. Transfer processes run when needed; automatic
mode uses a bounded cadence. Performance claims require measurement.

Source deletion keeps destination files by default, with optional propagation.
Destination deletion remains absent unless restoration is selected. Rclone's
ordinary copy behavior does not provide the second rule, so Covalent retains
the minimum durable per-destination state and passes exact file lists. An
uncertain interrupted operation must not silently repopulate deleted files.

Remove obsolete Syncthing runtime, build patches, and package assets as the
replacement is integrated. Preserve access to existing user data and the
historical evidence in Git. Ship rclone and selected dependency notices with
each package. The [requirements](../product/requirements.md) and
[completion ledger](../release/completion-progress.md) define acceptance.
