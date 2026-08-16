# ADR 0001: Rust engine with native clients

Status: accepted.

The data plane and safety-critical behavior live in memory-safe Rust. macOS and iOS use native SwiftUI; Android uses native Kotlin and Jetpack Compose; Docker/Unraid use the daemon and a tiny embedded web console.

This prevents divergent backup/restore rules while preserving native access, accessibility, and platform conventions. The FFI/local API boundary is versioned and narrower than internal engine types. Web technology is not used to replace native Tier 1 clients.
