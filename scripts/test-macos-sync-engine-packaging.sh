#!/bin/sh
# Fast, offline source contract for the macOS sync-engine bundle builder.
# Set COVALENT_SYNCTHING_ARCHIVE to run the complete package operation too.
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
package_script="$repo_root/apps/apple/Scripts/package-sync-engine.sh"
sign_script="$repo_root/apps/apple/Scripts/sign-sync-engine-bundle.sh"
guardian_source="$repo_root/packaging/sync-engine/engine-guardian.c"
provenance="$repo_root/apps/apple/SyncEngine/PROVENANCE.txt"

fail() {
  printf '%s\n' "$1" >&2
  exit 1
}

for script in "$package_script" "$sign_script"; do
  test -x "$script" || fail "sync-engine packaging script is not executable"
  sh -n "$script"
done

test "$(shasum -a 256 "$guardian_source" | awk '{print $1}')" = \
  c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579 ||
  fail "reviewed engine guardian source changed"

for contract in \
  v2.1.3 \
  946e2b83a1f6c6ae119427c09e0a5802940b82ff \
  e0f0d8df05bf0118c48c6515214a96bf3a3f11dbd115f56c3c0b52251b3f71aa \
  6743a0efbf9d39784c7fe2e925505cd0a839458346444aa28620341e47ba12b8 \
  c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579 \
  Syncthing-LICENSE.txt \
  Syncthing-AUTHORS.txt \
  'xcrun clang' \
  '-arch arm64' \
  '-mmacosx-version-min=15.0' \
  'sign-sync-engine-bundle.sh'
do
  grep -Fq -- "$contract" "$package_script" "$sign_script" ||
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

grep -Fq 'Mozilla Public License 2.0' "$provenance" ||
  fail "sync-engine provenance must state the upstream license"
grep -Fq 'Corresponding source:' "$provenance" ||
  fail "sync-engine provenance must retain a corresponding-source location"

if rg -n 'global|local discovery|relay|upgrad|browser|telemetry|crash' \
  "$package_script" "$sign_script" >/dev/null 2>&1; then
  fail "packaging must not add an engine network or updater launch path"
fi

if [ -n "${COVALENT_SYNCTHING_ARCHIVE:-}" ]; then
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
  COVALENT_SYNCTHING_ARCHIVE=$COVALENT_SYNCTHING_ARCHIVE \
    DERIVED_FILE_DIR="$test_root/derived" \
    "$package_script" "$test_root/MacOS" "$test_root/Resources/CovalentSyncEngine"
  for binary in "$test_root/MacOS/covalent-engine-guardian" \
    "$test_root/MacOS/covalent-syncthing"; do
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
PY

  COVALENT_SYNCTHING_ARCHIVE=$COVALENT_SYNCTHING_ARCHIVE \
    DERIVED_FILE_DIR="$test_root/repeat-derived" \
    "$package_script" "$test_root/repeat/MacOS" \
      "$test_root/repeat/Resources/CovalentSyncEngine" >/dev/null
  cmp "$test_root/MacOS/covalent-engine-guardian" \
    "$test_root/repeat/MacOS/covalent-engine-guardian"
  cmp "$test_root/MacOS/covalent-syncthing" \
    "$test_root/repeat/MacOS/covalent-syncthing"
  cmp "$test_root/Resources/CovalentSyncEngine/manifest.json" \
    "$test_root/repeat/Resources/CovalentSyncEngine/manifest.json"

  mkdir -p "$test_root/unsafe/MacOS" "$test_root/unsafe/Resources/CovalentSyncEngine"
  printf '%s' preserved > "$test_root/sentinel"
  ln -s "$test_root/sentinel" "$test_root/unsafe/MacOS/covalent-syncthing"
  if COVALENT_SYNCTHING_ARCHIVE=$COVALENT_SYNCTHING_ARCHIVE \
    DERIVED_FILE_DIR="$test_root/unsafe-derived" \
    "$package_script" "$test_root/unsafe/MacOS" \
      "$test_root/unsafe/Resources/CovalentSyncEngine" >/dev/null 2>&1; then
    fail "packager accepted a symlinked executable output"
  fi
  test "$(cat "$test_root/sentinel")" = preserved ||
    fail "packager changed a symlink target"
fi

echo "macOS sync-engine packaging contract: ok"
