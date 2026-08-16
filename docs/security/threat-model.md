# Threat model

## Assets

Plaintext file contents and paths, backup/content keys, long-lived device identity keys, signed rosters, manifests, settings, restore destinations, and availability metadata.

## Trust boundaries

- A paired device is authorized, not universally trusted. Transport access is role-scoped and device-wide; chunk reads additionally require possession of an opaque locator. Explicit replica intent is scoped per backup snapshot.
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
| Malicious provider | Verify framing, authenticated ciphertext, expected length, keyed locator, and BLAKE3 plaintext digest before use. Attribute rejected copies and repair only from an intact acknowledged copy. |
| Roster injection or rollback | Monotonic signed roster epochs, signer authorization, remembered high-water mark, explicit conflict state. |
| Device compromise | Owner-only key files, least-privilege grants, persistent revocation, and re-replication. A stolen backup master key remains valid for every epoch of that backup; recovery requires a new backup ID/master key and fresh replicas. |
| Path traversal | Canonical authorized root, protocol-level relative path type, rejection of absolute/empty/`.`/`..` paths, component-by-component no-follow resolution. |
| Symlink or TOCTOU escape | Do not follow source symlinks by default. Restore uses directory handles and no-follow operations where available, rechecks before commit, and fails closed on links. |
| Partial or torn writes | Same-filesystem staging, file sync, metadata transaction, parent-directory sync where supported, atomic rename, startup recovery journal. |
| Unbounded resource use | Framed message and local-body limits, bounded chunk size and worker count, per-peer request rate, QUIC stream/time limits, resumable cancellation, and bounded checkpoint/config state. Disk-full errors remain visible; capacity reservation is not claimed. |
| Discovery privacy | LAN discovery off switch, minimal advertisements, no backup names or paths, rate limits, manual/Tailnet alternatives, and Android local-network permission requested only after opt-in. |
| Unsafe settings import | Version/schema validation, size limits, reject unknown key-like fields, never deserialize identity keys from normal export. |
| Unraid mount mistake | Read-only sources by default; `/boot` optional and read-only for backup; restore requires a separate explicit writable target and preview. |

## Cryptographic design contract

- Device identity: Ed25519 signing key generated locally and stored in an owner-only atomic state file. Native platform key-store wrapping is a future hardening layer, not a current claim.
- Session establishment: QUIC TLS 1.3 with the peer certificate/transcript bound to the confirmed Covalent identity.
- Data: independently authenticated XChaCha20-Poly1305 records. Domain-separated HKDF derives a content-specific key and 192-bit nonce from backup ID, epoch, digest, and length; identical content within an epoch is intentionally deterministic for deduplication.
- Integrity: BLAKE3 plaintext digests inside the authenticated encrypted manifest; Ed25519 signature over the versioned encrypted manifest envelope.
- Key derivation: HKDF-SHA-256 with domain-separated labels.

Algorithm agility is versioned and downgrade-protected. Implementations use established RustCrypto, BLAKE3, Ed25519 Dalek, rustls, and Quinn libraries. Property and tamper tests cover round trips, domain separation, ciphertext modification, key mismatch, certificate pin mismatch, and roster rollback. Independent cryptographic review is still recommended before handling irreplaceable sole copies.

## Restore invariant

For authorized canonical root `R` and protocol path `p`, every created object must resolve lexically and physically beneath `R`. Any absolute component, parent traversal, platform prefix, NUL, intermediate link, final link, changed directory identity, or authorization loss aborts that entry. A restore cannot widen its root after preview.

## Residual risks

Endpoint compromise can expose data while decrypted. A stolen backup master key is not healed by incrementing its epoch. Traffic analysis reveals timing, approximate volume, repeated keyed locators within an epoch, and provider access patterns. Role grants are device-wide rather than per-backup ACLs. A user can explicitly choose too few providers. Mobile operating systems can revoke access or suspend background work. Covalent reports these states and does not promise availability it cannot prove.
