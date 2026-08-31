# Roadmap

## 0.1 Foundation

Contracts, threat model, protocol, Rust boundaries, native project roots, Docker/Unraid packaging, deterministic commands, and public CI.

## 0.2 Tier 1 vertical slice

The shared Rust implementation now has real pairing, encrypted backup, explicit replica copies, verified multi-source restore, device settings transfer, crash recovery, and local multi-node recovery drills. Completion still requires the native macOS/Android and packaged Docker/Unraid surfaces to drive these contracts end to end.

## 0.3 Tier 1 production readiness

Performance bounds, corruption repair, migrations, crash recovery, accessibility, container hardening, release artifacts, SBOMs, and independent zero-finding audits.

## iOS: out of scope for now

iOS is not a supported platform and is not on this roadmap. The `CovalentIOS`
target still exists and still compiles in the informational iOS CI lane, but it is
not published, not installable, not gated on, and not being invested in. That
lane is deliberately excluded from every release workflow's required checks.
There is no committed milestone for making iOS supported.

## Later

Only evidence-backed improvements within the locked backup/restore scope, for the supported platforms: Unraid, macOS, and Android. Windows and iOS clients, automatic replica placement, and required hosted services remain excluded.
