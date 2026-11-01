# Interlink automatic-update control

The first privileged settings feature is an explicit choice to enable or pause
Pi automatic updates from the approved Interlink phone. The current code adds
its cryptographic command contract only. No platform mutation endpoint, Pi
credential migration or companion toggle is exposed yet.

## Implemented command boundary

`covalent-core::appliance_updates` signs one version1 canonical JSON payload:
`schemaVersion`, the existing full owner-approved `bond`, a stable `actionId`,
the Pi-issued `expectedRevision`, and a boolean `enabled`. The proof contains
only exact `payload` text and a canonical Ed25519 signature. It cannot carry a
repository token, path, URL, component version, deletion option or shell command.
The signing domain is `covalent/appliance-updates/automatic/v1`; import and
withdrawal signatures cannot authorize this setting.

The verifier takes both expected bond and expected revision from independently
trusted privileged state. It rejects other phones/Pis, either changed approval
generation, stale or nil revisions, invalid identity fingerprints, unsupported
versions, nil action IDs, oversized payloads, altered intent, unknown/missing/
duplicate fields, noncanonical JSON and malformed/truncated/padded signatures.
The ordinary sync pairing and selecting a Pi do not supply this authority.

## Required effect and recovery boundary

Verification does not consume a revision or write settings. The privileged
handler still must hold the owner/state lock, compare the real current durable
revision and atomically publish the new policy with a new revision, including
for an unchanged boolean. A signed choice alone is not replay protection.
Local credential or policy changes must also invalidate the previous revision.
Preserve existing repository credentials and all unrelated state; never reset
damaged configuration or create credentials from a network request.

Keep the exact request ID/digest, old revision and result with the policy write
so a lost response can be resolved without repeating an effect. A read-only
duplicate lookup may verify against the matching persisted receipt's old
revision; applying a new choice must always use current locked state. Do not
trust the request itself as the source of an expected revision. A newer policy
must never be overwritten by an older signed request.

Next wire the native/root verifier and durable policy transaction, then the
bonded node transport and phone's durable request/readback flow. The Material3
Expressive toggle must display freshly confirmed Pi state and disclose an
unconfirmed outcome. An unconfigured updater must request setup rather than
claiming automatic updates are enabled. Existing idle, signature, recovery,
account-preservation and source-retention gates remain authoritative.

## Current verification

All186 core library tests pass, including seven new command-boundary tests.
Clippy covers all core targets/features with warnings denied. No existing
wire operation, import domain, dependency, application package or version changes.
No Pi/phone state, pending Pictures run, update bundle or reusable image changes.
This is source-only security groundwork, not a completed remote-control feature,
Android-native execution or installed appliance acceptance.
