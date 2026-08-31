# ADR 0005: Tiered platform readiness

Status: accepted, amended 2026-08-21.

**Amendment 2026-08-21.** The Tier 2 designation for iOS is withdrawn. Supported
platforms are now Unraid, macOS, and Android only; iOS is out of scope for now
and is no longer described as a supported client at any tier. The `iOS Tier 2`
CI job still runs as an explicitly non-release, informational lane and is
deliberately excluded from every release workflow's required-check list, which
`scripts/check-required-checks.sh` verifies. The historical decision below is
retained for the record and is superseded by this amendment.

---

macOS, Android, Docker, and Unraid are Tier 1 and collectively define production readiness. The historical iOS Tier 2 proposal below is not current policy: iOS is unreleased and unsupported, and its build, signing, background, or UX status cannot delay a Tier 1 release.

Shared Rust, protocol, security, or data-integrity defects block all affected supported platforms. CI may label the iOS diagnostics separately, but release evidence excludes that informational lane from readiness.
