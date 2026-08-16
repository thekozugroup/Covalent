# Architecture

## Shape

```text
Native macOS (T1) ─┐
Native Android (T1)├─ local versioned service facade ─ Rust engine
Docker/Unraid (T1) ┤                                  ├─ encrypted chunk store
Native iOS (T2) ───┘                                  ├─ signed manifest store
Embedded console ─ local HTTP API ─ node daemon       └─ QUIC peer sessions
```

The Rust workspace owns protocol types, identity, pairing, backup traversal, chunking, encryption, durable storage, verification, restore safety, discovery, and peer transport. Native applications own platform access grants and platform UI. Docker and Unraid run the daemon with the same engine and a small embedded console.

## Components

- `covalent-protocol`: canonical versioned messages, manifests, settings exports, errors, and compatibility fixtures. It has no platform UI dependencies.
- `covalent-core`: filesystem safety, encrypted storage, manifest lifecycle, replica intent, verification, restore planning, and job state.
- `covalent-node`: long-running peer and local API service with graceful shutdown and embedded assets.
- `covalent-ffi`: narrow in-process service facade for native bindings. Platform code never reaches storage internals directly.
- `covalent-cli`: operator workflows and deterministic diagnostics against the same facade/API.

## Data flow

1. A platform grants a source directory and passes an opaque grant plus selected relative roots to the engine.
2. The engine traverses without following symlinks, streams file content into bounded chunks, hashes plaintext, encrypts each chunk, and writes it to a staging area.
3. A signed encrypted manifest commits only after required local data is durable.
4. The user selects provider device IDs. The scheduler sends only to those providers and records acknowledgements; it never fills a desired count by choosing peers.
5. Restore fetches verified ciphertext concurrently from every authorized provider that advertises the exact chunk, decrypts and re-verifies it, then stages writes beneath an authorized root.

## Discovery and transport

LAN discovery is mDNS-based and separately disableable from inbound service operation. Tailnet candidates come from local interfaces, explicit addresses, MagicDNS hints, or remembered signed peer gossip. Tailscale supplies routing only; Covalent still performs its own authenticated pairing. Data uses authenticated encrypted QUIC with negotiated protocol versions and resource limits.

## Platform boundaries

- macOS Tier 1: sandbox-aware folder selection, security-scoped bookmarks, `NSFileCoordinator`, menus, keyboard, and supported background work.
- Android Tier 1: current stable API targeting, SAF tree URIs, persisted grants, opt-in local-network permission for LAN discovery, foreground/resumable work, Compose Material 3, and a restrained floating action toolbar.
- Docker/Unraid Tier 1: explicit read-only source mounts, durable config/data, explicit writable restore roots, rootless runtime, and clear network mode tradeoffs.
- iOS Tier 2: selected directories only, coordinated access, resumable jobs within iOS scheduling limits. Full-device backup is neither designed nor claimed.

## Readiness isolation

Shared engine and contract failures block every affected tier. Tier 1 platform failures block release. iOS-specific UI, signing, simulator, or background gaps remain visible Tier 2 findings but do not change Tier 1 readiness.
