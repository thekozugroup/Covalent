# Dependency and vulnerability policy

Covalent release candidates must have reproducible dependency inventories for Rust, Android, Apple, and the container. CI rejects known Rust advisories, yanked crates, wildcard dependencies, unknown registries or Git sources, and dependencies without an approved permissive license path. Pull requests also fail on high-severity dependency changes and denied copyleft licenses. Android packages retain upstream `META-INF` license resources; `THIRD_PARTY_NOTICES.md` and the Apple inventory preserve reviewed Swift notices. Release artifacts include source and platform SBOMs rather than relying only on an image SBOM.

High or critical vulnerabilities block release. A temporary exception must name the advisory, affected package and path, actual exposure, compensating control, owner, and expiration date in a reviewed repository change. An expired or undocumented exception is a failure. Generated SBOMs and reports are attached to immutable workflow runs or releases with the exact source commit; ignored local files are not release evidence.

Run locally:

```sh
cargo audit --deny warnings
cargo deny check advisories bans licenses sources
./apps/android/gradlew -p apps/android :app:generateAndroidSbom
./scripts/apple-dependency-inventory.sh
```

Credentialed package publication, signing, notarization, image promotion, and live Unraid drills remain separate release gates. CI must report an unavailable gate as blocked, never silently skip it.
