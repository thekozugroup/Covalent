# Contributing

Covalent develops directly on `main` during the foundation phase. Keep changes atomic, tested, and scoped to pairing, backup, restore, device settings, explicit replicas, verified storage, or LAN/Tailnet discovery.

## Local checks

```sh
./scripts/bootstrap.sh
./scripts/check.sh
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

Tier 1 regressions on macOS, Android, Docker, or Unraid block release. iOS remains supported as Tier 2 and is tracked separately so it does not delay a valid Tier 1 release.
