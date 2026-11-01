# Bonded appliance control — read-only v1

The controller bond gives one explicitly approved paired identity permission to
read the appliance's redacted sync status. Ordinary sync pairing does not grant
control. Interlink now uses it for a retained selected-Pi dashboard; see
`docs/interlink-dashboard.md`. The private Pi platform now implements an explicit
local approval/revocation page against this API; it requires a verified appliance
upgrade. Remote settings and import confirmations remain to be implemented.
The deployed GPhoto Pi0.2.3 does not
speak this protocol.

The same owner bond now protects a second read-only operation for redacted host
storage previews and update readiness. See `docs/appliance-platform.md`. It is
optional on native hosts and unavailable on the currently installed older Pi.

## Owner approval and durable authority

The local owner reads the bond generation and eligible confirmed paired public
identities, then approves the phone's full identity with that generation. A new
controller cannot replace another controller without explicit revocation. An
exact grant retry for the already-approved identity returns the existing bond
without rotating it; a stale grant cannot reinstate a revoked controller or
replace a different controller. Stale revocation cannot remove a newer bond.

`controller-bond.json` is private mode0600 authority state under the existing
node data directory. It binds the appliance public identity, controller public
identity and a generation UUID; it contains no address or credential. Both keys
are checked against retained, confirmed pairing identities. Revocation of the
ordinary peer grant also refuses control, regardless of the saved bond.

One authority store holds an exclusive lifetime lock on the private, empty
`controller-bond.lock`. A second store cannot overwrite a stale in-memory bond.
The lock file is not unlinked while in use. Missing authority state starts
unbound. An existing malformed, oversized, aliased, foreign-owned, non-private
or special authority file disables control without replacing the record. Every
nullable field is required explicitly; a missing controller field is not an
unbound record. Write failure disables authority in the running store. Bond
state is atomically replaced and file/directory-synced before success.

Damage to this separate state does not prevent the existing node/sync runtime
from opening. It makes the local controller API unavailable and denies remote
control. Repair is a local owner operation; no remote reset/claim shortcut exists.

## Local API and encrypted transport

All HTTP operations require that device's existing private local API token and
retain loopback-only binding. The Pi's token is never given to a controller.
The phone uses its own local API, whose node signs the remote request.

| Method and local path | Effect |
| --- | --- |
| `GET /api/v1/appliance/controller` | Read the current local bond and paired identity candidates |
| `POST /api/v1/appliance/controller` | Approve `{generation, controller}` locally |
| `DELETE /api/v1/appliance/controller` | Revoke `{generation}` locally |
| `POST /api/v1/appliance/status` | Request the paired `{peerId}` appliance's status |
| `POST /api/v1/appliance/platform` | Read the paired appliance's redacted host observations |

Requests are limited to4096 bytes and reject unknown fields. Full schemas and
error responses are in `docs/api/openapi.yaml`.

Remote status uses the existing signed request/response envelope over pinned
QUIC/TLS with the dedicated `covalent-appliance/1` ALPN. The operation binds
requester and target; the existing envelope binds the certificate pin, nonce,
timestamp and payload digest. Old folder/link ALPNs cannot accept this operation.
The receiver checks a confirmed paired identity, replay admission and the
separate owner bond. It rechecks authority after reading sync status before
signing a response. The client verifies the signed peer and requires matching
appliance/controller IDs, schema1, a non-nil bond generation and the supported
false cleanup/cloud-verification capability flags.

Status reports process lifecycle, share-record count and health freshness.
The control channel stays available when folder sync is absent or cannot return
status. It then reports `syncAvailable=false`, lifecycle `unavailable`, an explicit
null share count and non-fresh health. It never turns missing observations into
zero completed work. The client rejects contradictory availability/count/freshness
combinations and a missing count field.
This transport still lives in the Covalent node: failure of the core node or
identity startup is a separate recovery boundary. An independent privileged host
broker and its authenticated control surface remain appliance integration work.
It contains no local paths, raw errors, tokens, keys or media names. Running,
fresh health and consent-record counts do not establish transfer completion or
Google Photos backup. Source cleanup and cloud-backup verification remain
unavailable. There is no remote mutating operation in this protocol version.

The bond survives route changes because it contains identities, not addresses.
Routing still uses Covalent's retained authenticated transport bindings; this
API never trusts a discovered address or tailnet member automatically. The
companion's selected-Pi state compares full paired identities and survives
unavailable routes. Automatic LAN/Tailscale recovery and physical roaming
remain separate acceptance requirements. The existing per-process replay window
is suitable only for this read-only capability; future mutating operations need
durable action identity, attachment/preview binding and interruption semantics.

## Verification boundary

Focused tests exercise actual QUIC server dispatch and client HTTP handlers,
owner-only access, immediate/durable revocation, stale confirmations, full-key
pinning, exclusive-store ownership, damaged/special/private-file handling, I/O
failure, protocol separation, replay and signed malicious response rejection.
Synthetic stopped folder services contain no shared folders and never start
their inert worker fixtures. Test sockets are drained and their ports rebound.

The full node library suite, Android26 build and physical Pi results are recorded
in the development evidence and working.md. Passing isolated tests does not
prove an installed appliance upgrade, phone UI behavior, remote administration,
actual attached-media consent, Tailscale roaming or Google Photos backup. No gold
image or complete companion release is claimed by this control API.
