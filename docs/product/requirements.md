# Product requirements

Status: foundation contract, 2026-08-15.

## Product promise

Covalent makes private distributed backup understandable: pair devices, choose sources, choose exactly which devices hold extra copies, verify continuously, and restore safely to a directory the user selects.

Core workflows work without a hosted account, cloud coordinator, or subscription.

## Priority policy

| Priority | Platforms | Definition of ready |
| --- | --- | --- |
| Tier 1 | macOS, Android, Docker, Unraid | All required functional, safety, accessibility, packaging, and disaster-restore checks pass. |
| Tier 2 | iOS | Supported with native UX and selected-directory access, but its incomplete or unavailable validation cannot delay Tier 1 readiness. |

The architecture shares protocol and Rust behavior across tiers while validation and release reporting keep iOS evidence separate.

## Required journeys

### PR-01 Name and pair a device

The user names a device, discovers a candidate on a permitted LAN or Tailnet, compares a human-readable authentication string on both devices, and explicitly accepts. LAN discovery has a persistent off switch. Manual address or remembered-peer connection remains available.

### PR-02 Create a backup

The user selects one or more authorized source directories, reviews exclusions and access limits, chooses zero or more specific authorized provider devices for extra copies, and starts a resumable backup. Covalent never chooses replica devices automatically.

### PR-03 Store and verify

The engine streams, chunks, authenticates, encrypts, signs, and durably records content. Every connected authorized provider that holds a requested chunk may serve or verify it. Corruption is rejected and reported; repair requires an intact authorized source.

### PR-04 Restore

The user selects a backup, an authorized target root, relative paths, and a conflict policy. Covalent previews the result, then restores only normalized relative paths beneath that root with symlink and traversal defenses. Writes are staged, synchronized, and atomically committed where supported.

### PR-05 Manage replicas

Availability is visible per backup and provider. Adding or removing a replica is always an explicit user action with impact shown before deletion. Offline devices are degraded, not silently replaced.

### PR-06 Import and export settings

The user exports a versioned file containing the device name, LAN discovery preference, and remembered backup descriptors. Private identity keys and backup content keys are excluded by default and imports reject key-like unknown fields.

### PR-07 Platform-native access

- macOS Tier 1 uses open panels, security-scoped bookmarks in sandboxed builds, and coordinated file access.
- Android Tier 1 targets the current stable Android API, uses the Storage Access Framework and persisted URI grants, and requests local-network permission only when the user enables LAN discovery.
- Docker and Unraid Tier 1 use explicit mounts. Backup sources are read-only by default; restore targets require explicit writable mounts and confirmation.
- iOS Tier 2 uses document pickers, security-scoped URLs/bookmarks, coordinated access, and resumable work within platform background limits. It never claims full-device access.

## Quality attributes

- Bounded memory while scanning and transferring large files.
- Deterministic content verification and versioned contracts.
- No plaintext content or path disclosure to an untrusted storage provider beyond unavoidable traffic metadata.
- Crash-safe metadata and recoverable interrupted transfers.
- Native accessibility: VoiceOver, TalkBack, keyboard support where applicable, scalable text, contrast, and reduced-motion behavior.
- No required secrets for build, tests, or local multi-node development.

## Locked non-goals

- Windows clients or packaging.
- Hosted user accounts or required cloud coordination.
- Automatic replica placement, background selection, or opaque availability promises.
- Arbitrary filesystem restore or restore outside an explicitly authorized root.
- Full-device iOS backup, unsupported background execution, or access to other apps' private data.
- File sync, photo management, media streaming, password management, or generic object storage.

## Acceptance boundary

The foundation is accepted when its contracts, repository layout, baseline executable safety tests, deterministic commands, CI, and public repository exist. Production acceptance additionally requires later end-to-end pairing, encrypted backup, explicit replication, source-loss restore, corruption repair, platform builds, packaging, and independent zero-finding audits.
