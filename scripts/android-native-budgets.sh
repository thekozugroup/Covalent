# shellcheck shell=sh
# shellcheck disable=SC2034  # every value here is consumed by the scripts that source this file
#
# Size budgets for the Android native artefacts, in one place.
#
# scripts/build-android-jni.sh checks the freshly linked .so, and
# scripts/check-android-native-package.sh checks the copy inside the signed APK
# and AAB. Those are two gates on the same bytes. They used to carry two
# independent hardcoded copies of "2097152", which is how gates drift apart.
# Source this file instead; there is one number and one derivation.
#
# ---------------------------------------------------------------------------
# How these numbers were derived (2026-08-21)
# ---------------------------------------------------------------------------
#
# The previous 2 MiB per-ABI budget was not a measurement. It was calibrated
# against a library that did not contain the product: Android's
# keyProtectionAvailable() was hardcoded false, nothing reached the embedded
# node, and the optimiser dead-stripped the entire node runtime out of the
# shipped object. That artefact was 747,864 bytes on arm64-v8a and contained
# zero quinn, rustls and axum symbols. 2 MiB was a budget for an empty file.
#
# A working library links the whole node runtime: QUIC (quinn), TLS (rustls +
# ring), an HTTP server (axum/hyper), mDNS discovery, and the archive/transfer
# path. Measured symbol bytes by crate on the arm64-v8a object, largest first:
#
#   core 1,083 KB   covalent_node 632 KB   covalent_core 559 KB
#   serde_core 457 KB   quinn_proto 369 KB   axum 366 KB   alloc 365 KB
#   rustls 332 KB   regex_automata 307 KB   mdns_sd 247 KB
#   serde_json 242 KB   tokio 239 KB   std 188 KB   ring (C+Rust) 175 KB
#
# That is a genuinely large but not wasteful binary. Checks made before
# accepting it:
#   - rustls is pinned to default-features = false, features = ["ring", "std"].
#     Only `ring` is linked; `aws-lc-rs` is absent, so no second TLS backend.
#   - `cargo tree -d` shows no duplicate build of any heavyweight crate. The
#     duplicates are chacha20, getrandom, rand_core and syn (syn is build-time
#     only); together they are noise at this scale.
#   - `--gc-sections` and `--strip-all` are already applied at link time.
#     `-Wl,--icf=safe` saves 0 bytes on top of fat LTO; `--icf=all` saves
#     126,016 bytes (1.5%) but folds functions with identical bodies, which can
#     break Rust function-pointer identity. Not worth 1.5%.
#
# Two real, lossless reductions have been taken since:
#
#   1. [profile.release] `lto = "thin"` -> `lto = "fat"` in the workspace
#      Cargo.toml: arm64-v8a 9,260,232 -> 8,369,248 (-9.6%) and x86_64
#      10,952,432 -> 9,942,680 (-9.2%).
#   2. Feature-gating tracing-subscriber's `env-filter` (and the CLI-only
#      dependencies behind it) off the Android path: arm64-v8a 8,369,248 ->
#      8,222,272 (-146,976, -1.8%) and x86_64 9,942,680 -> 9,790,600
#      (-152,080, -1.5%).
#
# The numbers below are measured *after* both reductions.
#
#   toolchain: rustc 1.97.1, Android NDK 27.1.12297006, cargo-ndk 4.1.2
#   arm64-v8a  8,222,272 bytes
#   x86_64     9,790,600 bytes   <- worst ABI, the budget is derived from this
#
# Ceiling = worst measured ABI + 12%, rounded up to the next 64 KiB. It was
# derived when the worst ABI was 9,942,680:
#   9,942,680 * 1.12 = 11,135,802 -> ceil to 64 KiB -> 170 * 65536 = 11,141,120
#
# The env-filter trim moved the worst ABI down to 9,790,600 and the ceiling was
# deliberately left where it is, so the margin today is 11,141,120 - 9,790,600 =
# 1,350,520 bytes, or 13.8%. Re-deriving the ceiling downwards after every
# saving would ratchet it onto whatever the last build happened to measure and
# make an ordinary, deliberate increase fail; 12% is the minimum margin this
# gate is willing to leave, not a figure to re-round to. A saving widens the
# margin, a regression still has to be measured and argued for.
#
# Why 12% and not a rounder, more comfortable number: the thin -> fat LTO swing
# measured on this exact artefact was 9.2-9.6%. That is a real, observed bound
# on how far a single change in codegen policy moves this binary, and a
# toolchain upgrade that shifts inlining behaviour is the realistic drift. 12%
# covers that measured swing with a small margin and nothing more. It leaves
# 1.35 MB of headroom on the worst ABI, which is under one release's worth of
# ordinary dependency growth and will require a fresh measurement rather than
# silently absorbing a regression.
#
# Floor: a maximum-size gate is structurally incapable of catching the bug that
# started all this. The dead-stripped 747,864-byte library passes *any* ceiling.
# So there is also a floor, at 4 MiB: 5.6x above the broken artefact and 1.96x
# below the smallest real one, which is wide enough that no legitimate build
# lands in between by accident.
COVALENT_JNI_MAX_BYTES=11141120
COVALENT_JNI_MIN_BYTES=4194304

