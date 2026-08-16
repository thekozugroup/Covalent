#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

required_paths="
README.md
SECURITY.md
CONTRIBUTING.md
working.md
docs/product/requirements.md
docs/product/roadmap.md
docs/architecture/overview.md
docs/security/threat-model.md
docs/protocol/protocol.md
docs/release/validation-matrix.md
crates/covalent-core/Cargo.toml
crates/covalent-protocol/Cargo.toml
crates/covalent-node/Cargo.toml
crates/covalent-ffi/Cargo.toml
crates/covalent-cli/Cargo.toml
apps/apple/Project.yml
apps/android/app/build.gradle.kts
packaging/docker/Dockerfile
packaging/unraid/covalent.xml
.github/workflows/ci.yml
"

for path in $required_paths; do
  if [ ! -e "$path" ]; then
    echo "missing foundation path: $path" >&2
    exit 1
  fi
done

for path in README.md docs/product/requirements.md docs/product/roadmap.md docs/architecture/overview.md docs/release/validation-matrix.md; do
  grep -q "Tier 1" "$path"
  grep -q "Tier 2" "$path"
done

grep -q 'Mode="ro"' packaging/unraid/covalent.xml
grep -q 'Target="/boot-source"' packaging/unraid/covalent.xml
grep -q 'Target="/restore"' packaging/unraid/covalent.xml
grep -q 'Default="false"' packaging/unraid/covalent.xml

./scripts/validate-unraid-template.sh

if command -v jq >/dev/null 2>&1; then
  jq empty fixtures/contracts/settings-v1.json
  jq empty fixtures/contracts/pairing-invitation-v1.json
  jq empty fixtures/contracts/manifest-v1.json
fi

for script in scripts/*.sh; do
  sh -n "$script"
done

if rg -n --glob '!docs/**' --glob '!README.md' --glob '!working.md' --glob '!CONTRIBUTING.md' --glob '!scripts/validate-foundation.sh' --glob '!target/**' '(automatically (choose|select|place) replica|supports full-device iOS backup)' .; then
  echo "unsafe product claim found outside explicit documentation" >&2
  exit 1
fi

echo "foundation structure: ok"
