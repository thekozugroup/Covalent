# Interlink automatic-update control

The first privileged settings feature is an explicit choice to enable or pause
Pi automatic updates from the approved Interlink phone. The signed core command,
Pi atomic policy transaction and root verifier exist. The node now carries fixed
context/apply/lookup operations over its authenticated appliance QUIC channel,
with an explicitly installed privileged host bridge. The phone now has a durable exact-request outbox, bounded authenticated local
APIs and a Material3 Expressive confirmation/control surface. Source compilation
and host contracts pass; normal-package Android execution and installed Pi/phone
acceptance are separate gates.

## Authentication and transport

`covalent-core::appliance_updates` signs one version1 canonical JSON payload:
`schemaVersion`, the existing full owner-approved `bond`, a stable `actionId`,
the Pi-issued `expectedRevision`, and a boolean `enabled`. The proof contains
only exact `payload` text and a canonical Ed25519 signature. It cannot carry a
repository token, path, URL, component version, deletion option or shell command.
The signing domain is `covalent/appliance-updates/automatic/v1`; import and
withdrawal signatures cannot authorize this setting.

`ReadApplianceUpdates` and `ApplianceUpdate` use the existing dedicated appliance
ALPN. The authenticated envelope binds requester, target, certificate, verb,
proof, nonce and time. No legacy read fallback or write retry is added. Ordinary
sync pairing is insufficient: the node checks its separate approved full peer
identity and native bond generation before calling the host, and again before
returning a result. A shared bounded host-operation permit prevents overlapping
node import/update calls. Missing or invalid context is unavailable, never an
invented configured/enabled policy. A busy host produces a fixed busy response.

Node signature admission checks the approved phone's intent. It does not make
the caller's root generation or settings revision trusted. `ApplianceUpdateHost`
must independently verify its protected full bond and current durable revision
under the same privileged lock as publication. The Pi's root implementation does
this through its fixed `controllerUpdateContext`, `controllerUpdateApply` and
`controllerUpdateLookup` commands. The native bridge verifies the root socket's
UID before writing; only those commands and a signed proof can cross it. Requests,
responses and the whole exchange have fixed size/time bounds.

The strict context/receipt decoders reject unknown, duplicate, missing, malformed
and oversized fields. A configured context requires a nonnil revision; an
unconfigured context cannot claim enabled. A receipt binds the exact action ID
and payload digest. Applied means the chosen boolean with a new revision.
`notObserved` is a read observation of the expected revision, never cancellation,
proof a queued request cannot run later, or permission to repeat a mutation.

## Durable effects and phone recovery

The Pi retains credential, boolean, revision and latest receipt in one protected
atomic record, with file/parent fsync and readback. Every choice consumes a new
revision, including a no-op. An exact latest duplicate reads its receipt; an old
request cannot overwrite a newer policy. Local changes invalidate old revisions.
Owner revocation blocks both apply and lookup. Credentials remain local.

The phone persists and independently reads back the exact signed request before
its only apply attempt. An exact duplicate confirm returns the saved record;
restart, refresh and check never resend apply. A missing acknowledgement stays
unconfirmed. Read-only lookup can recover the matching latest Pi receipt. New
explicit choices use a fresh Pi context/revision and a new stable action ID;
they may replace an unconfirmed choice without replaying it. Even keeping the
current boolean is an explicit signed choice that consumes a revision on the Pi.
If an older queued choice wins a race, read fresh settings and confirm again;
the app never automatically sends a new mutation to hide that race.

Fresh authenticated state can prove a prior request cannot change current policy:
the full approval bond changed, or the nonnil Pi revision differs from the saved
request revision. The outbox retains that observation without claiming the old
request failed or never ran. A notObserved receipt with the original revision
alone cannot resolve or cancel a request. Applied receipts remain monotonic.
Only resolved history can be pruned at the32-record bound; unknown commands are
not discarded. Missing/damaged/foreign/linked state stays retained and refused.
The existing import and update journals share the same private-file locking,
size/owner/link guards, file/parent fsync, atomic publication and exact readback.
No source, account, sync pairing or import record is reset by update controls.

The companion checks saved selection and full paired identities before and after
IO, rechecks the review context before confirm, and signs only through its local
node. The switch follows freshly read Pi state. Offline, unapproved, unsupported,
unconfigured, busy and unreadable-history states cannot change policy. An
unconfirmed choice offers read-only Check status and an explicit Keep current
setting choice. No root setting, key, credential or arbitrary operation is sent
from the presentation layer. Token setup and all update/install/idle/recovery
policies remain unchanged.

## Acceptance boundary

Core signing and the privileged Pi transaction have separate retained source-bound
qualification. The new transport tests use real paired loopback QUIC and real
signatures with an in-memory privileged-host double. They do not prove durable
root publication, actual LAN/Tailscale roaming or an installed Android service.
Native adapter tests use Unix sockets and synthetic root replies; Android
compilation is a separate check. Report exact final test results in working.md.

No actual Pi/phone, account, pending Pictures run, source media, update candidate,
recovery or reusable image changes in this stage. The existing hosted API37
platform assertion and Android36 guest ANR failures remain failed acceptance
runs; these transport changes do not repair them.
