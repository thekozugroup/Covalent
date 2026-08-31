# Working state

Updated: 2026-08-31

## Status

The exact-current local `v0.2.0` release candidate from
`dafca8efebaf904ed886d48ba8371b0fde53af56` is validated and ready for its
milestone commit. Tier 1 is Android, Apple Silicon macOS, Docker, and Unraid.
iOS is informational only and unsupported. This is not a deployment: no
`v0.2.0` tag, replacement immutable image digest, published personal-use native
artifact, or live Atlas installation exists.

## Current release changes

- Versioned authenticated envelopes protect long-lived Rust node secrets.
  Docker/Unraid use a separate owner-only KEK, macOS uses Keychain, and Android
  uses Android Keystore. Missing or wrong protection fails closed.
- First-run ownership is a durable CLI-only HTTPS claim. Existing local-token
  deployments migrate as already claimed; native and web clients accept tokens,
  never setup codes.
- Backup/archive terminal results persist until clients acknowledge them.
  Clients preserve retry identity and receipts across reload, restart, and
  interruption; retained results apply bounded backpressure.
- Provider work uses durable bounded leases and streaming transfers. New
  snapshots use a 512 KiB average CDC chunk size; old recorded boundaries remain
  readable. Two consecutive exact 1 GiB source-loss gates completed at 26.127
  MiB/s and 25.860 MiB/s with exact provider-only restore and enforced RSS/disk
  ceilings.
- The macOS product and helper are arm64-only. Android targets API 37. Docker
  and Unraid remain Tier 1 with an immutable-digest-only template. Tailscale
  use is explicit-address routing, not container-side Tailnet discovery.
- Beginner setup now starts at one guide and ends with a verified restore.
  One-command personal builders produce a checked ad-hoc Apple Silicon app and
  a checked debug-signed Android APK. Docker setup uses separated, validated
  host paths; blocked Unraid and Atlas deployment paths are labeled honestly.

## Required remaining evidence

1. Commit and push the signed release candidate; wait for exact-commit hosted CI,
   CodeQL, and supply-chain results.
2. Create and verify the `v0.2.0` signed tag and published artifacts. Record the
   newly produced immutable container digest, then update the Unraid template
   atomically. Do not substitute a mutable tag or invent a digest.
3. Run the documented read-only Atlas preflight. A real Atlas/Unraid install,
   Tailnet pairing, upgrade, backup, and restore drill need explicit deployment
   authority and remain unperformed.
4. Publish the personal-use native scope honestly: ad-hoc Apple Silicon macOS
   and debug-signed Android APK. Apple Developer ID/notarization is excluded;
   Android production signing is deferred. Do not relabel either personal
   artifact as store-signed or notarized.

## Install and migration references

- [v0.2.0 release candidate notes](docs/release/notes/v0.2.0.md)
- [Docker](packaging/docker/README.md)
- [Unraid](docs/platform/unraid.md)
- [Atlas/Tailscale](docs/platform/atlas-tailscale.md)
- [UI tour](docs/product/ui-tour.md)
