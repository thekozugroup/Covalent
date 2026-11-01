# Signed appliance import choices

`covalent-core::appliance_import` shares one canonical signing contract between
the phone's identity owner and the Pi's independent privileged verifier. The
transport, durable phone outbox and companion review use this contract; a host
still needs its independent owner-approved root mirror and current preview.

`ImportApproval` signs full phone/Pi identities, root/native approval generations,
action UUID, service session and positive signed-64-bit sequence, opaque source
hash, source kind, once/remember/off choice, attachment/preview UUIDs and retain
originals. Once/remember require both UUIDs. Off requires explicit nulls. Source
paths and deletion modes are absent and rejected. Full raw keys
are authoritative; the short fingerprint alone never is.

`sign_import_approval` uses the actual phone `DeviceIdentity` and existing
`covalent/signature/v1` framing with the separate
`covalent/appliance-import/request/v1` domain. Payloads are capped at2048 bytes.
Version1 volume choices retain their exact original canonical bytes and domains.
Camera choices require version2 and the separate
`covalent/appliance-import/approval/v2` domain; their withdrawal uses
`covalent/appliance-import/withdraw/v2`. Version1 camera and version2 volume
combinations refuse. A signature for another version, kind or intent cannot
authorize the choice. The receiver
requires an exact round trip, rejecting alternate order/whitespace/UUID spelling,
missing nullable fields, duplicate/unknown fields and unsupported versions/types.
Unpadded base64url signatures are canonical64-byte Ed25519 values. Verification
uses the existing vetted dalek implementation's strict verification and rejects
weak keys; ordinary identity signature behavior is unchanged.

The expected `ImportApprovalBond` MUST come from the privileged caller's own
current owner-approved private record, never the request or ordinary pairing.
Verify the exact signed text against that bond while serializing revocation with
admission. The privileged source service must independently recheck the current
attachment/completed preview, map the opaque source through its own inventory,
and admit the action tuple with its durable ledger. A valid signature is not a
freshness check, replay receipt, import completion, cloud backup or deletion grant.
An interrupted acknowledgement must use the exact action's readback; never mint
a new action or repeat a mutation to guess whether it succeeded.
