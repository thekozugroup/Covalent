# ADR 0001: Rust engine with native clients

Status: accepted.

The data plane and safety-critical behavior live in memory-safe Rust. macOS uses native SwiftUI; Android uses native Kotlin and Jetpack Compose; Docker/Unraid use the daemon and a tiny embedded web console. The Apple package also builds an iOS SwiftUI target from the same shared sources, but iOS is not a supported platform (see ADR 0005).

This prevents divergent backup/restore rules while preserving native access, accessibility, and platform conventions. The FFI/local API boundary is versioned and narrower than internal engine types. Web technology is not used to replace the native supported clients.
