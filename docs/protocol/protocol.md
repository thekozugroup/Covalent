# Covalent protocol 1

Status: implemented v1 contract. Additive local-client fields require defaults; incompatible wire changes require a negotiated protocol version.

## Principles

Every object carries `protocolVersion`, canonical encoding rules, bounded sizes, and an authenticated context. Unknown critical fields fail closed. Peers negotiate the highest mutually supported non-downgraded version.

## Identity and pairing

1. The inviting device creates a signed, expiring, single-use invitation containing its device ID, identity public key, protocol range, rendezvous hints, and random bearer secret plus commitment.
2. The responder signs a role-bound acceptance. Both devices derive and display the same secret-bound short authentication string.
3. Each user explicitly confirms that string on their own device. The invitation is consumed durably only after both signed confirmations verify.
4. Each device stores the exact confirmed peer roles and a signed roster epoch. Revocation creates a persistent tombstone and immediately removes active provider access.
5. Subsequent QUIC requests pin the transferred TLS certificate and bind request/response digests, protocol version, freshness timestamp, nonce, certificate fingerprint, and peer identity signatures.

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

Chunks are streamed into the durable local store under a resumable checkpoint. Selected providers acknowledge durable ciphertext by opaque locator. The manifest records actual provider acknowledgements separately from requested replica intent. A signed manifest becomes visible only through an authenticated recovery transaction after local objects are durable; degraded replication is explicit and never repaired to an unselected peer. Garbage collection defers while any resumable job checkpoint exists.

## Restore

The client loads an encrypted manifest it is authorized to decrypt, validates its signature and epoch, then schedules each chunk across connected authorized providers that acknowledged that locator plus the local store. First valid response wins; corrupt responses are rejected and attributed. Plaintext digest verification occurs after authenticated decryption.

Restore requests contain an immutable authorized-root token plus protocol relative paths. Absolute paths and traversal are invalid messages. Conflict policy is one of `fail`, `skip`, `replace`, or `rename`; preview and execution share a signed plan digest.

## Settings transfer

Normal export contains only schema version, device name, LAN discovery preference, and remembered backup descriptors. It never contains device identity private keys, backup content keys, pairing secrets, access bookmarks, Android URI grants, or provider credentials. Import rejects unexpected secret-like fields and requires confirmation before replacing local settings.

## Limits

Implementations enforce maximum frame, provider record, invitation, roster, manifest-entry, path, pending-job, and concurrent-stream sizes before allocation. Provider operations are request-rate limited per remembered peer. Local API bodies are capped at 2 MiB. Peer errors are stable, non-secret categories; detailed local filesystem errors stay on the local control surface.

## Chunk cryptography

Each plaintext chunk is BLAKE3 hashed. HKDF-SHA-256 derives separate encryption, nonce, and opaque-locator material from the backup ID, key epoch, plaintext digest, and length. XChaCha20-Poly1305 encryption is deterministic only for identical content in the same backup epoch, enabling exact deduplication; unique chunk contexts use independent derived keys and nonces. A provider sees keyed locators, lengths, ciphertext, and access timing, but not paths or plaintext digests.
