# Contributing

Covalent develops directly on `main` during the foundation phase. Keep changes
atomic, tested, and scoped to the current one-way file product: pairing, folder
authorization, rclone transfers, shared link settings, scheduling, status, native
clients, the Docker web console, and LAN/Tailnet discovery.

Use the existing bundled rclone engine rather than adding a transfer algorithm
or alternative backend. Preserve existing files, identities, and access to
legacy encrypted backups; do not silently convert legacy behavior. See the
[product requirements](docs/product/requirements.md) and
[architecture](docs/architecture/overview.md) for the current contract.

## Local checks

```sh
./scripts/bootstrap.sh core
./scripts/check.sh core
```

Those commands need only the Rust toolchain. Before a platform-specific check,
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
