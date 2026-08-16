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
.github/workflows/apple-release.yml
scripts/verify-apple-silicon-bundle.sh
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

test "$(grep -c 'ARCHS: arm64' apps/apple/Project.yml)" -eq 2
test "$(grep -c 'EXCLUDED_ARCHS: x86_64' apps/apple/Project.yml)" -eq 2
grep -q 'targets: aarch64-apple-darwin$' .github/workflows/ci.yml
grep -q 'targets: aarch64-apple-darwin$' .github/workflows/apple-release.yml

if rg -n 'x86_64-apple-darwin|arm64/x86_64|Apple Silicon and Intel|universal (helper|Release archive|app-owned)' \
  apps/apple \
  .github/workflows/ci.yml \
  .github/workflows/apple-release.yml \
  docs/platform/capabilities.md \
  docs/product/traceability.md \
  docs/release/validation-matrix.md; then
  echo "obsolete multi-architecture macOS requirement found" >&2
  exit 1
fi

if ARCHS=x86_64 apps/apple/Scripts/build-node-helper.sh >/dev/null 2>&1; then
  echo "macOS helper build accepted x86_64" >&2
  exit 1
fi
if ARCHS='arm64 x86_64' apps/apple/Scripts/build-node-helper.sh >/dev/null 2>&1; then
  echo "macOS helper build accepted multiple architectures" >&2
  exit 1
fi

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
