# Contributing

Current work is on `main`, which contains the released rclone implementation.
Start from `main`. Keep changes
atomic, tested, and scoped to the current one-way file product: pairing, folder
authorization, rclone transfers, shared link settings, scheduling, status, native
clients, the Docker web console, and LAN/Tailnet discovery.

Use the existing bundled rclone engine rather than adding a transfer algorithm
or alternative backend. Preserve existing files, identities, and access to
legacy encrypted backups; do not silently convert legacy behavior. See the
[product requirements](https://github.com/thekozugroup/Covalent/blob/9088b50416f686d533d75e27b8d89804dab51139/docs/product/requirements.md) and
[architecture](https://github.com/thekozugroup/Covalent/blob/9088b50416f686d533d75e27b8d89804dab51139/docs/architecture/overview.md) for the current contract.

## Local checks

```sh
./scripts/bootstrap.sh core
./scripts/check.sh core
```

Those commands need only the Rust toolchain. Build output can live outside the
checkout: set `CARGO_TARGET_DIR` for the whole `check.sh` run and every step,
including the artifact size check, reads it. Before a platform-specific check,
run the matching read-only prerequisite report:

```sh
./scripts/setup-doctor.sh docker
./scripts/setup-doctor.sh macos
./scripts/setup-doctor.sh android
```

Run targeted checks while iterating:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
swift test --package-path apps/apple
./scripts/check-android.sh
```

Commits use concise conventional subjects. Configure repository-local authorship as:

```sh
git config user.name thekozugroup
git config user.email thekozugroup@gmail.com
```

Do not add generated attribution or co-author trailers. Never commit secrets, identity material, `.a5c` run state, build output, or local signing configuration.

Tier 1 regressions on macOS, Android, Docker, or Unraid block release. iOS and Windows are unsupported and excluded from builds and release gates.
