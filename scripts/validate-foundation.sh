#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

# ripgrep drives two safety gates below (obsolete multi-architecture macOS
# references, and unsafe product claims outside documentation). Both were
# previously written as `if rg ...; then fail; fi`, which treats a missing
# binary or an rg error as "no match found" and silently passes the gate.
# A safety gate that cannot run must fail closed, so require rg up front.
if ! command -v rg >/dev/null 2>&1; then
  echo "validate-foundation.sh requires ripgrep (rg), which was not found on PATH" >&2
  echo "install it (brew install ripgrep | apt-get install ripgrep) and re-run" >&2
  exit 1
fi

# Run one rg scan as a fail-closed gate.
#   $1 = message printed when the forbidden pattern matches
#   remaining args = rg arguments
# rg exits 0 on match, 1 on no match, and >=2 on error. Only status 1 passes.
scan_must_not_match() {
  scan_message=$1
  shift
  scan_status=0
  rg -n "$@" || scan_status=$?
  if [ "$scan_status" -eq 0 ]; then
    echo "$scan_message" >&2
    exit 1
  fi
  if [ "$scan_status" -ne 1 ]; then
    echo "ripgrep exited $scan_status while scanning for: $scan_message" >&2
    exit 1
  fi
}

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
docs/release/signed-history-policy.md
docs/release/cli-install.md
docs/getting-started.md
docs/troubleshooting.md
docs/platform/macos.md
docs/platform/android.md
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
.github/workflows/apple-unsigned-release.yml
.github/workflows/cli-release.yml
scripts/verify-apple-silicon-bundle.sh
scripts/verify-release-commit-signature.sh
scripts/package-cli-release.sh
scripts/generate-cli-release-inventory.sh
scripts/setup-doctor.sh
scripts/validate-setup-paths.sh
scripts/build-personal-macos-app.sh
scripts/build-personal-android-apk.sh
scripts/test-personal-macos-app-builder.sh
scripts/test-personal-android-apk-builder.sh
scripts/test-openapi-routes.sh
scripts/test-setup-guidance.mjs
scripts/test-setup-paths.sh
"

for path in $required_paths; do
  if [ ! -e "$path" ]; then
    echo "missing foundation path: $path" >&2
    exit 1
  fi
done

for path in README.md docs/product/requirements.md docs/product/roadmap.md docs/architecture/overview.md docs/release/validation-matrix.md; do
  grep -q "Tier 1" "$path"
  grep -Eqi 'iOS (and Windows are|is) not (a )?supported|iOS native \| Not a blocker — unsupported platform' "$path"
  grep -Eqi 'informational|not a required check|not gated|not gated on' "$path"
done

# iOS source and CI may remain for shared-code diagnostics, but current product
# documentation must not revive the withdrawn supported-Tier-2 policy. ADR 0005
# is deliberately excluded because it preserves that wording as historical
# context for the policy change.
scan_must_not_match "current documentation describes iOS as a supported Tier 2 platform" \
  -i --glob '*.md' --glob '!docs/adr/0005-tiered-platform-readiness.md' \
  --glob '!docs/release/notes/**' \
  'iOS (remains|is|as an?|independently) supported( as)? Tier 2|independently supported Tier 2 selected-folder client|Tier 2 supported iOS' \
  README.md CONTRIBUTING.md working.md apps docs

grep -q 'Mode="ro"' packaging/unraid/covalent.xml
grep -q 'Target="/boot-source"' packaging/unraid/covalent.xml
grep -q 'Target="/restore"' packaging/unraid/covalent.xml
grep -q 'Default="false"' packaging/unraid/covalent.xml

test "$(grep -c 'ARCHS: arm64' apps/apple/Project.yml)" -eq 2
test "$(grep -c 'EXCLUDED_ARCHS: x86_64' apps/apple/Project.yml)" -eq 2
grep -q 'targets: aarch64-apple-darwin$' .github/workflows/ci.yml
grep -q 'targets: aarch64-apple-darwin$' .github/workflows/apple-unsigned-release.yml

scan_must_not_match "obsolete multi-architecture macOS requirement found" \
  'x86_64-apple-darwin|arm64/x86_64|Apple Silicon and Intel|universal (helper|Release archive|app-owned)' \
  apps/apple \
  .github/workflows/ci.yml \
  .github/workflows/apple-unsigned-release.yml \
  docs/platform/capabilities.md \
  docs/product/traceability.md \
  docs/release/validation-matrix.md

if ARCHS=x86_64 apps/apple/Scripts/build-node-helper.sh >/dev/null 2>&1; then
  echo "macOS helper build accepted x86_64" >&2
  exit 1
fi
if ARCHS='arm64 x86_64' apps/apple/Scripts/build-node-helper.sh >/dev/null 2>&1; then
  echo "macOS helper build accepted multiple architectures" >&2
  exit 1
fi

./scripts/validate-unraid-template.sh
./scripts/test-release-guardrails.sh
./scripts/check-container-contract.sh
node ./scripts/test-setup-guidance.mjs
./scripts/test-setup-paths.sh
./scripts/test-personal-macos-app-builder.sh
./scripts/test-personal-android-apk-builder.sh
./scripts/test-openapi-routes.sh

# The contract fixtures are a gate, not a nicety: `if command -v jq` silently
# skipped every one of these when jq was absent, so a malformed fixture passed
# validation on any machine without jq. Require jq the same way ripgrep is
# required above so the gate fails closed instead of vanishing.
if ! command -v jq >/dev/null 2>&1; then
  echo "validate-foundation.sh requires jq, which was not found on PATH" >&2
  echo "install it (brew install jq | apt-get install jq) and re-run" >&2
  exit 1
fi
jq empty fixtures/contracts/settings-v1.json
jq empty fixtures/contracts/pairing-invitation-v1.json
jq empty fixtures/contracts/manifest-v1.json

for script in scripts/*.sh; do
  shell_declaration=$(sed -n '1p' "$script")
  case "$shell_declaration" in
    '#!/usr/bin/env bash'|'#!/bin/bash')
      bash -n "$script"
      ;;
    '#!/bin/sh'|'# shellcheck shell=sh')
      sh -n "$script"
      ;;
    *)
      echo "unsupported shell declaration in $script: $shell_declaration" >&2
      exit 1
      ;;
  esac
done

./scripts/test-zero-open-codeql-alerts.sh

scan_must_not_match "unsafe product claim found outside explicit documentation" \
  --glob '!docs/**' --glob '!README.md' --glob '!working.md' --glob '!CONTRIBUTING.md' \
  --glob '!scripts/validate-foundation.sh' --glob '!target/**' \
  '(automatically (choose|select|place) replica|supports full-device iOS backup)' .

echo "foundation structure: ok"
