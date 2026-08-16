# Security

## Reporting

Do not open a public issue for a suspected vulnerability. Email `thekozugroup@gmail.com` with affected version, impact, reproduction steps, and any proposed mitigation. Expect acknowledgement within seven days. Never include live private keys or unredacted user data.

## Supported code

Security fixes target the latest release and current `main`. This pre-1.0 foundation has not completed an external cryptographic audit and must not be described as audited.

## Security invariants

- Core workflows require no hosted account or central service.
- Pairing requires explicit confirmation of a short authentication string on both devices.
- Private identity keys never leave the device through normal settings export.
- Backup manifests and chunks are authenticated and encrypted before a storage provider receives them.
- A provider is used for an extra copy only after explicit user selection.
- Restore paths are normalized relative paths and remain beneath the authorized root. Absolute paths, parent traversal, and symlink traversal fail closed.
- LAN discovery can be disabled. Tailscale connectivity does not replace Covalent authentication or pairing.
- Revoked devices cannot receive new credentials or manifests; key rotation and re-replication are required after compromise.

The detailed assumptions and abuse cases are in [docs/security/threat-model.md](docs/security/threat-model.md).
