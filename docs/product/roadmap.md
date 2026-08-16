# Roadmap

## 0.1 Foundation

Contracts, threat model, protocol, Rust boundaries, native project roots, Docker/Unraid packaging, deterministic commands, and public CI.

## 0.2 Tier 1 vertical slice

The shared Rust implementation now has real pairing, encrypted backup, explicit replica copies, verified multi-source restore, device settings transfer, crash recovery, and local multi-node recovery drills. Completion still requires the native macOS/Android and packaged Docker/Unraid surfaces to drive these contracts end to end.

## 0.3 Tier 1 production readiness

Performance bounds, corruption repair, migrations, crash recovery, accessibility, container hardening, release artifacts, SBOMs, and independent zero-finding audits.

## Tier 2 iOS track

iOS develops against the same versioned service contracts with selected-directory access, honest background behavior, and native tests. Its milestones are published independently and do not block Tier 1 release decisions.

## Later

Only evidence-backed improvements within the locked backup/restore scope. Windows, automatic replica placement, and required hosted services remain excluded.
