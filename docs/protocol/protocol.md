# Covalent protocol 0

Status: design contract for implementation; not wire-stable.

## Principles

Every object carries `protocolVersion`, canonical encoding rules, bounded sizes, and an authenticated context. Unknown critical fields fail closed. Peers negotiate the highest mutually supported non-downgraded version.

## Identity and pairing

1. The inviting device creates an expiring single-use invitation containing its device ID, identity public key, protocol range, rendezvous hints, and random invitation secret commitment.
2. Peers establish encrypted QUIC and bind the handshake transcript to both identity keys and invitation.
3. Both derive and display the same short authentication string. Nothing is trusted until each user explicitly confirms it.
4. Each device stores a signed peer grant defining allowed roles. Rejection, timeout, mismatch, or replay destroys the pending invitation.

Discovery advertisements contain only protocol range, ephemeral service identifier, port, and capability bits. Backup names, paths, device identity keys, and user data are not advertised. LAN advertisements stop when LAN discovery is disabled. Tailnet reachability is only a candidate transport path.

## Core objects

- `DeviceId`: stable UUID associated with an Ed25519 public identity.
- `BackupId`: stable UUID for a logical backup.
- `SnapshotId`: monotonic ULID-like identifier scoped to a backup.
- `ChunkId`: BLAKE3 plaintext digest stored only inside encrypted metadata; providers address opaque keyed locators.
- `RelativePath`: non-empty normalized components, never absolute, `.` or `..`.
- `ReplicaIntent`: explicit set of provider device IDs chosen by the user. No desired-count field may trigger automatic selection.
- `ManifestEnvelope`: version, backup/epoch identifier, cipher suite, nonce, encrypted canonical manifest, signer ID, and signature.
- `Roster`: monotonic epoch, authorized devices and roles, revocations, previous digest, and authorized signatures.

## Backup commit

Chunks are staged and verified locally. Selected providers acknowledge durable ciphertext by opaque locator. The manifest records actual provider acknowledgements separately from requested replica intent. A signed manifest becomes visible only after local durable commit; degraded replication is explicit and never repaired to an unselected peer.

## Restore

The client requests an encrypted manifest it is authorized to decrypt, validates its signature and epoch, then schedules each chunk across all connected authorized providers that advertise it. First valid response wins; corrupt responses are rejected and attributed. Plaintext digest verification occurs after authenticated decryption.

Restore requests contain an immutable authorized-root token plus protocol relative paths. Absolute paths and traversal are invalid messages. Conflict policy is one of `fail`, `skip`, `replace`, or `rename`; preview and execution share a signed plan digest.

## Settings transfer

Normal export contains only schema version, device name, LAN discovery preference, and remembered backup descriptors. It never contains device identity private keys, backup content keys, pairing secrets, access bookmarks, Android URI grants, or provider credentials. Import rejects unexpected secret-like fields and requires confirmation before replacing local settings.

## Limits

Implementations enforce maximum frame, invitation, roster, manifest-entry, path, and concurrent-stream sizes before allocation. Errors are stable codes with safe user messages; internal filesystem paths and cryptographic material are never sent to peers.
