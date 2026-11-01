# Interlink automatic-update control

The first privileged settings feature is an explicit choice to enable or pause
Pi automatic updates from the approved Interlink phone. The signed core command,
Pi atomic policy transaction and root verifier exist. The node now carries fixed
context/apply/lookup operations over its authenticated appliance QUIC channel,
with an explicitly installed privileged host bridge. The phone outbox/readback
and Material3 Expressive control are still pending; no local mutation API or
companion switch is exposed before that recovery boundary is implemented.

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

Next persist the phone's exact signed request before sending, recover a lost
reply by lookup, and show freshly confirmed state and an unconfirmed outcome.
An unconfigured updater must request setup rather than claim enabled. Existing
idle, signature, recovery, account-preservation and source-retention gates remain
authoritative. Enabling scheduling cannot choose or immediately install a release.

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
