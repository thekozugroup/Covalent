#!/bin/sh
# Fast, offline source contract for the macOS sync-engine bundle builder.
# Set COVALENT_GO_BIN to run the complete operation.
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
package_script="$repo_root/apps/apple/Scripts/package-sync-engine.sh"
sign_script="$repo_root/apps/apple/Scripts/sign-sync-engine-bundle.sh"
build_script="$repo_root/scripts/build-macos-sync-engine.sh"
mac_host="$repo_root/crates/covalent-node/src/sync_engine/mac_host.rs"
bundle_verifier="$repo_root/scripts/verify-apple-silicon-bundle.sh"
guardian_source="$repo_root/packaging/sync-engine/engine-guardian.c"
provenance="$repo_root/apps/apple/SyncEngine/PROVENANCE.txt"

fail() {
  printf '%s\n' "$1" >&2
  exit 1
}

require_pin_count() {
  actual=$(grep -Foc -- "$1" "$2" || true)
  test "$actual" -eq "$3" || fail "sync-engine pin consumers disagree: $2"
}

for script in "$package_script" "$sign_script" "$build_script"; do
  test -x "$script" || fail "sync-engine packaging script is not executable"
  sh -n "$script"
done

test "$(shasum -a 256 "$guardian_source" | awk '{print $1}')" = \
  c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579 ||
  fail "reviewed engine guardian source changed"

for contract in \
  v1.75.1 \
  687d264b689b8c49a67e2e52a8a5e0caa01c04ce \
  d606a368fe4b83b81080aa9913d9f24b382c601e995d25f63ab80a49e2c8dee9 \
  c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579 \
  rclone-LICENSE.txt \
  covalent-rclone \
  THIRD-PARTY-NOTICES.txt \
  source-build.json \
  correspondingSources \
  'mod verify' \
  'CGO_ENABLED=0' \
  'sign-sync-engine-bundle.sh'
do
  grep -Fq -- "$contract" "$package_script" "$sign_script" "$build_script" ||
    fail "missing package contract: $contract"
done

for contract in \
  'com.apple.security.inherit' \
  'signedSha256' \
  'json.dumps(value, sort_keys=True' \
  '--timestamp=none' \
  '--options runtime'
do
  grep -Fq -- "$contract" "$sign_script" "$repo_root/apps/apple/Config/CovalentNode.entitlements" ||
    fail "missing signing contract: $contract"
done

worker_pin=$(sed -nE 's/.*"unsignedExecutableSha256": "([0-9a-f]{64})".*/\1/p' \
  "$sign_script")
printf '%s\n' "$worker_pin" | grep -Eq '^[0-9a-f]{64}$' ||
  fail "signer must define one unsigned sync-engine pin"
require_pin_count "$worker_pin" "$package_script" 1
require_pin_count "$worker_pin" "$sign_script" 1
require_pin_count "$worker_pin" "$mac_host" 1
require_pin_count "$worker_pin" "$bundle_verifier" 1

notice_pins=$(sed -nE 's/^verify_notice .* ([0-9a-f]{64})$/\1/p' "$sign_script")
test "$(printf '%s\n' "$notice_pins" | wc -l | tr -d '[:space:]')" -eq 6 &&
  test "$(printf '%s\n' "$notice_pins" | LC_ALL=C sort -u | wc -l | tr -d '[:space:]')" -eq 6 ||
  fail "signer notice pins are incomplete"
for pin in $notice_pins; do
  require_pin_count "$pin" "$sign_script" 2
  require_pin_count "$pin" "$mac_host" 2
  require_pin_count "$pin" "$bundle_verifier" 1
done

grep -Fq 'License: MIT' "$provenance" ||
  fail "sync-engine provenance must state the upstream license"
grep -Fq 'Upstream source:' "$provenance" || fail "sync-engine provenance must retain the upstream source location"

if rg -n 'global|local discovery|relay|upgrad|browser|telemetry|crash' \
  "$package_script" "$sign_script" >/dev/null 2>&1; then
  fail "packaging must not add an engine network or updater launch path"
fi

