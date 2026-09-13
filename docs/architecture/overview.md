# Architecture

## One-way links

Native macOS and Android clients and the Docker web console use the same node API.
Covalent owns pairing, folder authorization, shared link settings, scheduling,
status, and the lifecycle of its bundled rclone transfer worker.
The source commits setting revisions and distributes them to linked destinations.
Each destination writes ordinary files into its authorized folder; collection
links use separate child folders. Manual and scheduled links stop their transfer
worker between runs. Android reports local Wi-Fi and charging conditions to the
node; those observations expire and are not shared settings.

The rclone integration uses paired, read-only SFTP source access and scoped
destination copy operations. Android keeps its folder grant on-device and
streams through an authenticated loopback WebDAV adapter. It does not stage an
entire folder. See [ADR 0008](../adr/0008-rclone-one-way-links.md) for the engine
decision and the completion ledger for integration readiness.

The [product requirements](../product/requirements.md) define deletion behavior,
platform scope, and the completion gates. The older archive APIs described below
remain for access to existing data; they do not define the current product scope.

## Legacy archive components

The Rust workspace owns protocol types, identity, pairing, backup traversal, chunking, encryption, durable storage, verification, restore safety, discovery, and peer transport. Native applications own platform access grants and platform UI. Docker and Unraid run the daemon with the same engine and a small embedded console.

## Components

- `covalent-protocol`: canonical versioned messages, manifests, settings exports, errors, and compatibility fixtures. It has no platform UI dependencies.
- `covalent-core`: filesystem safety, encrypted storage, manifest lifecycle, replica intent, verification, restore planning, and job state.
- `covalent-node`: long-running peer and local API service with graceful shutdown and embedded assets.
- `covalent-cli`: operator workflows and deterministic diagnostics against the same facade/API.

## Data flow

1. A platform obtains durable access to a selected source. Unsandboxed filesystem clients pass an authorized local path. Android keeps SAF `content://` identifiers on-device and Apple keeps security-scoped URLs in-app; both stream a validated ZIP to a daemon-owned private staging path.
2. The engine anchors a source directory handle, traverses without following symlinks, streams file content into bounded chunks, hashes plaintext, encrypts each chunk, and checkpoints completed entries.
3. A signed encrypted manifest commits through a restart-recoverable transaction only after required local data is durable. Snapshot IDs are immutable and monotonic per backup.
4. The user selects provider device IDs. The scheduler sends only to those providers and records acknowledgements; it never fills a desired count by choosing peers.
5. Restore fetches verified ciphertext concurrently from the local store and acknowledged connected providers, decrypts and re-verifies it, then stages writes beneath a handle-anchored authorized root. Files use temporary write, metadata application, fsync, and atomic rename; completed entries are checkpointed for resume.

## Discovery and transport

LAN discovery is mDNS-based and separately disableable from inbound service operation. When disabled, no mDNS daemon is created and browsing returns no LAN results. Tailnet candidates come from the bounded Tailscale LocalAPI socket contract or explicit remembered addresses. Tailscale supplies routing only; Covalent still requires its own confirmed identity roles and pinned certificate. Data uses TLS 1.3 QUIC with signed request/response binding, strict transport-v2 ALPN/range negotiation, freshness/replay windows, byte-aware per-peer pacing, deadlines, bounded frames, and bounded global/stream work. HTTP/archive contracts remain protocol v1 and are versioned independently from QUIC framing.

## Durable state

One process owns a node state directory through an exclusive lock. Identity, backup keys, pairing state, provider pins, and the local API token are owner-only files under owner-only directories on Unix. Chunk objects are content-addressed by keyed opaque locators; snapshot metadata is immutable. Active checkpoints conservatively block garbage collection. Startup authenticates and completes any backup transaction journal before serving requests.

## Platform boundaries

- macOS Tier 1: bundled app-owned loopback node, private readiness/token files, inheritance-only helper entitlements, security-scoped and coordinated archive streaming without forwarding PowerBox paths, menus, keyboard, and supported background work.
- Android Tier 1: API 37 targeting, persisted SAF tree grants, file-descriptor archive streaming without forwarding content URIs, exact create-only restore into an empty tree, opt-in local-network permission for LAN discovery, foreground/resumable work, Compose Material 3, and a restrained floating action toolbar.
- Docker/Unraid Tier 1: explicit read-only source mounts, durable config/data, explicit writable restore roots, rootless runtime, and clear network mode tradeoffs.


## Readiness isolation

Shared engine and contract failures block every affected Tier 1 platform, and any supported-platform failure blocks release. iOS is not a supported platform and has no app target or CI lane.
