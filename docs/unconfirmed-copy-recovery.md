# Unconfirmed one-way copies

If a receiving worker writes a file but its completion acknowledgement is lost,
the receiver cannot safely decide that the file belongs to the link. It retains
the admitted inventory and holds the uncertain path for review. Unrelated new
files can continue to arrive. The link remains incomplete until that uncertainty
is resolved; successful later transfers do not turn it into a completed link.

The same hold applies when a new incoming file would replace an existing file
that the link does not own. Matching names or contents do not establish
ownership. Held scopes include canonical Unicode case/normalization aliases and
their parents/children; stored source proofs keep their exact original names.
This is conservative name protection, not a guarantee about every filesystem's
alias rules or concurrent external writes.

Each held attempt preserves its complete admitted source snapshot/index and
narrows only the paths that remain uncertain. It is excluded from later copy,
verification and ownership/deletion advancement. Verified unrelated paths still
advance. Restart preserves both the held records and verified later deliveries.
Source deletion never grants ownership of an uncertain destination original.
The existing restore-deleted-files choice can release a held path when its
destination is absent. It does not allow replacing an existing unknown original.

The private folder policy remains strictly parsed and size limited. At most 512
unresolved attempts are admitted per folder. Invalid, overlapping or excessive
records fail closed. Older readers accept policies without held records but
refuse a policy containing the new `unresolved` field. Preserve that policy;
never strip the field, clear a journal or force a successful run to enable a
rollback. Production migration and signed rollback acceptance remain separate.

## Read-only observations

`GET /api/v1/sync/recovery` requires the existing local bearer authentication.
It is separate from owner-loss recovery. Ordinary paired sync peers do not gain
this API or any authority to resolve a hold. No query parameters or mutations
are accepted.

The response has `schemaVersion: 1`, `observationsComplete`, and a `folders`
array. Each current trusted incoming one-way folder reports its `folderId`,
`available`, `unconfirmedFiles` and `unconfirmedSourceBytes`. Counts are nullable
when unavailable. Bytes are from admitted source inventories, not destination
disk use or cloud storage. No names, paths, credentials or source media are
returned. Removed and outgoing shares are excluded; paused incoming shares
remain observable.

Only one blocking metadata read is admitted at a time, off the lifecycle lock,
with an aggregate 8 MiB read budget plus the over-limit sentinel byte. The permit
survives request cancellation until that read finishes. Trust, journal revision
and active folder jobs are checked again before responding. Concurrent changes
produce the existing typed retryable busy refusal. An active unconfirmed write,
oversized policy or invalid private file produces unavailable/null observations.
Zero counts establish neither completed transfer nor cloud backup, indexing,
consent or the absence of untracked files.

An authenticated companion review screen and explicit resolution protocol are
still required. This endpoint cannot approve overwrite, deletion, adoption or
Google Photos cleanup. A transfer receipt is never cloud-backup confirmation.
Explicit destination-root replacement refuses recorded unconfirmed writes before
changing consent, again after the owned worker exits, and at the actual policy
reset. A late refusal resumes the unchanged configuration where possible. The
existing folder capability can be renewed without clearing held records. An
unreadable policy also refuses reset; opening a writerless FIFO cannot block the
metadata reader. Confirmed ownership can still permit a scoped reset without
erasing another folder's independent proof.

The local API returns `409 folder_sync_recovery_required` with explicit
`retryable: false` only when the proposed root was not committed. Android drops
only the matching pending proposal after that exact version-1 contract, saving
the acknowledged original choice durably. Reopen then cannot replay the refused
proposal or unnecessarily block startup. Network failures, unknown contracts,
failed local saves and an already-committed reset retain the exact saved intent.
An already-committed reset needing review uses the existing
`folder_sync_needs_attention` refusal. Never infer pre-commit rejection from a
generic conflict or non-retryable flag alone.

Do not retarget/reset a link to clear a hold. Explicit owner review/resolution,
production migration and signed rollback acceptance remain unfinished.

## Verification scope

The real-worker regression suppresses one completion acknowledgement after an
actual copy, preserves that destination original after a source deletion, adds
a second independent local-file collision, then delivers unrelated new files
before and after both nodes restart. It checks root-change refusal, exact
admitted proof retention,
lack of false ownership, incomplete run status and redacted recovery counts.
The old implementation fails at the first unrelated-file delivery deadline.
Original 60-second file deadlines remain in force.

Unit/service tests cover alias scopes and siblings, strict persisted state,
old-reader refusal, bounded metadata reads, authentication, query rejection,
unavailable observations, lifecycle-lock isolation and preservation after share
removal. They do not establish real Pi boot, phone roaming, external storage or
camera import, production migration, Google Photos confirmation or safe cleanup.
Android JVM tests cover exact refused-proposal persistence and reopen, stale
choices, failed saves, explicit retry contracts and uncertain startup replay.