if [ -n "${COVALENT_GO_BIN:-}" ]; then
  if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
    fail "the complete sync-engine package test requires Apple Silicon macOS"
  fi
  test_root=$(mktemp -d "${TMPDIR:-/tmp}/covalent-sync-engine-test.XXXXXX")
  cleanup() {
    python3 - "$test_root" <<'PY'
import shutil
import sys
shutil.rmtree(sys.argv[1], ignore_errors=True)
PY
  }
  trap cleanup EXIT HUP INT TERM

  # The output directory is intentionally retained for diagnostics, but no
  # extracted toolchain, module cache, or source export may survive even an
  # error that occurs before the Go executable can be validated.
  printf 'not the pinned Go archive\n' > "$test_root/invalid-go.tar.gz"
  if COVALENT_GO_ARCHIVE="$test_root/invalid-go.tar.gz" \
    "$build_script" "$repo_root/packaging/rclone" "$test_root/early-failure" \
      >"$test_root/early-failure.stdout" 2>"$test_root/early-failure.stderr"; then
    fail "source builder accepted an invalid Go archive"
  fi
  test -d "$test_root/early-failure" ||
    fail "source builder did not create its owned diagnostic output"
  test ! -e "$test_root/early-failure/.private-build" &&
    test ! -L "$test_root/early-failure/.private-build" ||
    fail "source builder retained private state after an early failure"
  test -z "$(find "$test_root/early-failure" -mindepth 1 -print -quit)" ||
    fail "source builder retained unexpected output after an early failure"

  COVALENT_GO_BIN=$COVALENT_GO_BIN \
    DERIVED_FILE_DIR="$test_root/derived" \
    "$package_script" "$test_root/MacOS" "$test_root/Resources/CovalentSyncEngine"
  for binary in "$test_root/MacOS/covalent-engine-guardian" \
    "$test_root/MacOS/covalent-rclone"; do
    test "$(xcrun lipo -archs "$binary")" = arm64 || fail "packaged binary is not arm64"
    codesign --verify --strict "$binary"
  done
  python3 - "$test_root" <<'PY'
import hashlib
import json
import pathlib
import sys
root = pathlib.Path(sys.argv[1])
manifest = json.loads((root / "Resources/CovalentSyncEngine/manifest.json").read_bytes())
for name, record in manifest["executables"].items():
    digest = hashlib.sha256((root / "MacOS" / name).read_bytes()).hexdigest()
    if digest != record["signedSha256"]:
        raise SystemExit("post-sign manifest digest mismatch")
notice_root = root / "Resources/CovalentSyncEngine/notices"
notice_manifest = json.loads((notice_root / "manifest.json").read_bytes())
sources = notice_manifest.get("correspondingSources")
if not isinstance(sources, list) or len(sources) > 2_048:
    raise SystemExit("packaged corresponding-source inventory differs")
combined = (notice_root / "THIRD-PARTY-NOTICES.txt").read_text()
for source in sources:
    archive = source["archive"]
    path = notice_root / archive["bundlePath"]
    if hashlib.sha256(path.read_bytes()).hexdigest() != archive["sha256"]:
        raise SystemExit("packaged corresponding-source archive differs")
    if archive["bundlePath"] not in combined or archive["sha256"] not in combined:
        raise SystemExit("corresponding-source access is not recipient-visible")
PY

  COVALENT_GO_BIN=$COVALENT_GO_BIN \
    DERIVED_FILE_DIR="$test_root/repeat-derived" \
    "$package_script" "$test_root/repeat/MacOS" \
      "$test_root/repeat/Resources/CovalentSyncEngine" >/dev/null
  cmp "$test_root/MacOS/covalent-engine-guardian" \
    "$test_root/repeat/MacOS/covalent-engine-guardian"
  cmp "$test_root/MacOS/covalent-rclone" \
    "$test_root/repeat/MacOS/covalent-rclone"
  cmp "$test_root/Resources/CovalentSyncEngine/manifest.json" \
    "$test_root/repeat/Resources/CovalentSyncEngine/manifest.json"
  cmp "$test_root/Resources/CovalentSyncEngine/notices/manifest.json" \
    "$test_root/repeat/Resources/CovalentSyncEngine/notices/manifest.json"
  if test -d "$test_root/Resources/CovalentSyncEngine/notices/sources"; then
    for archive in "$test_root/Resources/CovalentSyncEngine/notices/sources/"*.tar.gz; do
      cmp "$archive" "$test_root/repeat/Resources/CovalentSyncEngine/notices/sources/$(basename "$archive")"
    done
  fi

  mkdir -p "$test_root/unsafe/MacOS" "$test_root/unsafe/Resources/CovalentSyncEngine"
  printf '%s' preserved > "$test_root/sentinel"
  ln -s "$test_root/sentinel" "$test_root/unsafe/MacOS/covalent-rclone"
  if COVALENT_GO_BIN=$COVALENT_GO_BIN \
    DERIVED_FILE_DIR="$test_root/unsafe-derived" \
    "$package_script" "$test_root/unsafe/MacOS" \
      "$test_root/unsafe/Resources/CovalentSyncEngine" >/dev/null 2>&1; then
    fail "packager accepted a symlinked executable output"
  fi
  test "$(cat "$test_root/sentinel")" = preserved ||
    fail "packager changed a symlink target"
fi

echo "macOS sync-engine packaging contract: ok"
