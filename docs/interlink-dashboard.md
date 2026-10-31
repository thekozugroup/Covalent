# Interlink selected-Pi dashboard

Interlink now reads the existing authenticated appliance status protocol and
keeps one explicit selected Pi in credential-encrypted phone storage. It does
not automatically select a discovered device, acquire owner authority or replace
sync pairings. Existing photo links remain managed below the device card.

The chooser lists current paired identities, with names from non-revoked local
grants and comparison fingerprints. Confirmation rereads current pairing before
persisting the phone's full identity, Pi full identity and chosen display name.
The stored choice contains no network address, API token or media. A short
fingerprint is display-only; identity matching compares the complete paired key.
The authenticated local node validates Ed25519 keys/fingerprints and verifies
the remote signed status. Android also requires matching device IDs, exact
supported schemas/types, consistent unavailable/null observations and false
cloud-verification/source-cleanup capability flags.

`GET /api/v1/appliance/controller` supplies the local phone identity and paired
key references. `POST /api/v1/appliance/status` carries only the selected peer ID.
Both use the phone node's local bearer; the Pi token stays on the Pi. Responses
are bounded to64KiB and8KiB respectively. Bounded object requests refuse
redirects. This does not grant remote settings, import, deletion or overwrite
authority. The Pi's separate owner approval must already exist for remote status.
The currently deployed older Pi software does not provide this protocol.

## State and persistence

The card separates setup, checking, connected, owner approval required, Pi
unavailable, phone unavailable, changed/revoked identity, unsupported controls
and unreadable saved choice. A stopped/unavailable Pi folder service is distinct
from losing the control connection. Running service or fresh health never means
that transfers or Google Photos backups completed. Cloud status is explicitly
unavailable in this protocol. Raw transport/server messages are not displayed.

First run waits for explicit phone-node setup without probing. A saved choice
survives disabled service, network failures, owner refusal and unsupported
versions. Locked, malformed, oversized or future saved state is not converted
into empty setup. Selection and fresh bond-generation saves require synchronous
persistence, exact expected prior choice and current full paired identity.
A delayed status cannot replace a newer choice or publish connected state for
it. Failed saves surface attention and do not publish successful connection.
Selection is a local reference, not authority on the Pi.

The visible card refreshes every15seconds while its lifecycle is STARTED and
supports manual refresh. UI operations use one mutex; lifecycle cancellation
discards results while an already blocking, bounded HTTP call finishes. It does
not start a second overlapping probe. No background controller poller or new
always-on service is installed. LAN/Tailscale address recovery still belongs to
the native transport; this card neither invents an address nor proves roaming.

## Verification and remaining work

JVM tests cover the actual local HTTP methods/body/bearer, bounded responses,
redirect refusal, exact schema/types, unavailable/null consistency, forged
capability claims, key/context validation, explicit selection/reopen, first-run
admission, changed/revoked keys, refusal/unknown outcomes, locked/damaged state,
failed saves and delayed-response conflicts. Native Compose tests use synthetic
states to verify connection loss removes stale running claims while retaining
the name, service unavailability remains distinct, and200%text keeps both
actions reachable/disabled while busy.

Actual native previews use the debug fixture with example data, a temporary
separate Application/package and no sync-native producers. They prove rendering
and component behavior, not the installed companion's startup, approval/pairing
or a signed native package. Production build/package gates remain intact.

The approval-required card shows both full-identity comparison fingerprints.
The Pi platform now has a separate local approval/revocation page in source;
the installed older Pi still requires a verified upgrade. Three native component
tests cover fingerprint visibility and hiding after approval, connection loss,
and large-text actions. The paired keys, not displayed fingerprints, remain the
authority boundary. The preview is synthetic and never approves a real device.

Privileged settings, attachment-bound once/remember import confirmations and
real phone/Pi approval/roaming/control acceptance remain work.
The general photo-link setup still supports existing paired destinations; the
new selected controller reference does not silently migrate or retarget them.
Do not claim completed headless control or a gold ROM from this dashboard.
