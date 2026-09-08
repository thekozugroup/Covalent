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

## Two-way folder synchronization

Two-way folder synchronization is under active development and is not yet
released. The intended journey is simple: choose a folder, choose a paired
device, then accept the invitation on that device. [ADR 0007](../adr/0007-maintained-folder-sync-engine.md)
selects a pinned maintained Syncthing worker, controlled by Covalent's native
folder consent, verified package and durable lifecycle boundaries. The
experimental custom protocol in ADR 0006 is inactive.

Actual production runtime tests already cover pairing, signed folder consent,
bidirectional transfer, pause/resume, restart and file-preserving revocation.
Completion still requires the initial full-scan barrier, complete native and
server setup, permission repair, lifecycle clarity, packaging acceptance,
security and notices, and measured performance. Track these gates in
[Completion progress](../release/completion-progress.md).

Backup and restore retain their own identity and supported scope. Android's
SAF backup path remains separate from the personal-build raw-folder sync
permission. Windows, iOS, automatic replica placement and required hosted
services remain excluded. Docker is the accepted Unraid validation target
while Atlas is offline.
