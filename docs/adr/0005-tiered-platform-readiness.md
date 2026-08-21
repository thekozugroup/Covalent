# ADR 0005: Tiered platform readiness

Status: accepted, amended 2026-08-21.

**Amendment 2026-08-21.** The Tier 2 designation for iOS is withdrawn. Supported
platforms are now Unraid, macOS, and Android only; iOS is out of scope for now
and is no longer described as a supported client at any tier. The `iOS Tier 2`
CI job still runs and is deliberately excluded from every release workflow's
required-check list, which `scripts/check-required-checks.sh` verifies. The
original decision is recorded below unchanged.

---

macOS, Android, Docker, and Unraid are Tier 1 and collectively define production readiness. iOS is Tier 2: it remains a native supported client on the shared contracts, but an iOS-only build, signing, background, or UX gap cannot delay a Tier 1 release.

Shared Rust, protocol, security, or data-integrity defects block all affected tiers. CI and release evidence label Tier 2 results separately instead of hiding them.
