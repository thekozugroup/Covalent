# Security

## Reporting

Do not open a public issue for a suspected vulnerability. Email `thekozugroup@gmail.com` with affected version, impact, reproduction steps, and any proposed mitigation. Expect acknowledgement within seven days. Never include live private keys or unredacted user data.

## Supported code

Security fixes target the latest release and current `main`. This pre-1.0 foundation has not completed an external cryptographic audit and must not be described as audited.

## One-way file links

- Core workflows require no hosted account or central service.
- Pairing requires explicit confirmation of a short authentication string on both devices. Pairing alone does not share a folder.
- Each link requires selected source and destination folders and receiver consent. Transfers use the bundled rclone engine and paired, read-only SFTP source access with host-key verification.
- Android retains its folder grant on-device and streams through an authenticated loopback WebDAV adapter. It does not grant broad all-files access.
- Destination copies are ordinary files, not Covalent-encrypted archives. Encrypted network transport does not provide encryption at rest for these copies; protect the destination device and storage accordingly.
- Missing mounts, lost folder permissions, and failed scans are not treated as source deletions. Source-deletion propagation and restoration of destination deletions are both off by default.
- Private identity keys never leave the device through normal settings export. Docker/Unraid needs a separate owner-only key file; native apps use platform key storage.
- Network management requires HTTPS and an access token. LAN discovery can be disabled. Tailscale connectivity does not replace Covalent authentication or pairing.
- Revocation rejects new peer operations and refreshes transfer access. Revocation cannot erase files a device already received.

One-way transfers do not provide historical versions or a complete recovery
plan. Keep independent backups of important files. Copy live databases or
appdata only from an application-consistent source.

## Legacy encrypted backups

The older backup workflow remains available for existing data. Its manifests
and chunks are authenticated and encrypted before a selected storage provider
receives them. Providers are selected explicitly. Restore paths remain beneath
the authorized root; absolute paths, parent traversal, and symlink traversal
fail closed.

If a legacy backup master key was stolen, incrementing its epoch is insufficient;
create a new backup ID/master key and fresh replicas. These archive protections
do not describe ordinary files copied by current folder links.

The detailed assumptions and abuse cases are in
[current threat model](https://github.com/thekozugroup/Covalent/blob/9088b50416f686d533d75e27b8d89804dab51139/docs/security/threat-model.md).
