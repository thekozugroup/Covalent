# ADR 0005: Tiered platform readiness

Status: accepted.

macOS, Android, Docker, and Unraid are Tier 1 and collectively define production readiness. iOS is Tier 2: it remains a native supported client on the shared contracts, but an iOS-only build, signing, background, or UX gap cannot delay a Tier 1 release.

Shared Rust, protocol, security, or data-integrity defects block all affected tiers. CI and release evidence label Tier 2 results separately instead of hiding them.
