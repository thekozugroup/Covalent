# Threat model

## Assets

Plaintext file contents and paths, backup/content keys, long-lived device identity keys, signed rosters, manifests, settings, restore destinations, and availability metadata.

## Trust boundaries

- A paired device is authorized, not universally trusted. Access is scoped by backup and role.
- A storage provider may be curious, compromised, stale, malicious, or unavailable.
- LAN advertisements and Tailnet reachability are untrusted hints until pairing and identity verification complete.
- Local UI clients can invoke only the local authenticated service boundary.
- The operating system controls sandbox grants, mount permissions, process isolation, and key storage.

## Required defenses

| Threat | Defense |
| --- | --- |
| Impostor pairing | Ephemeral invitation, expiry, transcript-bound short authentication string, explicit confirmation on both devices, identity fingerprint persistence. |
| Passive or active network attacker | QUIC TLS 1.3 plus application identity binding, replay-resistant nonces, protocol negotiation, signed messages, strict size/time limits. |
| Curious provider | Client-side authenticated encryption for chunks and manifests; opaque chunk locators; no private keys on provider. Traffic size/timing leakage is documented. |
| Malicious provider | Verify authenticated ciphertext, decrypt, then verify expected BLAKE3 plaintext digest before use. Quarantine bad providers and repair only from intact copies. |
| Roster injection or rollback | Monotonic signed roster epochs, signer authorization, remembered high-water mark, explicit conflict state. |
| Device compromise | OS key store, least-privilege grants, revocation, new backup epoch/content key, and re-replication. Historical ciphertext may remain readable to a stolen historical key. |
| Path traversal | Canonical authorized root, protocol-level relative path type, rejection of absolute/empty/`.`/`..` paths, component-by-component no-follow resolution. |
| Symlink or TOCTOU escape | Do not follow source symlinks by default. Restore uses directory handles and no-follow operations where available, rechecks before commit, and fails closed on links. |
| Partial or torn writes | Same-filesystem staging, file sync, metadata transaction, parent-directory sync where supported, atomic rename, startup recovery journal. |
| Unbounded resource use | Framed message limits, bounded chunk size/queues, per-peer concurrency and bandwidth limits, disk reservation, cancellation and backpressure. |
| Discovery privacy | LAN discovery off switch, minimal advertisements, no backup names or paths, rate limits, manual/Tailnet alternatives. |
| Unsafe settings import | Version/schema validation, size limits, reject unknown key-like fields, never deserialize identity keys from normal export. |
| Unraid mount mistake | Read-only sources by default; `/boot` optional and read-only for backup; restore requires a separate explicit writable target and preview. |

## Cryptographic design contract

- Device identity: Ed25519 signing key generated locally and stored using platform key protection.
- Session establishment: QUIC TLS 1.3 with the peer certificate/transcript bound to the confirmed Covalent identity.
- Data: independently authenticated XChaCha20-Poly1305 records under per-backup epoch keys with unique nonces.
- Integrity: BLAKE3 plaintext digests inside the authenticated encrypted manifest; Ed25519 signature over the versioned encrypted manifest envelope.
- Key derivation: HKDF-SHA-256 with domain-separated labels.

Algorithm agility is versioned and downgrade-protected. Implementations use reviewed libraries rather than custom primitives. Cryptographic completion requires test vectors and external review; this foundation does not claim that review.

## Restore invariant

For authorized canonical root `R` and protocol path `p`, every created object must resolve lexically and physically beneath `R`. Any absolute component, parent traversal, platform prefix, NUL, intermediate link, final link, changed directory identity, or authorization loss aborts that entry. A restore cannot widen its root after preview.

## Residual risks

Endpoint compromise can expose data while decrypted. Traffic analysis can reveal timing and approximate volume. A user can explicitly choose too few providers. Mobile operating systems can revoke access or suspend background work. Covalent reports these states and does not promise availability it cannot prove.
