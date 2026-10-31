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
The existing explicit destination-root replacement resets that folder's policy.
Preserving held evidence or refusing that reset still needs implementation and
acceptance before production installation. Do not retarget/reset a link to clear
a hold. This draft is not a production migration or rollback candidate.

## Verification scope

The real-worker regression suppresses one completion acknowledgement after an
actual copy, preserves that destination original after a source deletion, adds
a second independent local-file collision, then delivers unrelated new files
before and after both nodes restart. It checks exact admitted proof retention,
lack of false ownership, incomplete run status and redacted recovery counts.
The old implementation fails at the first unrelated-file delivery deadline.
Original 60-second file deadlines remain in force.

Unit/service tests cover alias scopes and siblings, strict persisted state,
old-reader refusal, bounded metadata reads, authentication, query rejection,
unavailable observations, lifecycle-lock isolation and preservation after share
removal. They do not establish real Pi boot, phone roaming, external storage or
camera import, production migration, Google Photos confirmation or safe cleanup.