# Official Syncthing v2.1.3 Android helpers measured by the isolated API-37
# proof at commit 024afda5 (run 34237780913): arm64-v8a 30,787,168 bytes and
# x86_64 32,646,984 bytes. The common ceiling is the 32,646,984-byte worst ABI
# plus 12%, rounded up to 64 KiB. The 16 MiB floor catches a missing or stubbed
# engine while leaving substantial room for legitimate linker variation.
COVALENT_SYNC_ENGINE_MAX_BYTES=36569088
COVALENT_SYNC_ENGINE_MIN_BYTES=16777216

# The same proof measured the reviewed guardian at 14,912 bytes (arm64) and
# 14,616 bytes (x86_64). Its ceiling is the 14,912-byte worst ABI plus 12%,
# rounded up to 4 KiB. It remains separately bounded because a tiny guardian
# must not inherit the worker's much larger allowance.
COVALENT_ENGINE_GUARDIAN_MAX_BYTES=20480
COVALENT_ENGINE_GUARDIAN_MIN_BYTES=8192

# ---------------------------------------------------------------------------
# Whole-package budget
# ---------------------------------------------------------------------------
#
# There are no `splits { abi { } }` in apps/android/app/build.gradle.kts, so the
# universal APK carries both ABIs. The executable helper requires legacy native
# packaging so PackageManager extracts it into the installer-owned
# nativeLibraryDir. Package compression may make the APK smaller, but the safe
# release ceiling is still derived from every ELF's uncompressed upper bound.
#
# The AAB has no `bundle { abi { enableSplit = false } }` override, so ABI
# splitting is on by default: a Play install delivers one JNI library, worker,
# and guardian rather than both ABI sets. The universal APK on the GitHub
# release remains the both-ABI case.
#
# The app-only remainder was measured on the release build (2026-08-21,
# isMinifyEnabled + isShrinkResources), before adding the folder engine:
#   universal APK  20,040,528 bytes  <- worst case, both ABIs
#   AAB            12,745,116 bytes
#
# Of that APK, 18,311,928 bytes (91.4%) was the two JNI libraries. Only the
# remaining 1,728,600 bytes were dex, resources, manifest and signing blocks.
# The package budget now derives from all six bounded ELF files plus that
# independently measured app remainder rather than guessing a new total:
#
#   2 * COVALENT_JNI_MAX_BYTES
# + 2 * COVALENT_SYNC_ENGINE_MAX_BYTES
# + 2 * COVALENT_ENGINE_GUARDIAN_MAX_BYTES
# + 4 MiB for dex/resources/manifests/signing
# = 99,655,680 bytes
#
# The 4 MiB non-native allowance is 2.4x the 1,728,600 bytes of non-native
# content measured today, which is room for real feature growth in the Compose
# app without being so loose that a regression hides in it.
#
# Each helper still has its own measured floor, ceiling, packaged size, and
# manifest digest gate, so compression cannot conceal a stub or substituted
# executable inside this broader whole-package ceiling.
COVALENT_ANDROID_PACKAGE_MAX_BYTES=$((
  2 * COVALENT_JNI_MAX_BYTES +
  2 * COVALENT_SYNC_ENGINE_MAX_BYTES +
  2 * COVALENT_ENGINE_GUARDIAN_MAX_BYTES +
  4 * 1024 * 1024
))
