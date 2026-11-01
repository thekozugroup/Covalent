# Owner-bound import transport and pending requests

The native API now carries drive/camera confirmation context and exact signed import
choices over the existing pinned covalent-appliance/1 channel. This is the
backend for Interlink; the native Android client/confirmation and foreground
lifecycle wiring are now connected (interlink-imports.md). Ordinary hosts have
no import bridge and report unsupported.
No new listener, trust-by-discovery, arbitrary settings command,
source deletion or cloud-verification authority is added.

A fresh host context contains full approved phone/Pi keys, independent root and
native generations, opaque source references, complete attachment/preview IDs,
source counts, fixed destination, protected space and action session/sequence.
Source action tuples must agree within each kind's journal observation. Volume
and camera services have independent sessions/sequences. Unknown
space and incomplete/disconnected previews cannot advertise confirmation. Counts
include previously imported files. The native bond and current pairing are
checked before and after host access; the root host must independently validate
its private current approval at actual admission.

POST /api/v1/appliance/imports/context reads context for a retained peerId.
POST /api/v1/appliance/imports/confirm accepts the exact full choice shown to the
user. The phone checks a fresh signed context, signs with its existing identity,
and durably reserves the complete proof/source snapshot before sending one
mutation. Repeated confirmation of the same action only returns saved state;
changed fields with that UUID refuse. No address, Pi API token or private key is
stored in the outbox or supplied by the UI.

GET /api/v1/appliance/imports/actions reads pending/saved local actions.
POST /api/v1/appliance/imports/check accepts actionId and sends only a receipt
lookup for that saved proof. Unknown or uncertain results remain unresolved.
POST /api/v1/appliance/imports/withdraw signs the original canonical payload
under a separate withdrawal domain and persists that change of intent before
sending it. Approval signatures cannot authorize withdrawal or vice versa.
Repeated withdrawal calls check its result rather than replay a write.

The Pi's withdrawal journal must consume the original action identity/sequence
without applying it. A late original packet then returns withdrawn. Withdrawal
of an already admitted/uncertain choice must report that existing outcome;
it cannot claim cancellation or undo copying. The host requires a distinct
withdrawal signature, exact protected bond and shared admission/revocation lock.
The compatible host contract requires withdrawalAvailable=true; older/mixed
components fail unavailable before a new approval is sent.

A source pause remains available during uncertainty. Once its exact later pause
is acknowledged, older unresolved requests for that same full Pi identity and
source can be marked superseded by that pause. This does not prove what earlier
work copied and does not remove originals. Later automatic-import settings are
shown separately from old admission receipts.

Uncertainty blocks fresh once/remember choices within the same full protected
bond and service kind. A pending volume does not block a separate camera journal.
A freshly verified replacement root/native bond cannot admit the old
proof; it can confirm new work without inventing an old result or requiring the
old card to remain attached. At the history limit, earlier non-executable
requests for that same Pi can be pruned along with older resolved entries.
Current-bond uncertainty remains protected. All reservations still require a
fresh host context; the caller cannot choose an arbitrary replacement bond.

Outbox state is bounded to32 actions/128KiB, privately owned, canonical and
single-writer locked. Unsafe, missing,
linked, changed, malformed or foreign-identity state refuses import operations
without resetting it or stopping ordinary sync. An uncertain file publication
poisons the writer. Restart never dispatches a saved import or withdrawal.
Accepted is admission only; expired is an invalidated confirmation; neither
means copied, indexed, backed up to Google Photos or eligible for deletion.

Real pinned QUIC/native API tests cover owner gating, lost reply after admission,
service recreation, exact duplicate/conflicting UUID, separate withdrawal signing
and a late original request after withdrawal. Host fixtures have no real file
importer. Root Android service/crypto evidence is recorded separately by the
private appliance project; actual phone/Pi/roaming/camera/cloud/update/image
acceptance remains required. New actual QUIC coverage verifies explicit revocation/
reapproval with an old card absent, preserved unknown history and late-proof
refusal. The new local API is not a released companion app.

## Camera context and mixed versions

Context version2 requires `cameraServiceAvailable` and a boolean `canRemember`
on every source. A camera without a stable serial can only be confirmed once
for its current attachment. Volume availability is independent of camera
availability. Version1 remains volume-only, with those new fields absent.
Explicit nulls, missing version2 fields and inconsistent per-kind journals
refuse. Service availability, current bond, exact preview and protected space
are all required; a transport connection alone grants nothing.

The private Pi bridge requests version2 explicitly and gates camera support on
the verified native executable's camera protocol marker. The signed
ReadApplianceImports operation now has an optional `context_version` field.
Absent means legacy context1; a modern phone requests2. The field is signed.
Its absence retains the exact original operation bytes and digest. Explicit
nulls, strings and fractional versions refuse; unsupported versions never reach
the import host.

A newer host validates its complete protected context before projecting a
volume-only context1 for older readers. It removes camera entries and modern
fields, marks omitted sources, and preserves the full bond, volume journal,
preview, destination and storage. It never relabels a camera as a drive or
converts a signature. Older readers therefore keep their drive flow while a
newer companion can request both source kinds.

After TLS pin verification, a modern reader permits exactly one legacy read if
the peer closes before sending any response bytes. This can represent an older
strict decoder; the second query still requires all existing transport,
signature and owner checks. Signed refusals, busy results, partial/oversized
frames, malformed responses and invalid signatures never trigger fallback.
Both reads share the existing45-second exchange budget. A compatible earlier
camera host can still return context2 on the legacy query; modern readers accept
either valid version. This is read negotiation, never a write retry or a weaker
authentication mode.

Root/native component updates still require their ordinary coordinated bundle
and installed upgrade qualification. Mixed-version wire tests do not prove an
old installed APK or physical Pi upgrade. No fallback resets an outbox, rewrites
an old volume proof or retries an uncertain mutation. The outbox format stays
version1 and retains old volume requests beside new camera proofs.
